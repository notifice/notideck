// SPDX-License-Identifier: GPL-3.0-or-later
//! Handle for an active client session.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use nd_proto::v1::{
    CameraControl, CameraReadout, CameraStart, CameraStatus, CameraStop, Capability, DeviceHealth, Feedback, Goodbye,
    GoodbyeReason, PutIcon, ScreenInfo, SetLayout, UpdateControls,
};
use nd_proto::{Envelope, Msg};
use tokio::sync::{mpsc, Notify};

use crate::error::{Result, TransportError};
use crate::pairing::Fingerprint;
use crate::trust::{Link, PeerInfo, TrustState};

#[derive(Debug)]
struct SessionInner {
    session_id: String,
    device_id: String,
    device_model: String,
    app_version: String,
    android_sdk: u32,
    screen: Option<ScreenInfo>,
    capabilities: Vec<Capability>,
    outgoing_tx: mpsc::Sender<Envelope>,
    peer: PeerInfo,
    trust_state: TrustState,
    close_requested: Notify,
    goodbye: Mutex<Option<Goodbye>>,
    next_seq: AtomicU32,
    closed: AtomicBool,
    health: Mutex<Option<DeviceHealth>>,
    camera_status: Mutex<Option<CameraStatus>>,
    camera_readout: Mutex<Option<CameraReadout>>,
    mic_status: Mutex<Option<nd_proto::v1::MicStatus>>,
}

/// A cloneable handle to communicate with a connected client.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    inner: Arc<SessionInner>,
}

impl SessionHandle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: String,
        device_id: String,
        device_model: String,
        app_version: String,
        android_sdk: u32,
        screen: Option<ScreenInfo>,
        capabilities: Vec<Capability>,
        outgoing_tx: mpsc::Sender<Envelope>,
        peer: PeerInfo,
        trust_state: TrustState,
    ) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                session_id,
                device_id,
                device_model,
                app_version,
                android_sdk,
                screen,
                capabilities,
                outgoing_tx,
                peer,
                trust_state,
                close_requested: Notify::new(),
                goodbye: Mutex::new(None),
                next_seq: AtomicU32::new(1),
                closed: AtomicBool::new(false),
                health: Mutex::new(None),
                camera_status: Mutex::new(None),
                camera_readout: Mutex::new(None),
                mic_status: Mutex::new(None),
            }),
        }
    }

    /// The unique session ID (UUID) assigned to this connection.
    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    /// Stable unique device ID reported by the client.
    pub fn device_id(&self) -> &str {
        &self.inner.device_id
    }

    /// Client device model (e.g. "Google Pixel 6").
    pub fn device_model(&self) -> &str {
        &self.inner.device_model
    }

    /// App version string from the client.
    pub fn app_version(&self) -> &str {
        &self.inner.app_version
    }

    /// Android API level.
    pub fn android_sdk(&self) -> u32 {
        self.inner.android_sdk
    }

    /// Client screen dimensions and DPI if provided.
    pub fn screen(&self) -> Option<&ScreenInfo> {
        self.inner.screen.as_ref()
    }

    /// Hardware capabilities reported by the client.
    pub fn capabilities(&self) -> &[Capability] {
        &self.inner.capabilities
    }

    /// Remote socket address of the control connection.
    pub fn peer_addr(&self) -> SocketAddr {
        self.inner.peer.addr
    }

    /// SHA-256 fingerprint of the phone's TLS client certificate (its identity).
    pub fn peer_fp(&self) -> Fingerprint {
        self.inner.peer.fp
    }

    /// Everything the server learned about the phone during the handshake.
    pub fn peer_info(&self) -> &PeerInfo {
        &self.inner.peer
    }

    /// Pairing state when the session was established (always [`TrustState::Paired`] for
    /// sessions that reach `SessionConnected`).
    pub fn trust_state(&self) -> TrustState {
        self.inner.trust_state
    }

    /// How the phone reached us.
    pub fn link(&self) -> Link {
        self.inner.peer.link
    }

    /// `"usb"` when the peer is loopback (`adb reverse` arrives on 127.0.0.1), else `"lan"`
    /// (see [`Link`]).
    pub fn transport(&self) -> &'static str {
        self.inner.peer.link.as_str()
    }

    /// Returns `true` if the session is still active and connected.
    pub fn is_connected(&self) -> bool {
        !self.inner.closed.load(Ordering::SeqCst) && !self.inner.outgoing_tx.is_closed()
    }

    /// Allocate the next monotonic sequence number for outgoing envelopes.
    pub fn next_seq(&self) -> u32 {
        self.inner.next_seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a message inside an Envelope to the client.
    pub async fn send_msg(&self, msg: Msg) -> Result<()> {
        if !self.is_connected() {
            return Err(TransportError::SessionClosed);
        }

        let seq = self.next_seq();
        let env = Envelope::new(seq, msg);

        self.inner.outgoing_tx.send(env).await.map_err(|_| TransportError::SessionClosed)?;

        Ok(())
    }

    /// Try to send a message without awaiting if buffer capacity exists.
    pub fn try_send_msg(&self, msg: Msg) -> Result<()> {
        if !self.is_connected() {
            return Err(TransportError::SessionClosed);
        }

        let seq = self.next_seq();
        let env = Envelope::new(seq, msg);

        self.inner.outgoing_tx.try_send(env).map_err(|e| TransportError::Send(e.to_string()))?;

        Ok(())
    }

    /// Push a new full layout grid to the client.
    pub async fn set_layout(&self, layout: SetLayout) -> Result<()> {
        self.send_msg(Msg::SetLayout(layout)).await
    }

    /// Push partial control updates to the client.
    pub async fn update_controls(&self, update: UpdateControls) -> Result<()> {
        self.send_msg(Msg::UpdateControls(update)).await
    }

    /// Push icon image data to the client.
    pub async fn put_icon(&self, icon: PutIcon) -> Result<()> {
        self.send_msg(Msg::PutIcon(icon)).await
    }

    /// Send feedback (haptic, toast, flash) to the client.
    pub async fn send_feedback(&self, feedback: Feedback) -> Result<()> {
        self.send_msg(Msg::Feedback(feedback)).await
    }

    /// Round 20: play (or, with an empty url, close) a stream in the phone's built-in player.
    pub async fn play_stream(&self, play: nd_proto::v1::PlayStream) -> Result<()> {
        self.send_msg(Msg::PlayStream(play)).await
    }

    /// Ask the phone to start streaming its camera (it then opens the media connection).
    pub async fn camera_start(&self, start: CameraStart) -> Result<()> {
        self.send_msg(Msg::CameraStart(start)).await
    }

    /// Ask the phone to stop streaming its camera.
    pub async fn camera_stop(&self) -> Result<()> {
        self.send_msg(Msg::CameraStop(CameraStop {})).await
    }

    /// Change camera settings or request a key frame.
    pub async fn camera_control(&self, control: CameraControl) -> Result<()> {
        self.send_msg(Msg::CameraControl(control)).await
    }

    /// Round 25: ask the phone to start (or reconfigure) its microphone. It then opens an audio
    /// media connection. Only for phones with `CAPABILITY_MIC`.
    pub async fn mic_start(&self, start: nd_proto::v1::MicStart) -> Result<()> {
        self.send_msg(Msg::MicStart(start)).await
    }

    /// Round 25: ask the phone to stop its microphone.
    pub async fn mic_stop(&self) -> Result<()> {
        self.send_msg(Msg::MicStop(nd_proto::v1::MicStop {})).await
    }

    /// Round 25: last `MicStatus` reported by the phone, if any.
    pub fn last_mic_status(&self) -> Option<nd_proto::v1::MicStatus> {
        self.inner.mic_status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn set_mic_status(&self, status: nd_proto::v1::MicStatus) {
        *self.inner.mic_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
    }

    /// Last [`CameraStatus`] reported by the phone, if any.
    pub fn last_camera_status(&self) -> Option<CameraStatus> {
        self.inner.camera_status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Latest [`CameraReadout`] from the phone (round 15), if it ever sent one.
    pub fn last_camera_readout(&self) -> Option<CameraReadout> {
        *self.inner.camera_readout.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set_camera_readout(&self, readout: CameraReadout) {
        *self.inner.camera_readout.lock().unwrap_or_else(|e| e.into_inner()) = Some(readout);
    }

    pub(crate) fn set_camera_status(&self, status: CameraStatus) {
        *self.inner.camera_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
    }

    /// Gracefully close the session: the session task writes a [`Goodbye`], shuts the TLS stream
    /// down (flushing it) and ends. Queued outgoing messages are dropped. Idempotent.
    pub fn close(&self, reason: GoodbyeReason, message: impl Into<String>) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.inner.goodbye.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Goodbye { reason: reason as i32, message: message.into() });
        // `notify_one` stores a permit, so the session task sees it even if it is mid-write.
        self.inner.close_requested.notify_one();
    }

    pub(crate) async fn close_requested(&self) {
        self.inner.close_requested.notified().await;
    }

    pub(crate) fn take_goodbye(&self) -> Option<Goodbye> {
        self.inner.goodbye.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Last [`DeviceHealth`] report from the phone, if any.
    pub fn health(&self) -> Option<DeviceHealth> {
        *self.inner.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set_health(&self, health: DeviceHealth) {
        *self.inner.health.lock().unwrap_or_else(|e| e.into_inner()) = Some(health);
    }

    pub(crate) fn mark_closed(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
    }
}
