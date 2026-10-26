// SPDX-License-Identifier: GPL-3.0-or-later
//! Fake phone microphone (round 25): answers `MicStart` by opening an audio media connection
//! (`MediaAttach.kind = AUDIO`) and streaming a 440 Hz sine, mono s16le, in real time.
//! No cargo feature needed (no encoder).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nd_proto::framing;
use nd_proto::media::{write_packet, MediaPacket};
use nd_proto::v1::{Capability, MediaAttach, MediaKind, MicStart, MicState, MicStatus};
use nd_proto::{Envelope, Msg};
use nd_transport::tls::client_config;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant, MissedTickBehavior};
use tokio_rustls::TlsConnector;

use crate::client::FakePhone;
use crate::config::FakePhoneConfig;
use crate::error::FakePhoneError;

const STEP: Duration = Duration::from_secs(5);
/// Peak amplitude of the test tone.
pub const SINE_AMPLITUDE: f32 = 12_000.0;
pub const SINE_HZ: f32 = 440.0;

impl FakePhoneConfig {
    /// Also advertise `CAPABILITY_MIC`.
    pub fn with_mic(mut self) -> Self {
        if !self.capabilities.contains(&Capability::Mic) {
            self.capabilities.push(Capability::Mic);
        }
        self
    }
}

/// How the fake microphone behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MicMode {
    /// Stream the sine on `MicStart`.
    #[default]
    Normal,
    /// Report `NO_PERMISSION` (initially and on every `MicStart`); never stream.
    NoPermission,
}

/// Counters a test (or the CLI) can look at.
#[derive(Debug, Default)]
pub struct MicStats {
    /// `MicStart`s handled.
    pub starts: AtomicU64,
    /// `MicStop`s handled.
    pub stops: AtomicU64,
    /// Audio packets written.
    pub packets_sent: AtomicU64,
    /// True while the audio connection is open and packets flow.
    pub streaming: AtomicBool,
    /// `voice_processing` of the last `MicStart`.
    pub voice_processing: AtomicBool,
    /// `sample_rate` of the last `MicStart`.
    pub last_rate: AtomicU32,
}

/// The microphone side of a [`FakePhone`]. Dropping it stops everything.
pub struct FakeMic {
    stats: Arc<MicStats>,
    task: JoinHandle<()>,
    stream: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Drop for FakeMic {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(t) = self.stream.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
    }
}

impl FakeMic {
    /// Serve `MicStart` / `MicStop` for `phone` and send the initial `MicStatus` (the phone must
    /// have been configured with [`FakePhoneConfig::with_mic`] to advertise the capability).
    pub async fn attach(phone: Arc<FakePhone>, mode: MicMode) -> Result<Self, FakePhoneError> {
        let rx = phone.subscribe();
        let stats = Arc::new(MicStats::default());
        let first = if mode == MicMode::NoPermission { MicState::NoPermission } else { MicState::Stopped };
        send_status(&phone, first, 0, "").await?;
        let stream = Arc::new(Mutex::new(None));
        let task = tokio::spawn(controller(phone, rx, Arc::clone(&stats), mode, Arc::clone(&stream)));
        Ok(Self { stats, task, stream })
    }

    pub fn stats(&self) -> Arc<MicStats> {
        Arc::clone(&self.stats)
    }

    /// Resolves when the controller ended (control connection closed).
    pub async fn wait_ended(&mut self) {
        let _ = (&mut self.task).await;
    }
}

async fn send_status(phone: &FakePhone, state: MicState, rate: u32, error: &str) -> Result<(), FakePhoneError> {
    phone
        .send_message(Msg::MicStatus(MicStatus { state: state as i32, error: error.to_string(), sample_rate: rate }))
        .await
}

type StreamSlot = Arc<Mutex<Option<JoinHandle<()>>>>;

async fn controller(
    phone: Arc<FakePhone>,
    mut rx: broadcast::Receiver<Envelope>,
    stats: Arc<MicStats>,
    mode: MicMode,
    running: StreamSlot,
) {
    let stop = |running: &StreamSlot| {
        if let Some(t) = running.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
    };
    loop {
        let env = match rx.recv().await {
            Ok(e) => e,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        match env.msg {
            Some(Msg::MicStart(start)) => {
                stop(&running);
                stats.starts.fetch_add(1, Ordering::SeqCst);
                stats.voice_processing.store(start.voice_processing, Ordering::SeqCst);
                stats.last_rate.store(start.sample_rate, Ordering::SeqCst);
                if mode == MicMode::NoPermission {
                    let _ = send_status(&phone, MicState::NoPermission, 0, "").await;
                    continue;
                }
                let t = tokio::spawn(stream(Arc::clone(&phone), start, Arc::clone(&stats)));
                *running.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
            }
            Some(Msg::MicStop(_)) => {
                stop(&running);
                stats.stops.fetch_add(1, Ordering::SeqCst);
                stats.streaming.store(false, Ordering::SeqCst);
                let _ = send_status(&phone, MicState::Stopped, 0, "").await;
            }
            _ => {}
        }
    }
    stop(&running);
}

async fn open_media(
    phone: &FakePhone,
    session_id: String,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let id = phone.identity();
    let cfg =
        client_config(id.cert_der.clone(), id.key_der.clone(), Some(phone.host_fp())).map_err(|e| e.to_string())?;
    let tcp = timeout(STEP, TcpStream::connect(phone.remote_addr()))
        .await
        .map_err(|_| "media connect timed out")?
        .map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    let name = ServerName::try_from("notideck.local").expect("static name");
    let mut tls = timeout(STEP, TlsConnector::from(Arc::new(cfg)).connect(name, tcp))
        .await
        .map_err(|_| "media TLS timed out")?
        .map_err(|e| e.to_string())?;
    let attach = Envelope::new(1, Msg::MediaAttach(MediaAttach { session_id, kind: MediaKind::Audio as i32 }));
    timeout(STEP, framing::write_frame(&mut tls, &attach))
        .await
        .map_err(|_| "attach write timed out")?
        .map_err(|e| e.to_string())?;
    match timeout(STEP, framing::read_frame(&mut tls)).await {
        Ok(Ok(Some(Envelope { msg: Some(Msg::MediaAttached(a)), .. }))) if a.ok => Ok(tls),
        Ok(Ok(Some(Envelope { msg: Some(Msg::MediaAttached(a)), .. }))) => Err(format!("attach refused: {}", a.error)),
        Ok(_) => Err("no MediaAttached".into()),
        Err(_) => Err("MediaAttached timed out".into()),
    }
}

/// One sine frame; `n0` is the index of its first sample, so the phase continues across frames.
pub fn sine_frame(rate: u32, n0: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len * 2);
    for i in 0..len as u64 {
        let t = ((n0 + i) % rate as u64) as f32 / rate as f32;
        let v = (t * SINE_HZ * std::f32::consts::TAU).sin() * SINE_AMPLITUDE;
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

async fn stream(phone: Arc<FakePhone>, start: MicStart, stats: Arc<MicStats>) {
    let rate = if start.sample_rate == 0 { 48_000 } else { start.sample_rate };
    let frame_ms = if start.frame_ms == 0 { 20 } else { start.frame_ms.clamp(10, 40) };
    let _ = send_status(&phone, MicState::Starting, rate, "").await;
    let Some(session_id) = phone.session_id().await else {
        let _ = send_status(&phone, MicState::Error, 0, "no session").await;
        return;
    };
    let mut tls = match open_media(&phone, session_id).await {
        Ok(t) => t,
        Err(e) => {
            let _ = send_status(&phone, MicState::Error, 0, &e).await;
            return;
        }
    };
    let frame_len = (rate as u64 * frame_ms as u64 / 1000) as usize;
    let t0 = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(frame_ms as u64));
    tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
    let mut n: u64 = 0;
    loop {
        tick.tick().await;
        let pts = t0.elapsed().as_micros() as u64;
        let pkt = MediaPacket::new(pts, false, false, sine_frame(rate, n, frame_len));
        if timeout(STEP, write_packet(&mut tls, &pkt)).await.map(|r| r.is_ok()) != Ok(true) {
            break;
        }
        n += frame_len as u64;
        stats.packets_sent.fetch_add(1, Ordering::SeqCst);
        if !stats.streaming.swap(true, Ordering::SeqCst) {
            let _ = send_status(&phone, MicState::Streaming, rate, "").await;
        }
    }
    stats.streaming.store(false, Ordering::SeqCst);
}
