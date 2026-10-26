// SPDX-License-Identifier: GPL-3.0-or-later
//! Connection lifecycle for a single phone: TLS accept, `Hello`, pairing gate, session loop.
//!
//! See the "pairing (v2)" section of `control.proto` and ADR 0004 for the message flow.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use nd_proto::framing::{read_frame, write_frame};
use nd_proto::v1::{
    CameraStatus, Capability, DeviceHealth, Goodbye, GoodbyeReason, Hello, InputEvent, MediaKind, NeedIcon, PairResult,
    PairingPending, Ping, Pong, Welcome,
};
use nd_proto::{Envelope, Msg, MAX_MISSED_PINGS, PING_INTERVAL_MS, PROTO_VERSION};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

use crate::error::{Result, TransportError};
use crate::handle::SessionHandle;
use crate::identity::HostIdentity;
use crate::pairing::{Fingerprint, PAIR_REQUEST_TIMEOUT_SECS, USB_APPROVAL_TIMEOUT_SECS};
use crate::server::{PendingCmd, Shared};
use crate::trust::{Link, PeerInfo, PendingPeer, TrustState, TrustStore};

/// Time the phone has to send its first frame (TLS ClientHello or a v1 `Hello`).
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed for the TLS handshake.
const TLS_TIMEOUT: Duration = Duration::from_secs(10);
/// Every write to the phone must finish within this.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// After a Goodbye, how long we wait for the phone to close (avoids an RST eating the Goodbye).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// First byte of a TLS handshake record.
const TLS_HANDSHAKE_BYTE: u8 = 0x16;

/// Events surfaced from active sessions.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    SessionConnected(SessionHandle),
    SessionDisconnected(String),
    Input(InputEvent),
    Health(DeviceHealth),
    NeedIcon(NeedIcon),
    CameraStatus(CameraStatus),
    /// Same as [`TransportEvent::CameraStatus`] but says which session sent it.
    CameraStatusFrom {
        session_id: String,
        status: CameraStatus,
    },
    /// Round 25: a phone reported its microphone state.
    MicStatusFrom {
        session_id: String,
        status: nd_proto::v1::MicStatus,
    },
    Other(Envelope),
    /// An unknown phone connected over USB and waits for Allow/Deny
    /// (see `ServerHandle::resolve_pending`).
    PairingPending(PendingPeer),
    /// A pending entry is gone (allowed, denied, timed out or disconnected). Carries the
    /// pending session id.
    PendingResolved(String),
}

/// Everything a connection task needs from the server.
pub(crate) struct ServerCtx {
    pub host_name: String,
    pub identity: Arc<HostIdentity>,
    pub trust: Arc<dyn TrustStore>,
    pub acceptor: TlsAcceptor,
    pub input_tx: mpsc::Sender<InputEvent>,
    pub shared: Arc<Shared>,
    pub classify: fn(&SocketAddr) -> Link,
    pub media_tx: mpsc::Sender<crate::media::MediaStream>,
    /// Round 25: accepted audio (phone mic) media connections.
    pub audio_tx: mpsc::Sender<crate::media::MediaStream>,
}

enum Inbound {
    Frame(Envelope),
    Closed,
    Failed(String),
}

/// Split TLS stream. Reading happens in a dedicated task so that `recv()` is cancel-safe
/// inside `select!` (`read_frame` itself is not).
struct Conn {
    rx: mpsc::Receiver<Inbound>,
    wr: WriteHalf<TlsStream<TcpStream>>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Conn {
    fn new(stream: TlsStream<TcpStream>) -> Self {
        let (rd, wr) = tokio::io::split(stream);
        let (tx, rx) = mpsc::channel(64);
        let reader = tokio::spawn(read_loop(rd, tx));
        Self { rx, wr, reader }
    }

    async fn send_env(&mut self, env: &Envelope) -> Result<()> {
        match timeout(WRITE_TIMEOUT, write_frame(&mut self.wr, env)).await {
            Ok(r) => Ok(r?),
            Err(_) => Err(TransportError::Send("write timed out".into())),
        }
    }

    async fn send(&mut self, msg: Msg) -> Result<()> {
        self.send_env(&Envelope::new(0, msg)).await
    }

    async fn shutdown(&mut self) {
        let _ = timeout(Duration::from_secs(2), self.wr.shutdown()).await;
    }

    /// Send a Goodbye, flush it, close our side and give the phone a moment to close its own.
    async fn goodbye(&mut self, reason: GoodbyeReason, message: impl Into<String>) {
        let msg = Msg::Goodbye(Goodbye { reason: reason as i32, message: message.into() });
        let _ = self.send(msg).await;
        self.shutdown().await;
        let _ = timeout(DRAIN_TIMEOUT, async {
            while let Some(m) = self.rx.recv().await {
                if !matches!(m, Inbound::Frame(_)) {
                    break;
                }
            }
        })
        .await;
    }
}

async fn read_loop(mut rd: ReadHalf<TlsStream<TcpStream>>, tx: mpsc::Sender<Inbound>) {
    loop {
        let item = match read_frame(&mut rd).await {
            Ok(Some(env)) => Inbound::Frame(env),
            Ok(None) => Inbound::Closed,
            Err(e) => Inbound::Failed(e.to_string()),
        };
        let last = !matches!(item, Inbound::Frame(_));
        if tx.send(item).await.is_err() || last {
            return;
        }
    }
}

/// Entry point for every accepted TCP connection.
pub(crate) async fn handle_connection(stream: TcpStream, remote: SocketAddr, ctx: Arc<ServerCtx>) -> Result<()> {
    let _ = stream.set_nodelay(true);

    // Tell TLS from a v1 plaintext phone without consuming any bytes.
    let mut first = [0u8; 1];
    let n = match timeout(HELLO_TIMEOUT, stream.peek(&mut first)).await {
        Ok(r) => r?,
        Err(_) => return Err(TransportError::Handshake("no data from client".into())),
    };
    if n == 0 {
        return Ok(());
    }
    if first[0] != TLS_HANDSHAKE_BYTE {
        return reject_plaintext(stream, remote).await;
    }

    let tls = match timeout(TLS_TIMEOUT, ctx.acceptor.accept(stream)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return Err(TransportError::Tls(format!("handshake failed: {e}"))),
        Err(_) => return Err(TransportError::Tls("handshake timed out".into())),
    };
    let fp = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| Fingerprint::of_der(c))
        .ok_or_else(|| TransportError::Tls("client sent no certificate".into()))?;

    // The first frame decides what this connection is: a control session (`Hello`) or the
    // media connection of an existing one (`MediaAttach`, ADR 0005). Read it straight from the
    // stream (no reader task yet) so a media stream keeps its raw TLS stream.
    let mut tls = tls;
    let first = match read_first(&mut tls).await? {
        Some(f) => f,
        None => return Ok(()),
    };
    match first {
        First::Attach(session_id, kind) => crate::media::handle_attach(&ctx, tls, fp, remote, session_id, kind).await,
        First::Hello(hello) => {
            let mut conn = Conn::new(tls);
            let outcome = run_connection(&ctx, &mut conn, hello, fp, remote).await;
            if outcome.is_err() {
                conn.shutdown().await;
            }
            outcome
        }
    }
}

enum First {
    Hello(Hello),
    Attach(String, MediaKind),
}

/// Wait for `Hello` or `MediaAttach`, answering pings. `Ok(None)`: the phone went away.
async fn read_first(tls: &mut TlsStream<TcpStream>) -> Result<Option<First>> {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    loop {
        let env = match tokio::time::timeout_at(deadline, read_frame(tls)).await {
            Err(_) => return Err(TransportError::Handshake("Hello timed out".into())),
            Ok(Ok(None)) => return Ok(None),
            Ok(Err(e)) => return Err(TransportError::Handshake(format!("frame read error: {e}"))),
            Ok(Ok(Some(env))) => env,
        };
        match env.msg {
            Some(Msg::Hello(h)) => return Ok(Some(First::Hello(h))),
            Some(Msg::MediaAttach(a)) => {
                // Unknown kinds (newer phones) read as video, which the host then refuses or ignores.
                let kind = MediaKind::try_from(a.kind).unwrap_or(MediaKind::Video);
                return Ok(Some(First::Attach(a.session_id, kind)));
            }
            Some(Msg::Ping(p)) => {
                let pong = Envelope::new(0, Msg::Pong(Pong { nonce: p.nonce, host_time_ms: crate::unix_ms() }));
                match timeout(WRITE_TIMEOUT, write_frame(tls, &pong)).await {
                    Ok(r) => r.map_err(|e| TransportError::Send(e.to_string()))?,
                    Err(_) => return Err(TransportError::Send("write timed out".into())),
                }
            }
            Some(Msg::Goodbye(_)) => return Ok(None),
            _ => {}
        }
    }
}

/// A v1 phone speaks plaintext: read its Hello and tell it, in its own framing, to update.
async fn reject_plaintext(mut stream: TcpStream, remote: SocketAddr) -> Result<()> {
    match timeout(HELLO_TIMEOUT, read_frame(&mut stream)).await {
        Ok(Ok(Some(_))) => {}
        _ => return Ok(()),
    }
    tracing::info!(%remote, "Refusing plaintext (v1) phone");
    let env = Envelope::new(
        0,
        Msg::Goodbye(Goodbye {
            reason: GoodbyeReason::VersionMismatch as i32,
            message: "This PC runs a newer Notideck. Update the Notideck app on your phone.".into(),
        }),
    );
    let _ = timeout(WRITE_TIMEOUT, write_frame(&mut stream, &env)).await;
    let _ = timeout(Duration::from_secs(2), stream.shutdown()).await;
    // Let the phone read the Goodbye before the socket closes.
    let _ = timeout(DRAIN_TIMEOUT, async {
        let mut buf = [0u8; 256];
        loop {
            match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    Ok(())
}

async fn run_connection(
    ctx: &ServerCtx,
    conn: &mut Conn,
    hello: Hello,
    fp: Fingerprint,
    remote: SocketAddr,
) -> Result<()> {
    // 1. Hello (already read)
    if hello.proto_version != PROTO_VERSION {
        conn.goodbye(
            GoodbyeReason::VersionMismatch,
            format!(
                "Unsupported protocol version {} (this PC speaks {}). Update Notideck on the older side.",
                hello.proto_version, PROTO_VERSION
            ),
        )
        .await;
        return Ok(());
    }

    let link = (ctx.classify)(&remote);
    let device_name =
        if hello.device_name.trim().is_empty() { hello.device_model.clone() } else { hello.device_name.clone() };
    let mut peer = PeerInfo {
        fp,
        device_id: hello.device_id.clone(),
        device_name,
        device_model: hello.device_model.clone(),
        addr: remote,
        link,
    };
    if !hello.cert_fp.is_empty() && Fingerprint::from_hex(&hello.cert_fp) != Some(fp) {
        tracing::debug!(%remote, "Hello.cert_fp differs from the TLS certificate (ignored)");
    }

    // 2. Trust gate
    let session_id = uuid::Uuid::new_v4().to_string();
    if !ctx.trust.is_paired(&fp) {
        tracing::info!(%remote, fp = %fp.short(), ?link, "Unknown phone; starting pairing");
        if !await_pairing(ctx, conn, &mut peer, &session_id).await? {
            return Ok(());
        }
    }

    // 3. Session
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<Envelope>(64);
    let capabilities = hello.capabilities.iter().filter_map(|c| Capability::try_from(*c).ok()).collect();
    let session = SessionHandle::new(
        session_id.clone(),
        hello.device_id.clone(),
        hello.device_model.clone(),
        hello.app_version.clone(),
        hello.android_sdk,
        hello.screen,
        capabilities,
        outgoing_tx,
        peer.clone(),
        TrustState::Paired,
    );
    // Register first so the phone can be addressed the moment it sees Welcome. Anything queued
    // before the loop starts is sent right after Welcome.
    ctx.shared.register_session(session.clone());
    let welcome = Welcome {
        session_id: session_id.clone(),
        host_name: ctx.host_name.clone(),
        proto_version: PROTO_VERSION,
        host_id: ctx.identity.host_id().to_string(),
    };
    if let Err(e) = conn.send(Msg::Welcome(welcome)).await {
        session.mark_closed();
        ctx.shared.remove_session(&session_id, &fp);
        return Err(e);
    }
    tracing::info!(
        session_id = %session_id,
        device_id = %peer.device_id,
        model = %peer.device_model,
        link = peer.link.as_str(),
        "Handshake completed; session established"
    );
    ctx.trust.touch(&peer, true);
    let _ = ctx.shared.event_tx.send(TransportEvent::SessionConnected(session.clone()));

    session_loop(ctx, conn, &session, outgoing_rx).await;

    session.mark_closed();
    let still_connected = ctx.shared.remove_session(&session_id, &fp);
    if !still_connected {
        // Only report the device as gone if a newer session did not take over.
        ctx.trust.touch(&peer, false);
    }
    let _ = ctx.shared.event_tx.send(TransportEvent::SessionDisconnected(session_id));
    Ok(())
}

/// Run the pairing gate for an unknown phone. `Ok(true)`: now trusted, send Welcome.
/// `Ok(false)`: the connection was ended (Goodbye already sent).
async fn await_pairing(ctx: &ServerCtx, conn: &mut Conn, peer: &mut PeerInfo, session_id: &str) -> Result<bool> {
    let usb = peer.link == Link::Usb;
    let (mut cmd_rx, wait) = if usb {
        let (tx, rx) = mpsc::channel(4);
        ctx.shared.add_pending(session_id, peer.clone(), tx);
        let _ = ctx.shared.event_tx.send(TransportEvent::PairingPending(PendingPeer {
            session_id: session_id.to_string(),
            peer: peer.clone(),
        }));
        (Some(rx), USB_APPROVAL_TIMEOUT_SECS)
    } else {
        (None, PAIR_REQUEST_TIMEOUT_SECS)
    };

    let res = pairing_loop(ctx, conn, peer, usb, &mut cmd_rx, Duration::from_secs(wait)).await;

    if usb {
        ctx.shared.drop_pending(session_id);
        let _ = ctx.shared.event_tx.send(TransportEvent::PendingResolved(session_id.to_string()));
    }
    res
}

async fn pairing_loop(
    ctx: &ServerCtx,
    conn: &mut Conn,
    peer: &mut PeerInfo,
    usb: bool,
    cmd_rx: &mut Option<mpsc::Receiver<PendingCmd>>,
    wait: Duration,
) -> Result<bool> {
    if usb {
        conn.send(Msg::PairingPending(PairingPending { host_name: ctx.host_name.clone() })).await?;
    }
    let deadline = Instant::now() + wait;
    loop {
        let cmd = async {
            match cmd_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            cmd = cmd => match cmd {
                Some(PendingCmd::Allow) => {
                    ctx.trust.pin(peer);
                    tracing::info!(fp = %peer.fp.short(), "USB phone allowed");
                    return Ok(true);
                }
                Some(PendingCmd::Deny) => {
                    conn.goodbye(GoodbyeReason::PairingDenied, "The PC refused this phone").await;
                    return Ok(false);
                }
                Some(PendingCmd::Revoke) => {
                    conn.goodbye(GoodbyeReason::Revoked, "This phone was removed on the PC").await;
                    return Ok(false);
                }
                // Entry dropped without a decision; keep waiting for PairRequest / timeout.
                None => *cmd_rx = None,
            },
            msg = conn.rx.recv() => match msg {
                None | Some(Inbound::Closed) => return Ok(false),
                Some(Inbound::Failed(e)) => {
                    tracing::debug!("frame error while pairing: {e}");
                    return Ok(false);
                }
                Some(Inbound::Frame(env)) => match env.msg {
                    Some(Msg::Ping(p)) => conn.send(Msg::Pong(Pong { nonce: p.nonce, host_time_ms: crate::unix_ms() })).await?,
                    Some(Msg::Goodbye(_)) => return Ok(false),
                    Some(Msg::PairRequest(req)) => {
                        if !req.device_name.trim().is_empty() {
                            peer.device_name = req.device_name.clone();
                        }
                        // Never log the proof.
                        match ctx.trust.redeem(peer, &req.proof) {
                            Ok(()) => {
                                tracing::info!(fp = %peer.fp.short(), "Phone paired with a QR code");
                                conn.send(Msg::PairResult(PairResult {
                                    ok: true,
                                    error: String::new(),
                                    host_id: ctx.identity.host_id().to_string(),
                                    host_name: ctx.host_name.clone(),
                                }))
                                .await?;
                                return Ok(true);
                            }
                            Err(reason) => {
                                tracing::warn!(fp = %peer.fp.short(), %reason, "Pairing request rejected");
                                conn.send(Msg::PairResult(PairResult {
                                    ok: false,
                                    error: reason,
                                    host_id: String::new(),
                                    host_name: String::new(),
                                }))
                                .await?;
                                if !usb {
                                    conn.goodbye(GoodbyeReason::Unauthorized, "Pairing failed").await;
                                    return Ok(false);
                                }
                            }
                        }
                    }
                    _ => {}
                },
            },
            _ = tokio::time::sleep_until(deadline) => {
                conn.goodbye(GoodbyeReason::Unpaired, "This phone is not paired with the PC").await;
                return Ok(false);
            }
        }
    }
}

/// The established session: heartbeats, input, outgoing queue, close requests.
async fn session_loop(
    ctx: &ServerCtx,
    conn: &mut Conn,
    session: &SessionHandle,
    mut outgoing_rx: mpsc::Receiver<Envelope>,
) {
    let session_id = session.session_id().to_string();
    let event_tx = &ctx.shared.event_tx;
    let mut ping_interval = tokio::time::interval(Duration::from_millis(PING_INTERVAL_MS));
    // First tick fires immediately; consume it so we don't ping at 0ms.
    ping_interval.tick().await;
    let mut missed_pings: u32 = 0;

    loop {
        tokio::select! {
            biased;

            _ = session.close_requested() => {
                if let Some(g) = session.take_goodbye() {
                    tracing::info!(session_id = %session_id, reason = g.reason, "Closing session");
                    let reason = GoodbyeReason::try_from(g.reason).unwrap_or(GoodbyeReason::Unspecified);
                    conn.goodbye(reason, g.message).await;
                } else {
                    conn.shutdown().await;
                }
                return;
            }

            Some(env) = outgoing_rx.recv() => {
                if let Err(e) = conn.send_env(&env).await {
                    tracing::debug!(session_id = %session_id, "Writer error: {e}");
                    return;
                }
            }

            _ = ping_interval.tick() => {
                missed_pings += 1;
                if missed_pings > MAX_MISSED_PINGS {
                    tracing::warn!(session_id = %session_id, missed_pings, "Heartbeat timeout");
                    return;
                }
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = session.send_msg(Msg::Ping(Ping { nonce })).await;
            }

            msg = conn.rx.recv() => {
                let env = match msg {
                    Some(Inbound::Frame(env)) => env,
                    None | Some(Inbound::Closed) => {
                        tracing::info!(session_id = %session_id, "Client disconnected");
                        return;
                    }
                    Some(Inbound::Failed(e)) => {
                        tracing::warn!(session_id = %session_id, "Frame read error: {e}");
                        return;
                    }
                };
                match env.msg {
                    Some(Msg::Pong(_)) => missed_pings = 0,
                    Some(Msg::Ping(ping)) => {
                        let _ = session.send_msg(Msg::Pong(Pong { nonce: ping.nonce, host_time_ms: crate::unix_ms() })).await;
                    }
                    Some(Msg::Input(input_ev)) => {
                        let _ = ctx.input_tx.send(input_ev.clone()).await;
                        let _ = event_tx.send(TransportEvent::Input(input_ev));
                    }
                    Some(Msg::Goodbye(goodbye)) => {
                        tracing::info!(
                            session_id = %session_id,
                            reason = goodbye.reason,
                            message = %goodbye.message,
                            "Client closed session with Goodbye"
                        );
                        return;
                    }
                    Some(Msg::Health(h)) => {
                        session.set_health(h);
                        let _ = event_tx.send(TransportEvent::Health(h));
                    }
                    Some(Msg::NeedIcon(ni)) => {
                        let _ = event_tx.send(TransportEvent::NeedIcon(ni));
                    }
                    Some(Msg::CameraReadout(r)) => session.set_camera_readout(r),
                    Some(Msg::MicStatus(ms)) => {
                        session.set_mic_status(ms.clone());
                        let _ = event_tx
                            .send(TransportEvent::MicStatusFrom { session_id: session_id.clone(), status: ms });
                    }
                    Some(Msg::CameraStatus(cs)) => {
                        session.set_camera_status(cs.clone());
                        let _ = event_tx.send(TransportEvent::CameraStatusFrom {
                            session_id: session_id.clone(),
                            status: cs.clone(),
                        });
                        let _ = event_tx.send(TransportEvent::CameraStatus(cs));
                    }
                    Some(other) => {
                        let _ = event_tx.send(TransportEvent::Other(Envelope::new(env.seq, other)));
                    }
                    None => {}
                }
            }
        }
    }
}
