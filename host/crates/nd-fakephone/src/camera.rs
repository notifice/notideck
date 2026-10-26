// SPDX-License-Identifier: GPL-3.0-or-later
//! Fake phone camera (ADR 0005): answers `CameraStart` by opening the media connection and
//! streaming a synthetic H.264 test pattern (moving colour bars plus a frame counter) encoded
//! with `openh264`. Behind the `camera` cargo feature.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nd_proto::framing;
use nd_proto::media::{write_packet, MediaPacket};
use nd_proto::v1::{
    Antibanding, CameraControl, CameraImage, CameraMode, CameraReadout, CameraStart, CameraState, CameraStatus,
    ExposureMode, Lens, LensCaps, MediaAttach, WhiteBalance,
};
use nd_proto::{Envelope, Msg};
use nd_transport::tls::client_config;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile, RateControlMode,
};
use openh264::formats::YUVBuffer;
use openh264::OpenH264API;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use crate::client::FakePhone;
use crate::error::FakePhoneError;

const STEP: Duration = Duration::from_secs(5);

/// Counters a test (or the CLI) can look at while the camera runs.
#[derive(Debug, Default)]
pub struct CameraStats {
    /// Frame packets (not CONFIG) written to the media connection.
    pub frames_sent: AtomicU64,
    /// Of those, how many were key frames.
    pub key_frames_sent: AtomicU64,
    /// Number of `CameraStart`s handled.
    pub starts: AtomicU64,
    /// Last torch value requested.
    pub torch: AtomicBool,
    /// Last lens (`Lens as i32`) in use.
    pub lens: AtomicI32,
    /// True while the media connection is open and frames are flowing.
    pub streaming: AtomicBool,
    /// Last image settings applied (from `CameraStart.image` or `CameraControl.image`).
    pub image: Mutex<CameraImage>,
    /// Number of `CameraControl`s that carried `image`.
    pub image_controls: AtomicU64,
    /// Number of `CameraReadout`s sent.
    pub readouts: AtomicU64,
}

/// Fake per-lens capabilities: the back lens supports everything, the front one is basic.
pub fn lens_caps() -> Vec<LensCaps> {
    let back = LensCaps {
        lens: Lens::Back as i32,
        focus_manual: true,
        focus_lock: true,
        exposure_manual: true,
        iso_min: 50,
        iso_max: 3200,
        exposure_us_min: 10,
        exposure_us_max: 500_000,
        ev_min: -12,
        ev_max: 12,
        ev_step: 1.0 / 6.0,
        ae_lock: true,
        awb_lock: true,
        white_balances: [
            WhiteBalance::Auto,
            WhiteBalance::Incandescent,
            WhiteBalance::Fluorescent,
            WhiteBalance::WarmFluorescent,
            WhiteBalance::Daylight,
            WhiteBalance::CloudyDaylight,
            WhiteBalance::Twilight,
            WhiteBalance::Shade,
        ]
        .map(|w| w as i32)
        .to_vec(),
        antibandings: [Antibanding::Auto, Antibanding::Off, Antibanding::Antibanding50hz, Antibanding::Antibanding60hz]
            .map(|a| a as i32)
            .to_vec(),
        stabilization: true,
        zoom_max: 8.0,
    };
    let front = LensCaps {
        lens: Lens::Front as i32,
        focus_manual: false,
        focus_lock: false,
        exposure_manual: false,
        iso_min: 0,
        iso_max: 0,
        exposure_us_min: 0,
        exposure_us_max: 0,
        ev_min: -6,
        ev_max: 6,
        ev_step: 1.0 / 3.0,
        ae_lock: true,
        awb_lock: true,
        white_balances: vec![WhiteBalance::Auto as i32, WhiteBalance::Daylight as i32],
        antibandings: vec![Antibanding::Auto as i32, Antibanding::Off as i32],
        stabilization: false,
        zoom_max: 4.0,
    };
    vec![back, front]
}

/// Supported modes advertised in `CameraStatus`: back and front, 1080p/720p/360p at 30 fps.
pub fn supported_modes() -> Vec<CameraMode> {
    let mut v = Vec::new();
    for lens in [Lens::Back, Lens::Front] {
        for (width, height) in [(1920, 1080), (1280, 720), (640, 360)] {
            v.push(CameraMode { lens: lens as i32, width, height, fps: vec![30] });
        }
    }
    v
}

/// The camera side of a [`FakePhone`]. Dropping it stops everything.
pub struct FakeCamera {
    stats: Arc<CameraStats>,
    task: JoinHandle<()>,
}

impl Drop for FakeCamera {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeCamera {
    /// Start serving `CameraStart` / `CameraControl` / `CameraStop` for `phone` and send the
    /// initial `CameraStatus{STOPPED, supported_modes}`.
    pub async fn attach(phone: Arc<FakePhone>) -> Result<Self, FakePhoneError> {
        let rx = phone.subscribe(); // subscribe before announcing: no start can be missed
        let stats = Arc::new(CameraStats::default());
        stats.lens.store(Lens::Back as i32, Ordering::SeqCst);
        send_status(&phone, CameraState::Stopped, None, "").await?;
        let task = tokio::spawn(controller(phone, rx, Arc::clone(&stats)));
        Ok(Self { stats, task })
    }

    pub fn stats(&self) -> Arc<CameraStats> {
        Arc::clone(&self.stats)
    }

    /// Resolves when the controller ended (control connection closed).
    pub async fn wait_ended(&mut self) {
        let _ = (&mut self.task).await;
    }
}

async fn send_status(
    phone: &FakePhone,
    state: CameraState,
    active: Option<CameraStart>,
    error: &str,
) -> Result<(), FakePhoneError> {
    phone
        .send_message(Msg::CameraStatus(CameraStatus {
            state: state as i32,
            active,
            supported_modes: supported_modes(),
            error: error.to_string(),
            lens_caps: lens_caps(),
        }))
        .await
}

/// Shared with the encoder thread.
#[derive(Default)]
struct StreamCtl {
    stop: AtomicBool,
    keyframe: AtomicBool,
    lens: AtomicI32,
}

struct Running {
    ctl: Arc<StreamCtl>,
    task: JoinHandle<()>,
}

impl Running {
    fn stop(self) {
        self.ctl.stop.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

async fn controller(phone: Arc<FakePhone>, mut rx: broadcast::Receiver<Envelope>, stats: Arc<CameraStats>) {
    let mut running: Option<Running> = None;
    loop {
        let env = match rx.recv().await {
            Ok(e) => e,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        match env.msg {
            Some(Msg::CameraStart(start)) => {
                if let Some(r) = running.take() {
                    r.stop();
                }
                stats.starts.fetch_add(1, Ordering::SeqCst);
                *stats.image.lock().unwrap_or_else(|e| e.into_inner()) = start.image.unwrap_or_default();
                let ctl = Arc::new(StreamCtl::default());
                ctl.lens.store(
                    if start.lens == 0 { stats.lens.load(Ordering::SeqCst) } else { start.lens },
                    Ordering::SeqCst,
                );
                stats.lens.store(ctl.lens.load(Ordering::SeqCst), Ordering::SeqCst);
                let task = tokio::spawn(stream(Arc::clone(&phone), start, Arc::clone(&ctl), Arc::clone(&stats)));
                running = Some(Running { ctl, task });
            }
            Some(Msg::CameraControl(c)) => apply_control(&c, running.as_ref(), &stats),
            Some(Msg::CameraStop(_)) => {
                if let Some(r) = running.take() {
                    r.stop();
                }
                stats.streaming.store(false, Ordering::SeqCst);
                let _ = send_status(&phone, CameraState::Stopped, None, "").await;
            }
            _ => {}
        }
    }
    if let Some(r) = running.take() {
        r.stop();
    }
}

fn apply_control(c: &CameraControl, running: Option<&Running>, stats: &CameraStats) {
    if c.request_keyframe {
        if let Some(r) = running {
            r.ctl.keyframe.store(true, Ordering::SeqCst);
        }
    }
    if let Some(l) = c.lens {
        stats.lens.store(l, Ordering::SeqCst);
        if let Some(r) = running {
            r.ctl.lens.store(l, Ordering::SeqCst);
        }
        println!("camera: lens -> {:?}", Lens::try_from(l).unwrap_or(Lens::Unspecified));
    }
    if let Some(img) = &c.image {
        *stats.image.lock().unwrap_or_else(|e| e.into_inner()) = *img;
        stats.image_controls.fetch_add(1, Ordering::SeqCst);
        println!("camera: image -> {img:?}");
    }
    if let Some(t) = c.torch {
        stats.torch.store(t, Ordering::SeqCst);
        println!("camera: torch -> {}", if t { "on" } else { "off" });
    }
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
    let attach = Envelope::new(1, Msg::MediaAttach(MediaAttach { session_id, ..Default::default() }));
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

async fn stream(phone: Arc<FakePhone>, start: CameraStart, ctl: Arc<StreamCtl>, stats: Arc<CameraStats>) {
    let _ = send_status(&phone, CameraState::Starting, Some(start), "").await;
    let Some(session_id) = phone.session_id().await else {
        let _ = send_status(&phone, CameraState::Error, None, "no session").await;
        return;
    };
    let mut tls = match open_media(&phone, session_id).await {
        Ok(t) => t,
        Err(e) => {
            let _ = send_status(&phone, CameraState::Error, None, &e).await;
            return;
        }
    };

    let (tx, mut prx) = mpsc::channel::<MediaPacket>(8);
    let enc_ctl = Arc::clone(&ctl);
    let s2 = start;
    let thread = std::thread::spawn(move || encode_loop(s2, enc_ctl, tx));

    // About once a second while streaming: what the "camera" is doing right now.
    let readouts = {
        let stats = Arc::clone(&stats);
        let phone = Arc::clone(&phone);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                if !stats.streaming.load(Ordering::SeqCst) {
                    continue;
                }
                let img = *stats.image.lock().unwrap_or_else(|e| e.into_inner());
                let manual_exp = img.exposure_mode == ExposureMode::Manual as i32;
                let readout = CameraReadout {
                    iso: if manual_exp && img.iso > 0 { img.iso } else { 400 },
                    exposure_us: if manual_exp && img.exposure_us > 0 { img.exposure_us } else { 16_666 },
                    focus_permille: if img.focus_mode == nd_proto::v1::FocusMode::Manual as i32 {
                        img.focus_permille
                    } else {
                        300
                    },
                    focused: true,
                };
                if phone.send_message(Msg::CameraReadout(readout)).await.is_err() {
                    break;
                }
                stats.readouts.fetch_add(1, Ordering::SeqCst);
            }
        })
    };

    let mut announced = false;
    while let Some(p) = prx.recv().await {
        if timeout(STEP, write_packet(&mut tls, &p)).await.map(|r| r.is_ok()) != Ok(true) {
            break;
        }
        if !p.header.config {
            stats.frames_sent.fetch_add(1, Ordering::SeqCst);
            if p.header.key_frame {
                stats.key_frames_sent.fetch_add(1, Ordering::SeqCst);
            }
        }
        if !announced && !p.header.config {
            announced = true;
            stats.streaming.store(true, Ordering::SeqCst);
            let _ = send_status(&phone, CameraState::Streaming, Some(start), "").await;
        }
    }
    readouts.abort();
    ctl.stop.store(true, Ordering::SeqCst);
    stats.streaming.store(false, Ordering::SeqCst);
    drop(prx);
    let _ = thread.join();
}

fn even(v: u32, default: u32) -> usize {
    let v = if v == 0 { default } else { v };
    (v.clamp(16, 4096) & !1) as usize
}

fn encode_loop(start: CameraStart, ctl: Arc<StreamCtl>, tx: mpsc::Sender<MediaPacket>) {
    let (w, h) = (even(start.width, 640), even(start.height, 360));
    let fps = if start.fps == 0 { 30 } else { start.fps.min(120) };
    let bitrate = if start.bitrate_bps == 0 { 2_000_000 } else { start.bitrate_bps };
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .rate_control_mode(RateControlMode::Bitrate)
        .skip_frames(false)
        .profile(Profile::Baseline)
        .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 2));
    let mut enc = match Encoder::with_api_config(OpenH264API::from_source(), cfg) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("fake camera: encoder init failed: {e}");
            return;
        }
    };
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let t0 = Instant::now();
    let mut sent_config = false;
    let mut n: u64 = 0;
    while !ctl.stop.load(Ordering::SeqCst) {
        let due = t0 + interval * n as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if ctl.keyframe.swap(false, Ordering::SeqCst) {
            enc.force_intra_frame();
        }
        let mut raw = vec![0u8; w * h * 3 / 2];
        render(&mut raw, w, h, n, ctl.lens.load(Ordering::SeqCst));
        let yuv = YUVBuffer::from_vec(raw, w, h);
        let pts = (interval.as_micros() as u64) * n;
        let (bytes, key) = match enc.encode(&yuv) {
            Ok(bs) => (bs.to_vec(), matches!(bs.frame_type(), FrameType::IDR | FrameType::I)),
            Err(e) => {
                tracing::warn!("fake camera: encode failed: {e}");
                break;
            }
        };
        n += 1;
        if bytes.is_empty() {
            continue;
        }
        if !sent_config && key {
            let cfg_bytes = parameter_sets(&bytes);
            if !cfg_bytes.is_empty() {
                if tx.blocking_send(MediaPacket::new(0, true, false, cfg_bytes)).is_err() {
                    return;
                }
                sent_config = true;
            }
        }
        if !sent_config {
            continue; // never send frames before CONFIG
        }
        if tx.blocking_send(MediaPacket::new(pts, false, key, bytes)).is_err() {
            return;
        }
    }
}

/// SPS + PPS NAL units (with 4-byte start codes) found in an Annex-B access unit.
fn parameter_sets(annexb: &[u8]) -> Vec<u8> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map(|n| n - 3).unwrap_or(annexb.len());
        while e > s && annexb[e - 1] == 0 {
            e -= 1;
        }
        if s < annexb.len() && matches!(annexb[s] & 0x1f, 7 | 8) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&annexb[s..e]);
        }
    }
    out
}

// --- pattern -------------------------------------------------------------------------------

/// (Y, U, V) of white, yellow, cyan, green, magenta, red, blue, black.
const BARS: [(u8, u8, u8); 8] = [
    (235, 128, 128),
    (210, 16, 146),
    (170, 166, 16),
    (145, 54, 34),
    (106, 202, 222),
    (81, 90, 240),
    (41, 240, 110),
    (16, 128, 128),
];

const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Render frame `n` as I420 into `buf` (`w*h*3/2` bytes).
fn render(buf: &mut [u8], w: usize, h: usize, n: u64, lens: i32) {
    let (yp, rest) = buf.split_at_mut(w * h);
    let (up, vp) = rest.split_at_mut(w * h / 4);
    let front = lens == Lens::Front as i32;
    let shift = (n as usize * 4) % w;
    let cw = w / 2;
    // One row of bar indices at full resolution, reused for every row.
    let row: Vec<usize> = (0..w).map(|x| (((x + shift) % w) * 8 / w).min(7)).collect();
    for y in 0..h {
        for x in 0..w {
            let (yy, _, _) = BARS[row[x]];
            yp[y * w + x] = if front { 255 - yy.min(239) } else { yy };
        }
    }
    for y in 0..h / 2 {
        for x in 0..cw {
            let (_, u, v) = BARS[row[x * 2]];
            let (u, v) = if front { (v, u) } else { (u, v) };
            up[y * cw + x] = u;
            vp[y * cw + x] = v;
        }
    }
    // Counter box: black background, white digits.
    let cell = (h / 40).max(2) & !1;
    let digits = format!("{:08}", n % 100_000_000);
    let bw = (digits.len() * 4 + 1) * cell;
    let bh = 7 * cell;
    let (bx, by) = (cell, cell);
    fill(yp, up, vp, w, h, bx, by, bw.min(w - bx.min(w)), bh.min(h - by.min(h)), 16, 128, 128);
    for (i, ch) in digits.bytes().enumerate() {
        let g = &DIGITS[(ch - b'0') as usize];
        for (r, bits) in g.iter().enumerate() {
            for c in 0..3 {
                if bits & (0b100 >> c) != 0 {
                    let x = bx + (i * 4 + c + 1) * cell;
                    let y = by + (r + 1) * cell;
                    if x + cell <= w && y + cell <= h {
                        fill(yp, up, vp, w, h, x, y, cell, cell, 235, 128, 128);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill(
    yp: &mut [u8],
    up: &mut [u8],
    vp: &mut [u8],
    w: usize,
    h: usize,
    x: usize,
    y: usize,
    rw: usize,
    rh: usize,
    yv: u8,
    uv: u8,
    vv: u8,
) {
    let (x2, y2) = ((x + rw).min(w), (y + rh).min(h));
    for yy in y..y2 {
        yp[yy * w + x..yy * w + x2].fill(yv);
    }
    let cw = w / 2;
    for yy in y / 2..y2.div_ceil(2).min(h / 2) {
        let (a, b) = (x / 2, x2.div_ceil(2).min(cw));
        up[yy * cw + a..yy * cw + b].fill(uv);
        vp[yy * cw + a..yy * cw + b].fill(vv);
    }
}
