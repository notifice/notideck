// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure webcam decisions: mode and bitrate picking, session choice, and the start/stop state
//! machine. No I/O here, so every transition is unit-testable with a hand-driven clock.

use nd_config::{parse_camera_mode, CameraLens, CameraSettings, MirrorMode};
use nd_proto::v1::{CameraMode, CameraStart, Lens};
use nd_transport::Link;
use std::time::{Duration, Instant};

/// The consumer must be gone this long before the phone camera is stopped (flapping apps).
pub const CONSUMER_GRACE: Duration = Duration::from_secs(5);
/// No frame this long after `CameraStart`: stop and try again.
pub const START_TIMEOUT: Duration = Duration::from_secs(12);
/// Pause before restarting after the media stream died.
pub const RESTART_DELAY: Duration = Duration::from_secs(2);
/// Pause before retrying after the phone or the decoder reported an error.
pub const ERROR_RETRY: Duration = Duration::from_secs(10);
/// A `CameraStatus` older than this after our `CameraStart` is not an answer to it.
const STATUS_SETTLE: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
}

impl Mode {
    pub fn pixels(self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }
}

pub fn lens_proto(l: CameraLens) -> Lens {
    match l {
        CameraLens::Back => Lens::Back,
        CameraLens::Front => Lens::Front,
    }
}

/// All `(size, fps)` combinations the phone offers for `lens` (an empty fps list means 30).
pub fn modes_for(modes: &[CameraMode], lens: CameraLens) -> Vec<Mode> {
    let want = lens_proto(lens) as i32;
    let mut out = Vec::new();
    for m in modes.iter().filter(|m| m.lens == want) {
        let fps: Vec<u32> = if m.fps.is_empty() { vec![30] } else { m.fps.clone() };
        for f in fps {
            let mode = Mode { w: m.width, h: m.height, fps: f };
            if !out.contains(&mode) {
                out.push(mode);
            }
        }
    }
    out
}

fn closest_fps(modes: &[Mode], w: u32, h: u32, target: u32) -> Option<Mode> {
    modes.iter().filter(|m| m.w == w && m.h == h).min_by_key(|m| (m.fps.abs_diff(target), m.fps)).copied()
}

/// The user's fixed mode from the setting (`None` for "auto" or garbage).
pub fn user_mode(setting: &str) -> Option<Mode> {
    parse_camera_mode(setting).map(|(w, h, fps)| Mode { w, h, fps })
}

/// Choose the phone mode. `user` is honoured when the phone offers that size; otherwise "auto":
/// the smallest mode that is at least `want` in both dimensions (else the largest), with the fps
/// closest to 30. With no known modes the phone gets a 720p30 request and decides itself.
pub fn pick_mode(modes: &[Mode], want: (u32, u32), user: Option<Mode>) -> Mode {
    if let Some(u) = user {
        if let Some(m) = closest_fps(modes, u.w, u.h, u.fps) {
            return m;
        }
        if modes.is_empty() {
            return u;
        }
    }
    if modes.is_empty() {
        return Mode { w: 1280, h: 720, fps: 30 };
    }
    let covering = modes.iter().filter(|m| m.w >= want.0 && m.h >= want.1).min_by_key(|m| (m.pixels(), m.w)).copied();
    let size = covering.or_else(|| modes.iter().max_by_key(|m| (m.pixels(), m.w)).copied()).expect("non-empty");
    closest_fps(modes, size.w, size.h, 30).unwrap_or(size)
}

/// Encoder bitrate in bit/s: the setting if set, else 720p 4 Mbit/s (USB) or 2.5 Mbit/s (Wi-Fi)
/// scaled by pixel count.
pub fn pick_bitrate(setting_kbps: u32, mode: Mode, link: Link) -> u32 {
    if setting_kbps > 0 {
        return setting_kbps * 1000;
    }
    let base: f64 = match link {
        Link::Usb => 4_000_000.0,
        Link::Lan => 2_500_000.0,
    };
    let scale = mode.pixels() as f64 / (1280.0 * 720.0);
    (base * scale).clamp(400_000.0, 12_000_000.0).round() as u32
}

pub fn resolve_mirror(m: MirrorMode, lens: CameraLens) -> bool {
    match m {
        MirrorMode::Auto => lens == CameraLens::Front,
        MirrorMode::On => true,
        MirrorMode::Off => false,
    }
}

/// A connected, camera-capable session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub session_id: String,
    pub link: Link,
    pub active: bool,
}

/// Prefer the server's active session, then USB over Wi-Fi, then the first.
pub fn choose_session(cands: &[Candidate]) -> Option<usize> {
    let rank = |c: &Candidate| (!c.active, c.link != Link::Usb);
    (0..cands.len()).min_by_key(|&i| (rank(&cands[i]), i))
}

/// Settings JSON form -> proto `CameraImage` (round 15).
pub fn image_proto(i: &nd_config::CameraImageSettings) -> nd_proto::v1::CameraImage {
    use nd_config as c;
    use nd_proto::v1 as p;
    p::CameraImage {
        focus_mode: match i.focus {
            c::FocusMode::Continuous => p::FocusMode::Continuous,
            c::FocusMode::Locked => p::FocusMode::Locked,
            c::FocusMode::Manual => p::FocusMode::Manual,
        } as i32,
        focus_permille: i.focus_permille,
        exposure_mode: match i.exposure {
            c::ExposureMode::Auto => p::ExposureMode::Auto,
            c::ExposureMode::Manual => p::ExposureMode::Manual,
        } as i32,
        exposure_comp: i.exposure_comp,
        ae_lock: i.ae_lock,
        iso: i.iso,
        exposure_us: i.exposure_us,
        white_balance: match i.white_balance {
            c::WhiteBalance::Auto => p::WhiteBalance::Auto,
            c::WhiteBalance::Incandescent => p::WhiteBalance::Incandescent,
            c::WhiteBalance::Fluorescent => p::WhiteBalance::Fluorescent,
            c::WhiteBalance::WarmFluorescent => p::WhiteBalance::WarmFluorescent,
            c::WhiteBalance::Daylight => p::WhiteBalance::Daylight,
            c::WhiteBalance::CloudyDaylight => p::WhiteBalance::CloudyDaylight,
            c::WhiteBalance::Twilight => p::WhiteBalance::Twilight,
            c::WhiteBalance::Shade => p::WhiteBalance::Shade,
        } as i32,
        awb_lock: i.awb_lock,
        antibanding: match i.antibanding {
            c::Antibanding::Auto => p::Antibanding::Auto,
            c::Antibanding::Off => p::Antibanding::Off,
            c::Antibanding::Hz50 => p::Antibanding::Antibanding50hz,
            c::Antibanding::Hz60 => p::Antibanding::Antibanding60hz,
        } as i32,
        stabilization: i.stabilization,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneCam {
    Unknown,
    Stopped,
    Starting,
    Streaming,
    Throttled,
    Error,
}

/// What the machine knows about the chosen phone.
#[derive(Debug, Clone)]
pub struct PhoneView {
    pub session_id: String,
    pub link: Link,
    pub modes: Vec<CameraMode>,
    pub state: PhoneCam,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// A consumer wants the camera but no camera-capable phone is connected.
    WaitingPhone,
    Starting,
    Streaming,
    Error,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::WaitingPhone => "waiting_phone",
            Phase::Starting => "starting",
            Phase::Streaming => "streaming",
            Phase::Error => "error",
        }
    }
}

/// A started phone stream, remembered to detect when a settings change needs a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub session_id: String,
    pub mode: Mode,
    pub lens: CameraLens,
    pub bitrate_bps: u32,
    pub link: Link,
    pub mirror: bool,
    mode_setting: String,
    bitrate_setting: u32,
    started_at: Instant,
}

pub struct Input<'a> {
    /// Size the consumer (or the test button) wants; `None` = nobody (or paused / auto-start off).
    pub want: Option<(u32, u32)>,
    pub phone: Option<&'a PhoneView>,
    pub settings: &'a CameraSettings,
    /// The media connection of the running stream ended.
    pub media_ended: bool,
    /// At least one frame was decoded from the running stream.
    pub frames_flowing: bool,
    /// The H.264 decoder cannot be created or died.
    pub decoder_error: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Start { session_id: String, start: CameraStart },
    Stop { session_id: String },
    SetLens { session_id: String, lens: Lens },
}

#[derive(Debug)]
pub struct Machine {
    pub phase: Phase,
    pub message: String,
    pub running: Option<Running>,
    gone_since: Option<Instant>,
    retry_at: Option<Instant>,
}

impl Default for Machine {
    fn default() -> Self {
        Self { phase: Phase::Idle, message: String::new(), running: None, gone_since: None, retry_at: None }
    }
}

impl Machine {
    fn stop_effect(&mut self, out: &mut Vec<Effect>) {
        if let Some(r) = self.running.take() {
            out.push(Effect::Stop { session_id: r.session_id });
        }
    }

    /// Advance the machine. Returns the effects for the driver to carry out.
    pub fn step(&mut self, now: Instant, input: &Input) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some((ww, wh)) = input.want else {
            // Nobody wants the camera: keep streaming through the grace period, then stop.
            if self.running.is_some() {
                let since = *self.gone_since.get_or_insert(now);
                if now.duration_since(since) >= CONSUMER_GRACE {
                    self.stop_effect(&mut out);
                    self.gone_since = None;
                    self.phase = Phase::Idle;
                    self.message.clear();
                }
            } else {
                self.gone_since = None;
                self.phase = Phase::Idle;
                self.message.clear();
                self.retry_at = None;
            }
            return out;
        };
        self.gone_since = None;

        let Some(phone) = input.phone else {
            // The phone vanished (or never was): the placeholder shows; restart on reconnect.
            self.running = None;
            self.phase = Phase::WaitingPhone;
            self.message = "Waiting for a phone with a camera to connect.".into();
            return out;
        };

        // Another phone took over (e.g. USB plugged in while on Wi-Fi).
        if self.running.as_ref().is_some_and(|r| r.session_id != phone.session_id) {
            self.stop_effect(&mut out);
        }

        let lens = input.settings.lens;
        let modes = modes_for(&phone.modes, lens);
        let user = user_mode(&input.settings.mode);

        if let Some(r) = self.running.clone() {
            let mut restart = false;
            let want_mirror = resolve_mirror(input.settings.mirror, lens);
            let lens_changed = r.lens != lens;
            if lens_changed {
                // A mode the new lens does not offer, or a different mirror default, needs a restart.
                restart = (!modes.is_empty() && !modes.contains(&r.mode)) || r.mirror != want_mirror;
            }
            let desired = pick_mode(&modes, (ww, wh), user);
            if r.mode_setting != input.settings.mode && desired != r.mode {
                restart = true;
            }
            if user.is_none() && (r.mode.w < ww || r.mode.h < wh) && desired.pixels() > r.mode.pixels() {
                restart = true; // the consumer grew beyond the running mode
            }
            if r.bitrate_setting != input.settings.bitrate_kbps {
                restart = true;
            }
            if !lens_changed && r.mirror != want_mirror {
                restart = true;
            }
            if restart {
                self.stop_effect(&mut out);
                self.retry_at = None;
            } else if lens_changed {
                // Live lens change.
                out.push(Effect::SetLens { session_id: r.session_id.clone(), lens: lens_proto(lens) });
                if let Some(run) = self.running.as_mut() {
                    run.lens = lens;
                }
            }
        }

        if let Some(run) = self.running.clone() {
            // Health of the running stream.
            if let Some(err) = input.decoder_error {
                self.stop_effect(&mut out);
                self.phase = Phase::Error;
                self.message = format!("The H.264 decoder failed: {err}");
                self.retry_at = Some(now + ERROR_RETRY);
            } else if now.duration_since(run.started_at) >= STATUS_SETTLE && phone.state == PhoneCam::Error {
                self.stop_effect(&mut out);
                self.phase = Phase::Error;
                self.message = if phone.error.is_empty() {
                    "The phone reported a camera error.".into()
                } else {
                    phone.error.clone()
                };
                self.retry_at = Some(now + ERROR_RETRY);
            } else if input.media_ended {
                self.stop_effect(&mut out);
                self.phase = Phase::Starting;
                self.message = "The phone stopped streaming; restarting.".into();
                self.retry_at = Some(now + RESTART_DELAY);
            } else if input.frames_flowing {
                self.phase = Phase::Streaming;
                self.message.clear();
            } else if now.duration_since(run.started_at) >= START_TIMEOUT {
                self.stop_effect(&mut out);
                self.phase = Phase::Error;
                self.message = "The phone did not start streaming; retrying.".into();
                self.retry_at = Some(now + RESTART_DELAY);
            } else {
                self.phase = Phase::Starting;
                self.message.clear();
            }
        }

        if self.running.is_none() {
            if self.retry_at.is_some_and(|t| now < t) {
                return out; // keep the Error/Starting phase and message while waiting
            }
            self.retry_at = None;
            if input.decoder_error.is_some() {
                // Decoder gone for good: do not start a stream nobody can show.
                self.phase = Phase::Error;
                self.message = format!("The H.264 decoder is unavailable: {}", input.decoder_error.unwrap_or_default());
                return out;
            }
            let mode = pick_mode(&modes, (ww, wh), user);
            let bitrate_bps = pick_bitrate(input.settings.bitrate_kbps, mode, phone.link);
            let mirror = resolve_mirror(input.settings.mirror, lens);
            out.push(Effect::Start {
                session_id: phone.session_id.clone(),
                start: CameraStart {
                    width: mode.w,
                    height: mode.h,
                    fps: mode.fps,
                    bitrate_bps,
                    lens: lens_proto(lens) as i32,
                    rotation_deg: 0,
                    mirror,
                    image: Some(image_proto(&input.settings.image)),
                },
            });
            self.running = Some(Running {
                session_id: phone.session_id.clone(),
                mode,
                lens,
                bitrate_bps,
                link: phone.link,
                mirror,
                mode_setting: input.settings.mode.clone(),
                bitrate_setting: input.settings.bitrate_kbps,
                started_at: now,
            });
            self.phase = Phase::Starting;
            self.message.clear();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_proto_maps_every_variant() {
        use nd_config as c;
        use nd_proto::v1 as p;
        let d = image_proto(&c::CameraImageSettings::default());
        assert_eq!(
            (d.focus_mode, d.exposure_mode, d.white_balance, d.antibanding),
            (1, 1, 1, 1),
            "defaults are the explicit automatic values"
        );
        for (a, b) in [
            (c::FocusMode::Continuous, p::FocusMode::Continuous),
            (c::FocusMode::Locked, p::FocusMode::Locked),
            (c::FocusMode::Manual, p::FocusMode::Manual),
        ] {
            let i = c::CameraImageSettings { focus: a, ..Default::default() };
            assert_eq!(image_proto(&i).focus_mode, b as i32);
        }
        assert_eq!(p::FocusMode::Manual as i32, 3);
        let i = c::CameraImageSettings { exposure: c::ExposureMode::Manual, ..Default::default() };
        assert_eq!(image_proto(&i).exposure_mode, 2);
        use c::WhiteBalance as W;
        for (n, w) in [
            W::Auto,
            W::Incandescent,
            W::Fluorescent,
            W::WarmFluorescent,
            W::Daylight,
            W::CloudyDaylight,
            W::Twilight,
            W::Shade,
        ]
        .into_iter()
        .enumerate()
        {
            let i = c::CameraImageSettings { white_balance: w, ..Default::default() };
            assert_eq!(image_proto(&i).white_balance, n as i32 + 1);
        }
        for (n, a) in [c::Antibanding::Auto, c::Antibanding::Off, c::Antibanding::Hz50, c::Antibanding::Hz60]
            .into_iter()
            .enumerate()
        {
            let i = c::CameraImageSettings { antibanding: a, ..Default::default() };
            assert_eq!(image_proto(&i).antibanding, n as i32 + 1);
        }
        let i = c::CameraImageSettings {
            focus_permille: 500,
            exposure_comp: -3,
            ae_lock: true,
            iso: 800,
            exposure_us: 8000,
            awb_lock: true,
            stabilization: true,
            ..Default::default()
        };
        let o = image_proto(&i);
        assert_eq!((o.focus_permille, o.exposure_comp, o.iso, o.exposure_us), (500, -3, 800, 8000));
        assert!(o.ae_lock && o.awb_lock && o.stabilization);
    }

    fn m(w: u32, h: u32, fps: u32) -> Mode {
        Mode { w, h, fps }
    }

    fn proto_modes() -> Vec<CameraMode> {
        let mut v = Vec::new();
        for lens in [Lens::Back, Lens::Front] {
            for (w, h) in [(1920, 1080), (1280, 720), (640, 360)] {
                let fps = if lens == Lens::Back && w == 1920 { vec![24, 30, 60] } else { vec![30] };
                v.push(CameraMode { lens: lens as i32, width: w, height: h, fps });
            }
        }
        v
    }

    #[test]
    fn auto_picks_smallest_covering_mode() {
        let modes = modes_for(&proto_modes(), CameraLens::Back);
        assert_eq!(pick_mode(&modes, (640, 360), None), m(640, 360, 30));
        assert_eq!(pick_mode(&modes, (1280, 720), None), m(1280, 720, 30));
        assert_eq!(pick_mode(&modes, (800, 600), None), m(1280, 720, 30));
        assert_eq!(pick_mode(&modes, (1920, 1080), None), m(1920, 1080, 30), "fps closest to 30, not 60");
        assert_eq!(pick_mode(&modes, (3840, 2160), None), m(1920, 1080, 30), "else the largest");
        assert_eq!(pick_mode(&[], (640, 360), None), m(1280, 720, 30));
    }

    #[test]
    fn user_mode_wins_when_offered() {
        let modes = modes_for(&proto_modes(), CameraLens::Back);
        assert_eq!(pick_mode(&modes, (640, 360), Some(m(1920, 1080, 60))), m(1920, 1080, 60));
        assert_eq!(pick_mode(&modes, (640, 360), Some(m(1920, 1080, 50))), m(1920, 1080, 60));
        // Unsupported size falls back to auto.
        assert_eq!(pick_mode(&modes, (640, 360), Some(m(800, 600, 30))), m(640, 360, 30));
        assert_eq!(user_mode("auto"), None);
        assert_eq!(user_mode("1280x720@30"), Some(m(1280, 720, 30)));
    }

    #[test]
    fn modes_are_per_lens() {
        let back = modes_for(&proto_modes(), CameraLens::Back);
        let front = modes_for(&proto_modes(), CameraLens::Front);
        assert_eq!(back.len(), 5);
        assert_eq!(front.len(), 3);
    }

    #[test]
    fn bitrate_auto_scales_with_pixels_and_link() {
        assert_eq!(pick_bitrate(0, m(1280, 720, 30), Link::Usb), 4_000_000);
        assert_eq!(pick_bitrate(0, m(1280, 720, 30), Link::Lan), 2_500_000);
        assert_eq!(pick_bitrate(0, m(1920, 1080, 30), Link::Usb), 9_000_000);
        assert_eq!(pick_bitrate(0, m(640, 360, 30), Link::Usb), 1_000_000);
        assert_eq!(pick_bitrate(0, m(160, 90, 30), Link::Lan), 400_000, "floor");
        assert!(pick_bitrate(0, m(1920, 1080, 30), Link::Usb) <= 12_000_000);
        assert_eq!(pick_bitrate(3000, m(1920, 1080, 30), Link::Lan), 3_000_000);
    }

    #[test]
    fn mirror_auto_is_front_only() {
        assert!(resolve_mirror(MirrorMode::Auto, CameraLens::Front));
        assert!(!resolve_mirror(MirrorMode::Auto, CameraLens::Back));
        assert!(resolve_mirror(MirrorMode::On, CameraLens::Back));
        assert!(!resolve_mirror(MirrorMode::Off, CameraLens::Front));
    }

    #[test]
    fn session_choice_prefers_active_then_usb() {
        let c = |id: &str, link, active| Candidate { session_id: id.into(), link, active };
        assert_eq!(choose_session(&[]), None);
        assert_eq!(choose_session(&[c("a", Link::Lan, false), c("b", Link::Usb, false)]), Some(1));
        assert_eq!(choose_session(&[c("a", Link::Lan, true), c("b", Link::Usb, false)]), Some(0));
        assert_eq!(choose_session(&[c("a", Link::Lan, false), c("b", Link::Lan, false)]), Some(0));
    }

    // ---- state machine

    struct Rig {
        now: Instant,
        mach: Machine,
        settings: CameraSettings,
        phone: Option<PhoneView>,
        want: Option<(u32, u32)>,
        flowing: bool,
        ended: bool,
        dec_err: Option<String>,
    }

    impl Rig {
        fn new() -> Self {
            Rig {
                now: Instant::now(),
                mach: Machine::default(),
                settings: CameraSettings::default(),
                phone: Some(PhoneView {
                    session_id: "s1".into(),
                    link: Link::Usb,
                    modes: proto_modes(),
                    state: PhoneCam::Stopped,
                    error: String::new(),
                }),
                want: None,
                flowing: false,
                ended: false,
                dec_err: None,
            }
        }
        fn step(&mut self, advance_ms: u64) -> Vec<Effect> {
            self.now += Duration::from_millis(advance_ms);
            let input = Input {
                want: self.want,
                phone: self.phone.as_ref(),
                settings: &self.settings,
                media_ended: self.ended,
                frames_flowing: self.flowing,
                decoder_error: self.dec_err.as_deref(),
            };
            self.mach.step(self.now, &input)
        }
    }

    #[test]
    fn consumer_on_starts_frames_flow_consumer_off_stops_after_5s() {
        let mut r = Rig::new();
        assert!(r.step(250).is_empty());
        assert_eq!(r.mach.phase, Phase::Idle);

        r.want = Some((640, 360));
        let fx = r.step(250);
        assert_eq!(fx.len(), 1);
        match &fx[0] {
            Effect::Start { session_id, start } => {
                assert_eq!(session_id, "s1");
                assert_eq!((start.width, start.height, start.fps), (640, 360, 30));
                assert_eq!(start.bitrate_bps, 1_000_000);
                assert_eq!(start.lens, Lens::Back as i32);
                assert!(!start.mirror);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(r.mach.phase, Phase::Starting);
        assert!(r.step(250).is_empty(), "no second start while starting");

        r.flowing = true;
        assert!(r.step(250).is_empty());
        assert_eq!(r.mach.phase, Phase::Streaming);

        // Consumer leaves: still streaming during the grace period.
        r.want = None;
        assert!(r.step(250).is_empty());
        assert!(r.step(4000).is_empty());
        assert_eq!(r.mach.phase, Phase::Streaming);
        // A short flap resets the timer.
        r.want = Some((640, 360));
        assert!(r.step(250).is_empty());
        r.want = None;
        assert!(r.step(250).is_empty());
        assert!(r.step(4000).is_empty());
        let fx = r.step(1000);
        assert_eq!(fx, vec![Effect::Stop { session_id: "s1".into() }]);
        assert_eq!(r.mach.phase, Phase::Idle);
        assert!(r.mach.running.is_none());
    }

    #[test]
    fn waiting_for_phone_then_starts_on_reconnect() {
        let mut r = Rig::new();
        r.phone = None;
        r.want = Some((1280, 720));
        assert!(r.step(250).is_empty());
        assert_eq!(r.mach.phase, Phase::WaitingPhone);
        r.phone = Some(PhoneView {
            session_id: "s2".into(),
            link: Link::Lan,
            modes: proto_modes(),
            state: PhoneCam::Stopped,
            error: String::new(),
        });
        let fx = r.step(250);
        assert!(
            matches!(&fx[..], [Effect::Start { session_id, start }] if session_id == "s2" && start.bitrate_bps == 2_500_000)
        );
    }

    #[test]
    fn phone_disconnect_while_streaming_restarts_on_reconnect() {
        let mut r = Rig::new();
        r.want = Some((1280, 720));
        r.step(250);
        r.flowing = true;
        r.step(250);
        r.phone = None;
        r.flowing = false;
        assert!(r.step(250).is_empty(), "nothing to stop on a gone session");
        assert_eq!(r.mach.phase, Phase::WaitingPhone);
        r.phone = Some(PhoneView { session_id: "s3".into(), ..Rig::new().phone.unwrap() });
        assert!(matches!(&r.step(250)[..], [Effect::Start { session_id, .. }] if session_id == "s3"));
    }

    #[test]
    fn consumer_grows_restarts_with_bigger_mode_but_shrinking_just_rescales() {
        let mut r = Rig::new();
        r.want = Some((640, 360));
        r.step(250);
        r.flowing = true;
        r.step(250);
        r.want = Some((1280, 720));
        let fx = r.step(250);
        assert!(matches!(&fx[..], [Effect::Stop { .. }, Effect::Start { start, .. }] if start.width == 1280));
        r.want = Some((640, 360));
        assert!(r.step(250).is_empty(), "running 720p covers 360p");
    }

    #[test]
    fn settings_changes_apply_live_or_by_restart() {
        let mut r = Rig::new();
        r.want = Some((1280, 720));
        r.step(250);
        r.flowing = true;
        r.step(250);
        // Lens with mirror off: live control, no restart (mode is offered on both lenses).
        r.settings.mirror = MirrorMode::Off;
        r.step(250);
        r.settings.lens = CameraLens::Front;
        let fx = r.step(250);
        assert_eq!(fx, vec![Effect::SetLens { session_id: "s1".into(), lens: Lens::Front }]);
        assert!(r.step(250).is_empty());
        r.settings.mirror = MirrorMode::Auto; // front + auto = mirrored: restart
        let fx = r.step(250);
        assert!(
            matches!(&fx[..], [Effect::Stop { .. }, Effect::Start { start, .. }] if start.mirror && start.lens == Lens::Front as i32)
        );
        // Fixed mode: restart.
        r.settings.mode = "640x360@30".into();
        let fx = r.step(250);
        assert!(
            matches!(&fx[..], [Effect::Stop { .. }, Effect::Start { start, .. }] if start.width == 640 && start.mirror)
        );
        // Bitrate: restart.
        r.flowing = true;
        r.step(250);
        r.settings.bitrate_kbps = 2000;
        let fx = r.step(250);
        assert!(
            matches!(&fx[..], [Effect::Stop { .. }, Effect::Start { start, .. }] if start.bitrate_bps == 2_000_000)
        );
        // Mirror: restart.
        r.step(250);
        r.settings.mirror = MirrorMode::Off;
        let fx = r.step(250);
        assert!(matches!(&fx[..], [Effect::Stop { .. }, Effect::Start { start, .. }] if !start.mirror));
    }

    #[test]
    fn phone_error_retries_after_delay() {
        let mut r = Rig::new();
        r.want = Some((640, 360));
        r.step(250);
        // Stale error right after start is ignored.
        r.phone.as_mut().unwrap().state = PhoneCam::Error;
        r.phone.as_mut().unwrap().error = "camera in use".into();
        assert!(r.step(250).is_empty());
        assert_eq!(r.mach.phase, Phase::Starting);
        let fx = r.step(2000);
        assert_eq!(fx, vec![Effect::Stop { session_id: "s1".into() }]);
        assert_eq!((r.mach.phase, r.mach.message.as_str()), (Phase::Error, "camera in use"));
        assert!(r.step(5000).is_empty());
        assert_eq!(r.mach.phase, Phase::Error);
        r.phone.as_mut().unwrap().state = PhoneCam::Stopped;
        let fx = r.step(6000);
        assert!(matches!(&fx[..], [Effect::Start { .. }]));
    }

    #[test]
    fn start_timeout_and_media_end_and_decoder_error() {
        let mut r = Rig::new();
        r.want = Some((640, 360));
        r.step(250);
        let fx = r.step(12_000);
        assert_eq!(fx, vec![Effect::Stop { session_id: "s1".into() }]);
        assert!(matches!(&r.step(2500)[..], [Effect::Start { .. }]));
        r.flowing = true;
        r.step(250);
        r.ended = true;
        assert_eq!(r.step(250), vec![Effect::Stop { session_id: "s1".into() }]);
        r.ended = false;
        r.flowing = false;
        assert!(r.step(500).is_empty(), "waits RESTART_DELAY");
        assert!(matches!(&r.step(2000)[..], [Effect::Start { .. }]));
        r.dec_err = Some("no mft".into());
        assert_eq!(r.step(250), vec![Effect::Stop { session_id: "s1".into() }]);
        assert_eq!(r.mach.phase, Phase::Error);
        r.step(11_000);
        assert_eq!(r.mach.phase, Phase::Error, "never restarts into a missing decoder");
        assert!(r.mach.running.is_none());
    }

    #[test]
    fn usb_takes_over_from_lan() {
        let mut r = Rig::new();
        r.phone.as_mut().unwrap().link = Link::Lan;
        r.want = Some((640, 360));
        r.step(250);
        r.phone = Some(PhoneView { session_id: "usb".into(), link: Link::Usb, ..r.phone.clone().unwrap() });
        let fx = r.step(250);
        assert!(
            matches!(&fx[..], [Effect::Stop { session_id: old }, Effect::Start { session_id: new, .. }] if old == "s1" && new == "usb")
        );
    }
}
