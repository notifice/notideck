// SPDX-License-Identifier: GPL-3.0-or-later

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tokio::task::JoinHandle;

use nd_proto::framing;
use nd_proto::v1::*;
use nd_proto::{Envelope, Msg};
use nd_transport::pairing::compute_proof;
use nd_transport::tls::client_config;
use nd_transport::Fingerprint;
use rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

use crate::config::FakePhoneConfig;
use crate::error::FakePhoneError;
use crate::store::ClientIdentity;

/// Recorded state of the fake phone client.
#[derive(Debug, Default, Clone)]
pub struct PhoneState {
    /// Latest welcome message received from host.
    pub welcome: Option<Welcome>,
    /// Result of the last `PairRequest`.
    pub pair_result: Option<PairResult>,
    /// Set when the host answered `PairingPending` (waiting for Allow on the PC).
    pub pairing_pending: Option<PairingPending>,
    /// The Goodbye that ended (or is ending) the session, if any.
    pub goodbye: Option<Goodbye>,
    /// Latest deck layout received from host.
    pub latest_layout: Option<SetLayout>,
    /// Latest control patches received from host.
    pub latest_update_controls: Option<UpdateControls>,
    /// History of all `UpdateControls` messages received.
    pub update_controls_history: Vec<UpdateControls>,
    /// Latest feedback message received from host.
    pub latest_feedback: Option<Feedback>,
    /// History of all feedback messages received.
    pub feedback_history: Vec<Feedback>,
    /// Latest navigation-rail state received from host.
    pub latest_nav_state: Option<NavState>,
    /// Latest side-rail key set received from host.
    pub latest_rail: Option<SetRail>,
    /// History of all `PlayStream` messages received (empty url = close).
    pub play_stream_history: Vec<PlayStream>,
    /// All raw envelopes received from host.
    pub all_received: Vec<Envelope>,
    /// Total pings received from host.
    pub pings_received: u64,
    /// Total pongs sent back to host.
    pub pongs_sent: u64,
}

/// Headless fake phone client driving the TLS control channel.
pub struct FakePhone {
    config: FakePhoneConfig,
    outgoing_tx: mpsc::Sender<Envelope>,
    seq_counter: Arc<AtomicU32>,
    state: Arc<RwLock<PhoneState>>,
    events_tx: broadcast::Sender<Envelope>,
    closed_rx: watch::Receiver<bool>,
    host_fp: Fingerprint,
    own_fp: Fingerprint,
    remote_addr: SocketAddr,
    identity: Arc<ClientIdentity>,
    writer_handle: JoinHandle<()>,
    reader_handle: JoinHandle<()>,
}

fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn goodbye_error(g: &Goodbye) -> FakePhoneError {
    FakePhoneError::Goodbye {
        reason: GoodbyeReason::try_from(g.reason).unwrap_or(GoodbyeReason::Unspecified),
        message: g.message.clone(),
    }
}

fn hello_for(config: &FakePhoneConfig, cert_fp: &str) -> Hello {
    Hello {
        proto_version: config.proto_version,
        device_id: config.device_id.clone(),
        device_model: config.device_model.clone(),
        android_sdk: config.android_sdk,
        app_version: config.app_version.clone(),
        screen: Some(config.screen),
        capabilities: config.capabilities.iter().map(|c| *c as i32).collect(),
        cert_fp: cert_fp.to_string(),
        device_name: config.device_name.clone(),
    }
}

/// Send a v1 (plaintext, no TLS) `Hello` and return the host's `Goodbye`. A v2 host refuses such
/// phones with `VERSION_MISMATCH`; this exercises that path.
pub async fn plaintext_v1_probe(addr: SocketAddr, config: &FakePhoneConfig) -> Result<Goodbye, FakePhoneError> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut hello = hello_for(config, "");
    hello.proto_version = 1;
    framing::write_frame(&mut stream, &Envelope::new(1, Msg::Hello(hello))).await?;
    let deadline = tokio::time::Instant::now() + config.handshake_timeout;
    loop {
        match tokio::time::timeout_at(deadline, framing::read_frame(&mut stream)).await {
            Err(_) => return Err(FakePhoneError::Timeout("Goodbye")),
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok(None)) => return Err(FakePhoneError::ConnectionClosed),
            Ok(Ok(Some(Envelope { msg: Some(Msg::Goodbye(g)), .. }))) => return Ok(g),
            Ok(Ok(Some(_))) => continue,
        }
    }
}

impl FakePhone {
    /// Connect to the default host control endpoint (`127.0.0.1:47800`).
    pub async fn connect_default() -> Result<Self, FakePhoneError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], nd_proto::CONTROL_PORT));
        Self::connect(addr).await
    }

    /// Connect to a specified address with default configuration (throw-away identity, TOFU).
    pub async fn connect(addr: SocketAddr) -> Result<Self, FakePhoneError> {
        Self::connect_with_config(addr, FakePhoneConfig::default()).await
    }

    /// Connect to a specified address with custom configuration.
    pub async fn connect_with_config(addr: SocketAddr, config: FakePhoneConfig) -> Result<Self, FakePhoneError> {
        let stream = TcpStream::connect(addr).await?;
        Self::from_stream_with_config(stream, config).await
    }

    /// Try each address in turn (first TCP connect that succeeds wins), then handshake.
    pub async fn connect_any(addrs: &[SocketAddr], config: FakePhoneConfig) -> Result<Self, FakePhoneError> {
        let mut last = None;
        for addr in addrs {
            match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr)).await {
                Ok(Ok(stream)) => return Self::from_stream_with_config(stream, config).await,
                Ok(Err(e)) => last = Some(FakePhoneError::Io(e)),
                Err(_) => last = Some(FakePhoneError::Timeout("TCP connect")),
            }
        }
        Err(last.unwrap_or_else(|| FakePhoneError::Other("no address to connect to".into())))
    }

    /// TLS-handshake over an existing `TcpStream`, then run the Hello / pairing / Welcome flow.
    pub async fn from_stream_with_config(stream: TcpStream, config: FakePhoneConfig) -> Result<Self, FakePhoneError> {
        let identity = match &config.identity {
            Some(id) => id.clone(),
            None => Arc::new(ClientIdentity::generate("Notideck Fake Phone")?),
        };
        let own_fp = identity.fp;
        let tls_cfg = client_config(identity.cert_der.clone(), identity.key_der.clone(), config.pin)
            .map_err(|e| FakePhoneError::Tls(e.to_string()))?;
        let _ = stream.set_nodelay(true);
        let remote_addr = stream.peer_addr()?;
        let name = ServerName::try_from("notideck.local").expect("static name");
        let tls =
            tokio::time::timeout(config.handshake_timeout, TlsConnector::from(Arc::new(tls_cfg)).connect(name, stream))
                .await
                .map_err(|_| FakePhoneError::Timeout("TLS handshake"))?
                .map_err(|e| FakePhoneError::Tls(e.to_string()))?;
        let host_fp = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|c| c.first())
            .map(|c| Fingerprint::of_der(c))
            .ok_or_else(|| FakePhoneError::Tls("host sent no certificate".into()))?;

        let (mut read_half, mut write_half) = tokio::io::split(tls);
        let state = Arc::new(RwLock::new(PhoneState::default()));
        let (events_tx, _) = broadcast::channel(256);
        let (closed_tx, closed_rx) = watch::channel(false);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Envelope>(64);
        let seq_counter = Arc::new(AtomicU32::new(1));

        let writer_handle = tokio::spawn(async move {
            while let Some(env) = outgoing_rx.recv().await {
                if let Err(e) = framing::write_frame(&mut write_half, &env).await {
                    tracing::debug!("fakephone writer finished: {e}");
                    break;
                }
            }
        });

        let reader_handle = tokio::spawn({
            let state = Arc::clone(&state);
            let events_tx = events_tx.clone();
            let outgoing_tx = outgoing_tx.clone();
            let seq_counter = Arc::clone(&seq_counter);
            let auto_pong = config.auto_pong;

            async move {
                loop {
                    match framing::read_frame(&mut read_half).await {
                        Ok(Some(env)) => {
                            let mut is_ping = false;
                            let mut ping_nonce = 0u64;

                            {
                                let mut st = state.write().await;
                                st.all_received.push(env.clone());

                                if let Some(ref msg) = env.msg {
                                    match msg {
                                        Msg::Welcome(w) => {
                                            st.welcome = Some(w.clone());
                                        }
                                        Msg::PairResult(r) => {
                                            st.pair_result = Some(r.clone());
                                        }
                                        Msg::PairingPending(p) => {
                                            st.pairing_pending = Some(p.clone());
                                        }
                                        Msg::Goodbye(g) => {
                                            st.goodbye = Some(g.clone());
                                        }
                                        Msg::SetLayout(l) => {
                                            st.latest_layout = Some(l.clone());
                                        }
                                        Msg::NavState(n) => {
                                            st.latest_nav_state = Some(n.clone());
                                        }
                                        Msg::SetRail(r) => {
                                            st.latest_rail = Some(r.clone());
                                        }
                                        Msg::PlayStream(p) => {
                                            st.play_stream_history.push(p.clone());
                                        }
                                        Msg::UpdateControls(u) => {
                                            st.latest_update_controls = Some(u.clone());
                                            st.update_controls_history.push(u.clone());
                                        }
                                        Msg::Feedback(f) => {
                                            st.latest_feedback = Some(f.clone());
                                            st.feedback_history.push(f.clone());
                                        }
                                        Msg::Ping(p) => {
                                            st.pings_received += 1;
                                            if auto_pong {
                                                st.pongs_sent += 1;
                                                is_ping = true;
                                                ping_nonce = p.nonce;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }

                            let _ = events_tx.send(env);

                            if is_ping {
                                let pong_seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                                let pong_env =
                                    Envelope::new(pong_seq, Msg::Pong(Pong { nonce: ping_nonce, host_time_ms: 0 }));
                                if outgoing_tx.send(pong_env).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::debug!("fakephone reader received EOF");
                            break;
                        }
                        Err(e) => {
                            tracing::debug!("fakephone reader error: {e}");
                            break;
                        }
                    }
                }
                let _ = closed_tx.send(true);
            }
        });

        let phone = FakePhone {
            config,
            outgoing_tx,
            seq_counter,
            state,
            events_tx,
            closed_rx,
            host_fp,
            own_fp,
            remote_addr,
            identity,
            writer_handle,
            reader_handle,
        };
        // On error `phone` drops here and aborts its tasks.
        phone.handshake().await?;
        Ok(phone)
    }

    /// Hello, optional PairRequest, then wait for Welcome (following pending approval) and layout.
    async fn handshake(&self) -> Result<(), FakePhoneError> {
        let mut rx = self.events_tx.subscribe();
        let mut closed = self.closed_rx.clone();

        let hello = hello_for(&self.config, &self.own_fp.to_hex());
        self.send_msg(Msg::Hello(hello)).await?;
        if let Some(secret) = &self.config.pair_secret {
            let proof = compute_proof(secret, &self.own_fp, &self.host_fp);
            self.send_msg(Msg::PairRequest(PairRequest {
                proof: proof.to_vec(),
                device_name: self.config.device_name.clone(),
            }))
            .await?;
        }

        let mut deadline = tokio::time::Instant::now() + self.config.handshake_timeout;
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(env) => match env.msg {
                        Some(Msg::Welcome(_)) => break,
                        Some(Msg::PairResult(r)) if !r.ok => {
                            return Err(FakePhoneError::PairingRejected(r.error));
                        }
                        Some(Msg::PairResult(_)) => {
                            deadline = tokio::time::Instant::now() + self.config.handshake_timeout;
                        }
                        Some(Msg::PairingPending(p)) => {
                            if let Some(h) = &self.config.on_pending {
                                (h.0)(&p.host_name);
                            }
                            deadline = tokio::time::Instant::now() + self.config.approval_timeout;
                        }
                        Some(Msg::Goodbye(g)) => return Err(goodbye_error(&g)),
                        _ => {}
                    },
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(FakePhoneError::ConnectionClosed),
                },
                _ = closed.changed() => {
                    let st = self.state.read().await;
                    if st.welcome.is_some() {
                        break;
                    }
                    if let Some(r) = st.pair_result.as_ref().filter(|r| !r.ok) {
                        return Err(FakePhoneError::PairingRejected(r.error.clone()));
                    }
                    if let Some(g) = &st.goodbye {
                        return Err(goodbye_error(g));
                    }
                    return Err(FakePhoneError::ConnectionClosed);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(FakePhoneError::Timeout(if self.state.read().await.pairing_pending.is_some() {
                        "approval on the PC"
                    } else {
                        "Welcome"
                    }));
                }
            }
        }

        if self.config.require_layout {
            self.ensure_layout(self.config.handshake_timeout).await?;
        }
        Ok(())
    }

    /// Address of the host this phone connected to (used for the media connection).
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// TLS client identity of this phone (the media connection must present the same one).
    pub fn identity(&self) -> Arc<ClientIdentity> {
        Arc::clone(&self.identity)
    }

    /// Session id from `Welcome`, once received.
    pub async fn session_id(&self) -> Option<String> {
        self.state.read().await.welcome.as_ref().map(|w| w.session_id.clone())
    }

    /// Send any message on the control connection.
    pub async fn send_message(&self, msg: Msg) -> Result<(), FakePhoneError> {
        self.send_msg(msg).await
    }

    async fn send_msg(&self, msg: Msg) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        self.outgoing_tx.send(Envelope::new(seq, msg)).await.map_err(|_| FakePhoneError::ChannelClosed)
    }

    /// Return the current layout, waiting up to `timeout` for the first one if none arrived yet.
    pub async fn ensure_layout(&self, timeout: Duration) -> Result<SetLayout, FakePhoneError> {
        let mut rx = self.events_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(l) = self.state.read().await.latest_layout.clone() {
                return Ok(l);
            }
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Err(_) => return Err(FakePhoneError::Timeout("SetLayout")),
            }
        }
    }

    /// SHA-256 fingerprint of the host certificate seen in the TLS handshake.
    pub fn host_fp(&self) -> Fingerprint {
        self.host_fp
    }

    /// Fingerprint of this phone's own certificate.
    pub fn own_fp(&self) -> Fingerprint {
        self.own_fp
    }

    /// The `Goodbye` received from the host, if any.
    pub async fn goodbye(&self) -> Option<Goodbye> {
        self.state.read().await.goodbye.clone()
    }

    /// The `PairResult` received from the host, if any.
    pub async fn pair_result(&self) -> Option<PairResult> {
        self.state.read().await.pair_result.clone()
    }

    /// Whether the host ever answered `PairingPending`.
    pub async fn was_pending(&self) -> bool {
        self.state.read().await.pairing_pending.is_some()
    }

    /// Whether the connection has ended.
    pub fn is_closed(&self) -> bool {
        *self.closed_rx.borrow()
    }

    /// Wait until the connection ends. `None` waits forever. Returns whether it closed in time.
    pub async fn wait_closed(&self, timeout: Option<Duration>) -> bool {
        let mut rx = self.closed_rx.clone();
        let fut = async {
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        };
        match timeout {
            Some(t) => tokio::time::timeout(t, fut).await.is_ok(),
            None => {
                fut.await;
                true
            }
        }
    }

    /// Next sequence number for an outgoing message.
    fn next_seq(&self) -> u32 {
        self.seq_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Active configuration of this fake phone.
    pub fn config(&self) -> &FakePhoneConfig {
        &self.config
    }

    /// Subscribe to all raw incoming envelopes.
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.events_tx.subscribe()
    }

    // ----------------------------------------------------------- State inspection

    /// Inspect latest `Welcome` message from host.
    pub async fn welcome(&self) -> Option<Welcome> {
        self.state.read().await.welcome.clone()
    }

    /// Inspect latest `SetLayout` message received from host.
    pub async fn latest_layout(&self) -> Option<SetLayout> {
        self.state.read().await.latest_layout.clone()
    }

    /// Inspect latest `UpdateControls` message received from host.
    pub async fn latest_update_controls(&self) -> Option<UpdateControls> {
        self.state.read().await.latest_update_controls.clone()
    }

    /// Complete history of `UpdateControls` received during this session.
    pub async fn update_controls_history(&self) -> Vec<UpdateControls> {
        self.state.read().await.update_controls_history.clone()
    }

    /// Inspect latest `Feedback` message received from host.
    pub async fn latest_feedback(&self) -> Option<Feedback> {
        self.state.read().await.latest_feedback.clone()
    }

    /// Complete history of `Feedback` messages received.
    pub async fn feedback_history(&self) -> Vec<Feedback> {
        self.state.read().await.feedback_history.clone()
    }

    /// Complete history of all `Envelope` messages received from host.
    pub async fn all_received(&self) -> Vec<Envelope> {
        self.state.read().await.all_received.clone()
    }

    /// Latest `NavState` received from host.
    pub async fn latest_nav_state(&self) -> Option<NavState> {
        self.state.read().await.latest_nav_state.clone()
    }

    /// Latest `SetRail` received from host.
    pub async fn latest_rail(&self) -> Option<SetRail> {
        self.state.read().await.latest_rail.clone()
    }

    /// Complete history of `PlayStream` messages received.
    pub async fn play_stream_history(&self) -> Vec<PlayStream> {
        self.state.read().await.play_stream_history.clone()
    }

    /// Latest `PlayStream` received from host.
    pub async fn latest_play_stream(&self) -> Option<PlayStream> {
        self.state.read().await.play_stream_history.last().cloned()
    }

    /// Current active page ID from the latest layout.
    pub async fn current_page_id(&self) -> Option<String> {
        self.latest_layout().await.map(|l| l.page_id)
    }

    /// Number of Ping messages received from host.
    pub async fn pings_received_count(&self) -> u64 {
        self.state.read().await.pings_received
    }

    /// Number of Pong messages automatically sent to host.
    pub async fn pongs_sent_count(&self) -> u64 {
        self.state.read().await.pongs_sent
    }

    // ------------------------------------------------------------- Input helpers

    /// Send a raw input event to the host.
    pub async fn send_input(&self, event: InputEvent) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env = Envelope::new(seq, Msg::Input(event));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)?;
        Ok(())
    }

    /// Send a KEY_DOWN event for a control on the current layout page.
    pub async fn send_key_down(&self, control_id: &str) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_key_down_on_page(&page_id, control_id).await
    }

    /// Send a KEY_DOWN event for a control on an explicit page.
    pub async fn send_key_down_on_page(&self, page_id: &str, control_id: &str) -> Result<(), FakePhoneError> {
        self.send_input(InputEvent {
            page_id: page_id.to_string(),
            control_id: control_id.to_string(),
            kind: InputKind::KeyDown as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Send a KEY_UP event for a control on the current layout page.
    pub async fn send_key_up(&self, control_id: &str) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_key_up_on_page(&page_id, control_id).await
    }

    /// Send a KEY_UP event for a control on an explicit page.
    pub async fn send_key_up_on_page(&self, page_id: &str, control_id: &str) -> Result<(), FakePhoneError> {
        self.send_input(InputEvent {
            page_id: page_id.to_string(),
            control_id: control_id.to_string(),
            kind: InputKind::KeyUp as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Simulate a button tap: KEY_DOWN followed by KEY_UP after 20ms.
    pub async fn simulate_tap(&self, control_id: &str) -> Result<(), FakePhoneError> {
        self.simulate_tap_with_duration(control_id, Duration::from_millis(20)).await
    }

    /// Simulate a button tap with a custom press hold duration.
    pub async fn simulate_tap_with_duration(
        &self,
        control_id: &str,
        hold_duration: Duration,
    ) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_key_down_on_page(&page_id, control_id).await?;
        if !hold_duration.is_zero() {
            tokio::time::sleep(hold_duration).await;
        }
        self.send_key_up_on_page(&page_id, control_id).await?;
        Ok(())
    }

    /// Tap a side-rail key (`page_id` `"__rail"`).
    pub async fn simulate_rail_tap(&self, control_id: &str) -> Result<(), FakePhoneError> {
        self.send_key_down_on_page("__rail", control_id).await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.send_key_up_on_page("__rail", control_id).await
    }

    /// Send a LONG_PRESS event for a control.
    pub async fn send_long_press(&self, control_id: &str) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_input(InputEvent {
            page_id,
            control_id: control_id.to_string(),
            kind: InputKind::LongPress as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Send a DIAL_ROTATE event with detent delta (+ for clockwise, - for counter-clockwise).
    pub async fn send_dial_rotate(&self, control_id: &str, delta: i32) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_input(InputEvent {
            page_id,
            control_id: control_id.to_string(),
            kind: InputKind::DialRotate as i32,
            delta,
            value: 0.0,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Send a DIAL_PRESS event.
    pub async fn send_dial_press(&self, control_id: &str) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_input(InputEvent {
            page_id,
            control_id: control_id.to_string(),
            kind: InputKind::DialPress as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Send a DIAL_RELEASE event.
    pub async fn send_dial_release(&self, control_id: &str) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_input(InputEvent {
            page_id,
            control_id: control_id.to_string(),
            kind: InputKind::DialRelease as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Send a SLIDER_VALUE event (value in range 0.0..=1.0).
    pub async fn send_slider_value(&self, control_id: &str, value: f32) -> Result<(), FakePhoneError> {
        let page_id = self.current_page_id().await.unwrap_or_default();
        self.send_input(InputEvent {
            page_id,
            control_id: control_id.to_string(),
            kind: InputKind::SliderValue as i32,
            delta: 0,
            value,
            phone_time_ms: now_millis(),
        })
        .await
    }

    /// Send periodic device health report.
    pub async fn send_health(&self, health: DeviceHealth) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env = Envelope::new(seq, Msg::Health(health));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)?;
        Ok(())
    }

    /// Send manual Ping.
    pub async fn send_ping(&self, nonce: u64) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env = Envelope::new(seq, Msg::Ping(Ping { nonce }));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)?;
        Ok(())
    }

    /// Send manual Pong.
    pub async fn send_pong(&self, nonce: u64) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env = Envelope::new(seq, Msg::Pong(Pong { nonce, host_time_ms: 0 }));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)?;
        Ok(())
    }

    /// Send Goodbye message before closing.
    pub async fn send_goodbye(&self, reason: GoodbyeReason, message: &str) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env = Envelope::new(seq, Msg::Goodbye(Goodbye { reason: reason as i32, message: message.to_string() }));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)?;
        Ok(())
    }

    /// Send a navigation-rail request (Home, Back, ...) to the host.
    pub async fn send_nav_request(&self, action: NavAction) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env = Envelope::new(seq, Msg::NavRequest(NavRequest { action: action as i32, page_index: 0 }));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)
    }

    /// Page-dot tap: `NAV_ACTION_GO_TO_PAGE` for top-level page `index`.
    pub async fn send_nav_go_to_page(&self, index: u32) -> Result<(), FakePhoneError> {
        let seq = self.next_seq();
        let env =
            Envelope::new(seq, Msg::NavRequest(NavRequest { action: NavAction::GoToPage as i32, page_index: index }));
        self.outgoing_tx.send(env).await.map_err(|_| FakePhoneError::ChannelClosed)
    }

    // ------------------------------------------------------------- Wait helpers

    /// Wait for the next `NavState` envelope within `timeout`.
    pub async fn wait_for_nav_state(&self, timeout: Duration) -> Result<NavState, FakePhoneError> {
        let mut rx = self.events_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("NavState"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(Envelope { msg: Some(Msg::NavState(n)), .. })) => return Ok(n),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => return Err(FakePhoneError::Timeout("NavState")),
            }
        }
    }

    /// Wait for the next `SetLayout` envelope within `timeout`.
    pub async fn wait_for_layout(&self, timeout: Duration) -> Result<SetLayout, FakePhoneError> {
        let mut rx = self.events_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("SetLayout"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(Envelope { msg: Some(Msg::SetLayout(l)), .. })) => return Ok(l),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => return Err(FakePhoneError::Timeout("SetLayout")),
            }
        }
    }

    /// Wait for the next `UpdateControls` message within `timeout`.
    pub async fn wait_for_update(&self, timeout: Duration) -> Result<UpdateControls, FakePhoneError> {
        let mut rx = self.events_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("UpdateControls"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(Envelope { msg: Some(Msg::UpdateControls(u)), .. })) => return Ok(u),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => return Err(FakePhoneError::Timeout("UpdateControls")),
            }
        }
    }

    /// Wait for the next `Feedback` message within `timeout`.
    pub async fn wait_for_feedback(&self, timeout: Duration) -> Result<Feedback, FakePhoneError> {
        let mut rx = self.events_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(FakePhoneError::Timeout("Feedback"));
            }
            match tokio::time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(Envelope { msg: Some(Msg::Feedback(f)), .. })) => return Ok(f),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(FakePhoneError::ConnectionClosed),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Err(_) => return Err(FakePhoneError::Timeout("Feedback")),
            }
        }
    }

    /// Disconnect and terminate client worker tasks.
    pub fn disconnect(self) {
        self.writer_handle.abort();
        self.reader_handle.abort();
    }
}

impl Drop for FakePhone {
    fn drop(&mut self) {
        self.writer_handle.abort();
        self.reader_handle.abort();
    }
}
