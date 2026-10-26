// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS control channel server (SPEC §4.5, §5; ADR 0004). One listener serves USB (`adb reverse`,
//! loopback peers) and LAN phones.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use nd_proto::v1::{Feedback, GoodbyeReason, InputEvent, MediaKind, PutIcon, SetLayout, UpdateControls};
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_rustls::TlsAcceptor;

use crate::error::{Result, TransportError};
use crate::handle::SessionHandle;
use crate::identity::HostIdentity;
use crate::pairing::Fingerprint;
use crate::session::{handle_connection, ServerCtx, TransportEvent};
use crate::trust::{Link, PeerInfo, PendingPeer, TrustStore};

/// Decision for a pending USB phone, delivered to its session task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingCmd {
    Allow,
    Deny,
    Revoke,
}

struct PendingEntry {
    peer: PeerInfo,
    cmd: mpsc::Sender<PendingCmd>,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, SessionHandle>,
    active: Option<String>,
    pending: HashMap<String, PendingEntry>,
    /// Close switch of the live media stream per session id and kind (ADR 0005; round 25 adds audio).
    media: HashMap<(String, MediaKind), watch::Sender<bool>>,
}

/// State shared between the accept loop, the session tasks and every [`ServerHandle`].
pub(crate) struct Shared {
    state: Mutex<State>,
    pub(crate) event_tx: broadcast::Sender<TransportEvent>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register an established session. Any older session of the same certificate is removed
    /// and closed with `SUPERSEDED`: one session per phone, the newest wins (so a USB session
    /// replaces a LAN one, ADR 0004 §8).
    pub(crate) fn register_session(&self, session: SessionHandle) {
        let mut st = self.lock();
        let fp = session.peer_fp();
        let old: Vec<String> =
            st.sessions.iter().filter(|(_, s)| s.peer_fp() == fp).map(|(id, _)| id.clone()).collect();
        for id in old {
            if let Some(s) = st.sessions.remove(&id) {
                s.close(GoodbyeReason::Superseded, "Replaced by a newer connection from this phone");
            }
        }
        let id = session.session_id().to_string();
        st.sessions.insert(id.clone(), session);
        st.active = Some(id);
    }

    /// Remove a finished session. Returns `true` if another session of the same certificate
    /// is still registered (so the device is still connected).
    pub(crate) fn remove_session(&self, id: &str, fp: &Fingerprint) -> bool {
        let mut st = self.lock();
        st.sessions.remove(id);
        for kind in [MediaKind::Video, MediaKind::Audio] {
            if let Some(m) = st.media.remove(&(id.to_string(), kind)) {
                let _ = m.send(true);
            }
        }
        if st.active.as_deref() == Some(id) {
            st.active = None;
        }
        st.sessions.values().any(|s| s.peer_fp() == *fp)
    }

    /// The live, paired session `id`, if it belongs to `fp`.
    pub(crate) fn session_for_media(&self, id: &str, fp: &Fingerprint) -> Option<SessionHandle> {
        let st = self.lock();
        st.sessions.get(id).filter(|s| s.peer_fp() == *fp && s.is_connected()).cloned()
    }

    /// Install a new media stream for `id`, closing the previous one. Returns the receiver half
    /// of the close switch for the new stream.
    pub(crate) fn attach_media(&self, id: &str, kind: MediaKind) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        if let Some(old) = self.lock().media.insert((id.to_string(), kind), tx) {
            let _ = old.send(true);
        }
        rx
    }

    /// Race guard: if the session vanished while attaching, close the switch we just installed.
    pub(crate) fn detach_media_if_closed(&self, id: &str, kind: MediaKind) {
        let mut st = self.lock();
        if !st.sessions.contains_key(id) {
            if let Some(m) = st.media.remove(&(id.to_string(), kind)) {
                let _ = m.send(true);
            }
        }
    }

    pub(crate) fn add_pending(&self, session_id: &str, peer: PeerInfo, cmd: mpsc::Sender<PendingCmd>) {
        self.lock().pending.insert(session_id.to_string(), PendingEntry { peer, cmd });
    }

    pub(crate) fn drop_pending(&self, session_id: &str) {
        self.lock().pending.remove(session_id);
    }

    fn close_all(&self, reason: GoodbyeReason, message: &str) {
        let sessions: Vec<SessionHandle> = self.lock().sessions.values().cloned().collect();
        for s in sessions {
            s.close(reason, message);
        }
    }
}

/// System hostname helper.
pub fn get_host_name() -> String {
    if let Ok(name) = std::env::var("HOSTNAME") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    if let Ok(content) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "Notideck Host".to_string()
}

/// Cloneable handle to the running server.
#[derive(Clone)]
pub struct ServerHandle {
    shared: Arc<Shared>,
    host_fp: Fingerprint,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle").field("sessions", &self.session_count()).finish_non_exhaustive()
    }
}

impl ServerHandle {
    /// Returns the currently active session handle if connected.
    pub fn active_session(&self) -> Option<SessionHandle> {
        let st = self.shared.lock();
        st.sessions.get(st.active.as_ref()?).cloned()
    }

    /// Returns all connected sessions.
    pub fn all_sessions(&self) -> Vec<SessionHandle> {
        self.shared.lock().sessions.values().cloned().collect()
    }

    /// Round 20: send `PlayStream` to the phone that should play it: the active session if it advertises
    /// `CAPABILITY_PLAYER`, else the first connected session that does. `Err(NoActiveSession)` if none can.
    pub async fn play_stream(&self, play: nd_proto::v1::PlayStream) -> Result<()> {
        let can = |s: &SessionHandle| s.capabilities().contains(&nd_proto::v1::Capability::Player);
        let target =
            self.active_session().filter(|s| can(s)).or_else(|| self.all_sessions().into_iter().find(|s| can(s)));
        match target {
            Some(s) => s.play_stream(play).await,
            None => Err(TransportError::NoActiveSession),
        }
    }

    /// Round 20: close the player on every session that has `CAPABILITY_PLAYER`.
    pub async fn stop_stream(&self) {
        for s in self.all_sessions() {
            if s.capabilities().contains(&nd_proto::v1::Capability::Player) {
                let _ = s.play_stream(nd_proto::v1::PlayStream::default()).await;
            }
        }
    }

    /// Number of connected sessions.
    pub fn session_count(&self) -> usize {
        self.shared.lock().sessions.len()
    }

    /// Push a layout to the active session (or all connected sessions if no single active).
    pub async fn set_layout(&self, layout: SetLayout) -> Result<()> {
        if let Some(session) = self.active_session() {
            return session.set_layout(layout).await;
        }

        let all = self.all_sessions();
        if all.is_empty() {
            return Err(TransportError::NoActiveSession);
        }

        for s in all {
            let _ = s.set_layout(layout.clone()).await;
        }
        Ok(())
    }

    /// Push control updates to the active session (or all connected sessions).
    pub async fn update_controls(&self, update: UpdateControls) -> Result<()> {
        if let Some(session) = self.active_session() {
            return session.update_controls(update).await;
        }

        let all = self.all_sessions();
        if all.is_empty() {
            return Err(TransportError::NoActiveSession);
        }

        for s in all {
            let _ = s.update_controls(update.clone()).await;
        }
        Ok(())
    }

    /// Push icon data to the active session (or all connected sessions).
    pub async fn put_icon(&self, icon: PutIcon) -> Result<()> {
        if let Some(session) = self.active_session() {
            return session.put_icon(icon).await;
        }

        let all = self.all_sessions();
        if all.is_empty() {
            return Err(TransportError::NoActiveSession);
        }

        for s in all {
            let _ = s.put_icon(icon.clone()).await;
        }
        Ok(())
    }

    /// Send feedback (haptic, toast, flash) to the active session.
    pub async fn send_feedback(&self, feedback: Feedback) -> Result<()> {
        if let Some(session) = self.active_session() {
            return session.send_feedback(feedback).await;
        }

        let all = self.all_sessions();
        if all.is_empty() {
            return Err(TransportError::NoActiveSession);
        }

        for s in all {
            let _ = s.send_feedback(feedback.clone()).await;
        }
        Ok(())
    }

    /// Subscribe to all transport events (connect, disconnect, input, health, etc.).
    pub fn subscribe_events(&self) -> broadcast::Receiver<TransportEvent> {
        self.shared.event_tx.subscribe()
    }

    /// Fingerprint of the host certificate this server presents.
    pub fn host_fp(&self) -> Fingerprint {
        self.host_fp
    }

    /// Unknown USB phones currently waiting for Allow/Deny.
    pub fn pending(&self) -> Vec<PendingPeer> {
        let mut v: Vec<PendingPeer> = self
            .shared
            .lock()
            .pending
            .iter()
            .map(|(id, e)| PendingPeer { session_id: id.clone(), peer: e.peer.clone() })
            .collect();
        v.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        v
    }

    /// Allow or deny a pending USB phone. Returns `false` if `session_id` is not (or no longer)
    /// pending. Allow pins the phone through the [`TrustStore`] and lets the handshake finish.
    pub fn resolve_pending(&self, session_id: &str, allow: bool) -> bool {
        let entry = self.shared.lock().pending.remove(session_id);
        match entry {
            Some(e) => e.cmd.try_send(if allow { PendingCmd::Allow } else { PendingCmd::Deny }).is_ok(),
            None => false,
        }
    }

    /// Close every session and pending entry of `fp` with `REVOKED`. Returns whether any existed.
    /// The caller is responsible for removing the pin from its [`TrustStore`] first.
    pub fn revoke(&self, fp: &Fingerprint) -> bool {
        let mut found = false;
        let (sessions, pending) = {
            let mut st = self.shared.lock();
            let sessions: Vec<SessionHandle> = st.sessions.values().filter(|s| s.peer_fp() == *fp).cloned().collect();
            let ids: Vec<String> =
                st.pending.iter().filter(|(_, e)| e.peer.fp == *fp).map(|(id, _)| id.clone()).collect();
            let pending: Vec<PendingEntry> = ids.iter().filter_map(|id| st.pending.remove(id)).collect();
            (sessions, pending)
        };
        for s in sessions {
            found = true;
            s.close(GoodbyeReason::Revoked, "This phone was removed on the PC");
        }
        for p in pending {
            found = true;
            let _ = p.cmd.try_send(PendingCmd::Revoke);
        }
        found
    }
}

/// TLS listener control server.
pub struct ControlServer {
    listener: TcpListener,
    local_addr: SocketAddr,
    host_name: String,
    identity: Arc<HostIdentity>,
    trust: Arc<dyn TrustStore>,
    acceptor: TlsAcceptor,
    shared: Arc<Shared>,
    classify: fn(&SocketAddr) -> Link,
    input_tx: mpsc::Sender<InputEvent>,
    input_rx: Option<mpsc::Receiver<InputEvent>>,
    media_tx: mpsc::Sender<crate::media::MediaStream>,
    media_rx: Option<mpsc::Receiver<crate::media::MediaStream>>,
    audio_tx: mpsc::Sender<crate::media::MediaStream>,
    audio_rx: Option<mpsc::Receiver<crate::media::MediaStream>>,
}

impl ControlServer {
    /// Bind a mutually-authenticated TLS 1.3 listener (ADR 0004). `0.0.0.0:<port>` serves USB
    /// and LAN; `127.0.0.1:<port>` is USB only.
    pub async fn bind_tls<A: ToSocketAddrs>(
        addr: A,
        identity: Arc<HostIdentity>,
        trust: Arc<dyn TrustStore>,
    ) -> Result<Self> {
        let tls = crate::tls::server_config(&identity)?;
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let (input_tx, input_rx) = mpsc::channel(256);
        let (media_tx, media_rx) = mpsc::channel(4);
        let (audio_tx, audio_rx) = mpsc::channel(4);
        let (event_tx, _) = broadcast::channel(128);

        Ok(Self {
            listener,
            local_addr,
            host_name: get_host_name(),
            identity,
            trust,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
            shared: Arc::new(Shared { state: Mutex::new(State::default()), event_tx }),
            classify: Link::of,
            input_tx,
            input_rx: Some(input_rx),
            media_tx,
            media_rx: Some(media_rx),
            audio_tx,
            audio_rx: Some(audio_rx),
        })
    }

    pub fn set_host_name(&mut self, name: impl Into<String>) {
        self.host_name = name.into();
    }

    /// Override how a peer address maps to [`Link`]. The default is [`Link::of`] (loopback is
    /// USB). Tests use this to exercise the LAN path over 127.0.0.1.
    #[doc(hidden)]
    pub fn set_link_classifier(&mut self, f: fn(&SocketAddr) -> Link) {
        self.classify = f;
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Fingerprint of the host certificate this server presents.
    pub fn host_fp(&self) -> Fingerprint {
        self.identity.fingerprint()
    }

    /// Takes the receiver for client input events. Can only be called once.
    pub fn take_input_receiver(&mut self) -> Option<mpsc::Receiver<InputEvent>> {
        self.input_rx.take()
    }

    /// Takes the receiver for accepted media connections (phone camera streams, ADR 0005).
    /// Can only be called once. If nobody takes it, media connections are refused.
    pub fn take_media_receiver(&mut self) -> Option<mpsc::Receiver<crate::media::MediaStream>> {
        self.media_rx.take()
    }

    /// Round 25: takes the receiver for accepted audio media connections (phone microphone).
    /// Can only be called once. If nobody takes it, audio attaches are refused.
    pub fn take_audio_receiver(&mut self) -> Option<mpsc::Receiver<crate::media::MediaStream>> {
        self.audio_rx.take()
    }

    /// Get a handle to control connected sessions.
    pub fn handle(&self) -> ServerHandle {
        ServerHandle { shared: Arc::clone(&self.shared), host_fp: self.identity.fingerprint() }
    }

    /// Spawns the server loop onto a background tokio task.
    ///
    /// Returns a [`tokio::task::JoinHandle`] and a [`watch::Sender<bool>`] to trigger shutdown.
    pub fn spawn(self) -> (tokio::task::JoinHandle<()>, watch::Sender<bool>) {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let ctx = Arc::new(ServerCtx {
            host_name: self.host_name,
            identity: self.identity,
            trust: self.trust,
            acceptor: self.acceptor,
            input_tx: self.input_tx,
            shared: Arc::clone(&self.shared),
            classify: self.classify,
            media_tx: self.media_tx,
            audio_tx: self.audio_tx,
        });
        let listener = self.listener;
        let shared = self.shared;

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            tracing::info!("ControlServer received shutdown signal");
                            break;
                        }
                    }

                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((stream, remote_addr)) => {
                                tracing::debug!(%remote_addr, "Accepted incoming control connection");
                                let ctx = Arc::clone(&ctx);
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, remote_addr, ctx).await {
                                        tracing::warn!(%remote_addr, "Connection ended with error: {e}");
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::error!("Failed to accept incoming connection: {e}");
                                // Avoid a hot loop on persistent errors (e.g. fd exhaustion).
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }

            shared.close_all(GoodbyeReason::Shutdown, "Notideck is shutting down");
        });

        (handle, shutdown_tx)
    }
}
