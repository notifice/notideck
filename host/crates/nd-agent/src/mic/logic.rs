// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure decisions of the mic manager (round 25): the state machine, the jitter / drift
//! controller, gain, level metering, packet validation and the default-device guard.
//! Nothing here touches the OS, the network or a clock (callers pass `Instant`s).

use nd_platform::{AudioEndpoint, DefaultDevices, DeviceFlow, DeviceRole, MicOutputDevice};
use nd_transport::Link;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub const DEFAULT_RATE: u32 = 48_000;
/// Wait this long before sending MicStart again to a phone stuck in NO_PERMISSION / ERROR.
pub const PHONE_RETRY: Duration = Duration::from_secs(10);
/// A phone that says STOPPED this long after our MicStart is told to start again.
pub const STOPPED_GRACE: Duration = Duration::from_secs(3);

// --------------------------------------------------------------------------- state machine

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Off,
    Waiting,
    Starting,
    Live,
    NoOutput,
    NoPermission,
    Error,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Off => "off",
            Phase::Waiting => "waiting",
            Phase::Starting => "starting",
            Phase::Live => "live",
            Phase::NoOutput => "no_output",
            Phase::NoPermission => "no_permission",
            Phase::Error => "error",
        }
    }
}

/// `MicStatus.state` as the manager sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhoneMic {
    #[default]
    Unknown,
    Stopped,
    Starting,
    Streaming,
    Error,
    NoPermission,
}

impl PhoneMic {
    pub fn as_str(self) -> &'static str {
        match self {
            PhoneMic::Unknown | PhoneMic::Stopped => "stopped",
            PhoneMic::Starting => "starting",
            PhoneMic::Streaming => "streaming",
            PhoneMic::Error => "error",
            PhoneMic::NoPermission => "no_permission",
        }
    }
}

/// The phone the manager talks to (newest connected session with `CAPABILITY_MIC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneView {
    pub session_id: String,
    pub link: Link,
    pub state: PhoneMic,
    pub error: String,
    /// `MicStatus.sample_rate`, 0 = unknown.
    pub sample_rate: u32,
}

/// Candidate for the phone choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub session_id: String,
    /// Connection order, larger = connected later.
    pub order: u64,
}

/// Newest connected session wins (same rule as the camera uses for the active session).
pub fn choose_session(cands: &[Candidate]) -> Option<usize> {
    (0..cands.len()).max_by_key(|&i| (cands[i].order, std::cmp::Reverse(i)))
}

/// What the playback side currently holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputStatus {
    /// Nothing open (not tried yet, or closed by us).
    #[default]
    Closed,
    Open {
        rate: u32,
        /// The `mic_device` setting it was opened for (`""` = auto).
        device: String,
    },
    /// Opening failed or the device was lost; the text is for the user.
    Failed(String),
}

pub struct Input<'a> {
    pub now: Instant,
    pub enabled: bool,
    pub device: &'a str,
    pub voice_processing: bool,
    pub phone: Option<&'a PhoneView>,
    /// The device list was read and holds nothing that fits the setting.
    pub devices_missing: bool,
    pub output: &'a OutputStatus,
    /// Audio packets arrived recently and the output is open.
    pub audio_flowing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Open (or reopen) the output at `rate` for `device`.
    OpenOutput {
        device: String,
        rate: u32,
    },
    CloseOutput,
    StartPhone {
        session_id: String,
        voice_processing: bool,
    },
    StopPhone {
        session_id: String,
    },
}

#[derive(Debug, Clone)]
struct Started {
    session_id: String,
    voice_processing: bool,
    at: Instant,
}

#[derive(Debug, Default)]
pub struct Machine {
    pub phase: Phase,
    pub message: String,
    started: Option<Started>,
    /// Which platform wording to use for "no cable".
    pub windows: bool,
}

impl Machine {
    pub fn new(windows: bool) -> Self {
        Machine { windows, ..Default::default() }
    }

    /// Session the phone was told to start on.
    pub fn started_session(&self) -> Option<&str> {
        self.started.as_ref().map(|s| s.session_id.as_str())
    }

    fn set(&mut self, phase: Phase, message: impl Into<String>) {
        self.phase = phase;
        self.message = message.into();
    }

    fn stop_phone(&mut self, fx: &mut Vec<Effect>) {
        if let Some(s) = self.started.take() {
            fx.push(Effect::StopPhone { session_id: s.session_id });
        }
    }

    pub fn no_output_message(&self, detail: Option<&str>) -> String {
        if self.windows {
            match detail {
                Some(d) if !d.is_empty() => format!(
                    "The audio output for the phone microphone is not available ({d}). Install the free VB-Cable \
                     (vb-audio.com/Cable); Notideck notices it within a few seconds."
                ),
                _ => "No virtual audio cable found. Install the free VB-Cable (vb-audio.com/Cable); Notideck \
                      notices it within a few seconds."
                    .into(),
            }
        } else {
            match detail {
                Some(d) if !d.is_empty() => format!("The virtual microphone could not be created ({d})."),
                _ => "The virtual microphone could not be created.".into(),
            }
        }
    }

    pub fn step(&mut self, i: &Input) -> Vec<Effect> {
        let mut fx = Vec::new();
        if !i.enabled {
            self.stop_phone(&mut fx);
            if !matches!(i.output, OutputStatus::Closed) {
                fx.push(Effect::CloseOutput);
            }
            self.set(Phase::Off, "The phone microphone is off.");
            return fx;
        }

        // The output device the setting wants is not there at all.
        if i.devices_missing && !matches!(i.output, OutputStatus::Open { .. }) {
            self.stop_phone(&mut fx);
            let msg = self.no_output_message(None);
            self.set(Phase::NoOutput, msg);
            return fx;
        }

        let Some(phone) = i.phone else {
            // Phone gone: its control session is over, so no MicStop is possible.
            self.started = None;
            if matches!(i.output, OutputStatus::Open { .. }) {
                fx.push(Effect::CloseOutput);
            }
            self.set(Phase::Waiting, "Waiting for a phone with the Notideck app to connect.");
            return fx;
        };

        // A different phone than the one we started: stop the old one.
        if let Some(s) = &self.started {
            if s.session_id != phone.session_id {
                self.stop_phone(&mut fx);
            }
        }

        match i.output {
            OutputStatus::Failed(e) => {
                self.stop_phone(&mut fx);
                let msg = self.no_output_message(Some(e));
                self.set(Phase::NoOutput, msg);
                fx.push(Effect::OpenOutput { device: i.device.to_string(), rate: DEFAULT_RATE });
                return fx;
            }
            OutputStatus::Closed => {
                self.stop_phone(&mut fx);
                self.set(Phase::Starting, "Opening the audio output.");
                fx.push(Effect::OpenOutput { device: i.device.to_string(), rate: DEFAULT_RATE });
                return fx;
            }
            OutputStatus::Open { rate, device } => {
                let phone_rate = phone.sample_rate;
                let streamish = matches!(phone.state, PhoneMic::Starting | PhoneMic::Streaming);
                let want_rate = if streamish && phone_rate >= 8_000 { phone_rate } else { *rate };
                if device != i.device || want_rate != *rate {
                    fx.push(Effect::OpenOutput { device: i.device.to_string(), rate: want_rate });
                    return fx;
                }
            }
        }

        // Output is open for the right device and rate: (re)start the phone when needed.
        let restart = match &self.started {
            None => true,
            Some(s) => {
                s.voice_processing != i.voice_processing
                    || (phone.state == PhoneMic::Stopped && i.now.duration_since(s.at) >= STOPPED_GRACE)
                    || (matches!(phone.state, PhoneMic::NoPermission | PhoneMic::Error)
                        && i.now.duration_since(s.at) >= PHONE_RETRY)
            }
        };
        if restart {
            self.started =
                Some(Started { session_id: phone.session_id.clone(), voice_processing: i.voice_processing, at: i.now });
            fx.push(Effect::StartPhone { session_id: phone.session_id.clone(), voice_processing: i.voice_processing });
        }

        match phone.state {
            PhoneMic::NoPermission => {
                self.set(Phase::NoPermission, "Allow microphone access on the phone.");
            }
            PhoneMic::Error => {
                let m = if phone.error.is_empty() {
                    "The phone could not record audio.".to_string()
                } else {
                    format!("The phone could not record audio: {}", phone.error)
                };
                self.set(Phase::Error, m);
            }
            PhoneMic::Streaming if i.audio_flowing => {
                self.set(Phase::Live, "The phone microphone is live.");
            }
            PhoneMic::Streaming => self.set(Phase::Starting, "Waiting for audio from the phone."),
            PhoneMic::Unknown | PhoneMic::Stopped | PhoneMic::Starting => {
                self.set(Phase::Starting, "Starting the microphone on the phone.");
            }
        }
        fx
    }
}

/// Which device from `devices` the setting selects: `""` = first virtual cable, else the id.
pub fn resolve_device<'a>(devices: &'a [MicOutputDevice], setting: &str) -> Option<&'a MicOutputDevice> {
    if setting.is_empty() {
        devices.iter().find(|d| d.virtual_cable)
    } else {
        devices.iter().find(|d| d.id == setting)
    }
}

// ------------------------------------------------------------------------- packets, gain, level

pub const MAX_PACKET_BYTES: usize = 96_000;

/// Audio payload sanity: s16le mono, non-empty, even, at most ~1 s at 48 kHz.
pub fn validate_packet(len: usize) -> Result<(), String> {
    if len == 0 {
        Err("empty audio packet".into())
    } else if !len.is_multiple_of(2) {
        Err(format!("audio packet of {len} bytes is not a whole number of samples"))
    } else if len > MAX_PACKET_BYTES {
        Err(format!("audio packet of {len} bytes is too large"))
    } else {
        Ok(())
    }
}

pub fn pcm_from_le(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect()
}

pub fn db_to_linear(db: i32) -> f32 {
    10f32.powf(db as f32 / 20.0)
}

/// Multiply by `gain` with i16 saturation.
pub fn apply_gain(samples: &mut [i16], gain: f32) {
    if (gain - 1.0).abs() < 1e-6 {
        return;
    }
    for s in samples {
        *s = (f32::from(*s) * gain).round().clamp(-32768.0, 32767.0) as i16;
    }
}

pub fn peak(samples: &[i16]) -> f32 {
    let m = samples.iter().map(|s| i32::from(*s).unsigned_abs()).max().unwrap_or(0);
    (m as f32 / 32768.0).min(1.0)
}

/// Peak of what was written during the last ~100 ms.
#[derive(Debug, Default)]
pub struct LevelMeter {
    blocks: VecDeque<(Instant, f32)>,
}

pub const LEVEL_WINDOW: Duration = Duration::from_millis(100);

impl LevelMeter {
    pub fn feed(&mut self, now: Instant, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        self.blocks.push_back((now, peak(samples)));
        self.expire(now);
    }
    fn expire(&mut self, now: Instant) {
        while self.blocks.front().is_some_and(|(t, _)| now.duration_since(*t) > LEVEL_WINDOW) {
            self.blocks.pop_front();
        }
    }
    pub fn level(&mut self, now: Instant) -> f32 {
        self.expire(now);
        self.blocks.iter().map(|(_, p)| *p).fold(0.0, f32::max)
    }
}

// ----------------------------------------------------------------------------------- jitter

pub const PREBUFFER_MS: u32 = 40;
pub const TARGET_MS: u32 = 60;
pub const MAX_MS: u32 = 150;
pub const STARVE_MS: u64 = 40;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterCounters {
    pub underruns: u64,
    pub dropped_ms: u64,
}

/// Keeps the device queue near [`TARGET_MS`]. `push` packets as they arrive; call `plan` often with
/// the device's current queue length and write what it returns.
#[derive(Debug)]
pub struct Jitter {
    rate: u32,
    target_ms: u32,
    prebuffer_ms: u32,
    max_ms: u32,
    queue: VecDeque<i16>,
    prebuffering: bool,
    had_data: bool,
    starving: bool,
    last_packet: Option<Instant>,
    pub counters: JitterCounters,
}

impl Jitter {
    pub fn new(rate: u32) -> Self {
        Self::with_target(rate, TARGET_MS)
    }

    /// Keep `target_ms` queued in the device; prebuffer `min(40, target)`, drop above `target + 90`.
    pub fn with_target(rate: u32, target_ms: u32) -> Self {
        Jitter {
            rate: rate.max(1),
            target_ms,
            prebuffer_ms: PREBUFFER_MS.min(target_ms),
            max_ms: target_ms + (MAX_MS - TARGET_MS),
            queue: VecDeque::new(),
            prebuffering: true,
            had_data: false,
            starving: false,
            last_packet: None,
            counters: JitterCounters::default(),
        }
    }

    fn samples(&self, ms: u32) -> usize {
        (self.rate as u64 * ms as u64 / 1000) as usize
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn push(&mut self, now: Instant, samples: &[i16]) {
        self.queue.extend(samples.iter().copied());
        self.last_packet = Some(now);
    }

    /// Count `n` samples the device refused as dropped.
    pub fn note_dropped(&mut self, n: usize) {
        self.counters.dropped_ms += (n as u64 * 1000) / self.rate as u64;
    }

    /// Milliseconds buffered in the device plus here.
    pub fn buffer_ms(&self, device_queued: usize) -> u32 {
        ((device_queued + self.queue.len()) as u64 * 1000 / self.rate as u64) as u32
    }

    /// Samples to write to the device now (empty = nothing).
    pub fn plan(&mut self, now: Instant, device_queued: usize) -> Vec<i16> {
        let target = self.samples(self.target_ms);
        // Too much buffered overall: drop the oldest back to the target.
        if device_queued + self.queue.len() > self.samples(self.max_ms) {
            let excess = (device_queued + self.queue.len()).saturating_sub(target).min(self.queue.len());
            self.queue.drain(..excess);
            self.counters.dropped_ms += (excess as u64 * 1000) / self.rate as u64;
        }
        let need = target.saturating_sub(device_queued);
        if need == 0 {
            return Vec::new();
        }
        let starved = self.last_packet.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(STARVE_MS));
        if self.prebuffering
            && (self.queue.len() >= self.samples(self.prebuffer_ms) || (starved && !self.queue.is_empty()))
        {
            self.prebuffering = false;
        }
        if self.prebuffering {
            if starved && self.had_data {
                // Still nothing from the phone: keep the device fed (same underrun episode).
                return vec![0; need];
            }
            return Vec::new();
        }
        let n = need.min(self.queue.len());
        let mut out: Vec<i16> = self.queue.drain(..n).collect();
        if n > 0 {
            self.had_data = true;
            self.starving = false;
        }
        if out.len() < need && starved && self.had_data && self.queue.is_empty() {
            // Nothing more is coming: silence now, and rebuild the prebuffer before real audio resumes.
            if !self.starving {
                self.counters.underruns += 1;
                self.starving = true;
            }
            self.prebuffering = true;
            out.resize(need, 0);
        }
        out
    }
}

// ------------------------------------------------------------------------------------ guard

pub const MAX_RESTORED: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Render,
    RenderComm,
    Capture,
    CaptureComm,
}

impl Slot {
    pub const ALL: [Slot; 4] = [Slot::Render, Slot::RenderComm, Slot::Capture, Slot::CaptureComm];

    pub fn flow(self) -> DeviceFlow {
        match self {
            Slot::Render | Slot::RenderComm => DeviceFlow::Render,
            Slot::Capture | Slot::CaptureComm => DeviceFlow::Capture,
        }
    }
    pub fn role(self) -> DeviceRole {
        match self {
            Slot::Render | Slot::Capture => DeviceRole::Console,
            Slot::RenderComm | Slot::CaptureComm => DeviceRole::Communications,
        }
    }
    pub fn flow_str(self) -> &'static str {
        match self.flow() {
            DeviceFlow::Render => "render",
            DeviceFlow::Capture => "capture",
        }
    }
    pub fn role_str(self) -> &'static str {
        match self.role() {
            DeviceRole::Console => "console",
            DeviceRole::Communications => "comm",
        }
    }
    fn noun(self) -> &'static str {
        match self.flow() {
            DeviceFlow::Render => "playback device",
            DeviceFlow::Capture => "microphone",
        }
    }
    fn default_of(self, d: &DefaultDevices) -> &str {
        match self {
            Slot::Render => &d.render,
            Slot::RenderComm => &d.render_comm,
            Slot::Capture => &d.capture,
            Slot::CaptureComm => &d.capture_comm,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub id: String,
    pub name: String,
}

/// The non-cable default per slot, persisted in `mic_defaults.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardMemory {
    #[serde(flatten)]
    pub slots: HashMap<Slot, Remembered>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardAction {
    Remember { slot: Slot, device: Remembered },
    Restore { slot: Slot, id: String, from: String, to: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardDecision {
    pub actions: Vec<GuardAction>,
    /// Current warnings (a cable is the default and nothing is remembered to put back).
    pub warnings: Vec<String>,
}

fn find_endpoint<'a>(endpoints: &'a [AudioEndpoint], slot: Slot, key: &str) -> Option<&'a AudioEndpoint> {
    if key.is_empty() {
        return None;
    }
    endpoints.iter().find(|e| e.flow == slot.flow() && (e.id == key || e.name == key))
}

/// Decide what the default-device guard does for one reading of the defaults.
pub fn guard_decide(memory: &GuardMemory, defaults: &DefaultDevices, endpoints: &[AudioEndpoint]) -> GuardDecision {
    let mut out = GuardDecision::default();
    for slot in Slot::ALL {
        let Some(cur) = find_endpoint(endpoints, slot, slot.default_of(defaults)) else { continue };
        if !cur.virtual_cable {
            let r = Remembered { id: cur.id.clone(), name: cur.name.clone() };
            if memory.slots.get(&slot) != Some(&r) {
                out.actions.push(GuardAction::Remember { slot, device: r });
            }
            continue;
        }
        let back = memory
            .slots
            .get(&slot)
            .and_then(|m| endpoints.iter().find(|e| e.flow == slot.flow() && e.id == m.id && !e.virtual_cable));
        match back {
            Some(b) => out.actions.push(GuardAction::Restore {
                slot,
                id: b.id.clone(),
                from: cur.name.clone(),
                to: b.name.clone(),
            }),
            None => out.warnings.push(format!(
                "Windows uses {} as the default {} and Notideck does not know which device to put back. Pick \
                 your {} in the Windows sound settings.",
                cur.name,
                slot.noun(),
                slot.noun()
            )),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Restored {
    pub flow: &'static str,
    pub role: &'static str,
    pub from: String,
    pub to: String,
    pub at: u64,
}

/// Append keeping the last [`MAX_RESTORED`].
pub fn push_restored(log: &mut Vec<Restored>, r: Restored) {
    log.push(r);
    if log.len() > MAX_RESTORED {
        let extra = log.len() - MAX_RESTORED;
        log.drain(..extra);
    }
}

// ------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(state: PhoneMic) -> PhoneView {
        PhoneView { session_id: "s1".into(), link: Link::Usb, state, error: String::new(), sample_rate: 0 }
    }

    struct Rig {
        now: Instant,
        m: Machine,
        enabled: bool,
        device: String,
        vp: bool,
        phone: Option<PhoneView>,
        missing: bool,
        out: OutputStatus,
        flowing: bool,
    }

    impl Rig {
        fn new() -> Self {
            Rig {
                now: Instant::now(),
                m: Machine::new(true),
                enabled: true,
                device: String::new(),
                vp: true,
                phone: Some(phone(PhoneMic::Stopped)),
                missing: false,
                out: OutputStatus::Closed,
                flowing: false,
            }
        }
        fn step(&mut self, ms: u64) -> Vec<Effect> {
            self.now += Duration::from_millis(ms);
            let i = Input {
                now: self.now,
                enabled: self.enabled,
                device: &self.device,
                voice_processing: self.vp,
                phone: self.phone.as_ref(),
                devices_missing: self.missing,
                output: &self.out,
                audio_flowing: self.flowing,
            };
            self.m.step(&i)
        }
        fn open(&mut self, rate: u32) {
            self.out = OutputStatus::Open { rate, device: self.device.clone() };
        }
    }

    #[test]
    fn off_when_disabled_and_stops_phone() {
        let mut r = Rig::new();
        r.enabled = false;
        assert!(r.step(10).is_empty());
        assert_eq!(r.m.phase, Phase::Off);
        r.enabled = true;
        r.open(48000);
        assert!(matches!(r.step(10)[..], [Effect::StartPhone { .. }]));
        r.enabled = false;
        let fx = r.step(10);
        assert!(fx.contains(&Effect::StopPhone { session_id: "s1".into() }));
        assert!(fx.contains(&Effect::CloseOutput));
        assert_eq!(r.m.phase, Phase::Off);
    }

    #[test]
    fn waiting_without_phone() {
        let mut r = Rig::new();
        r.phone = None;
        assert!(r.step(10).is_empty());
        assert_eq!(r.m.phase, Phase::Waiting);
    }

    #[test]
    fn opens_output_before_starting_phone() {
        let mut r = Rig::new();
        let fx = r.step(10);
        assert_eq!(fx, vec![Effect::OpenOutput { device: "".into(), rate: 48000 }]);
        assert_eq!(r.m.phase, Phase::Starting);
        r.open(48000);
        let fx = r.step(10);
        assert_eq!(fx, vec![Effect::StartPhone { session_id: "s1".into(), voice_processing: true }]);
        // Not repeated.
        assert!(r.step(10).is_empty());
    }

    #[test]
    fn failed_output_is_no_output_and_phone_not_started() {
        let mut r = Rig::new();
        r.out = OutputStatus::Failed("no virtual cable found".into());
        let fx = r.step(10);
        assert_eq!(r.m.phase, Phase::NoOutput);
        assert!(r.m.message.contains("VB-Cable"));
        assert!(!fx.iter().any(|e| matches!(e, Effect::StartPhone { .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::OpenOutput { .. })));
    }

    #[test]
    fn output_loss_stops_the_phone() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.out = OutputStatus::Failed("lost".into());
        let fx = r.step(10);
        assert!(fx.contains(&Effect::StopPhone { session_id: "s1".into() }));
        assert_eq!(r.m.phase, Phase::NoOutput);
        // Reopened later: the phone is started again.
        r.open(48000);
        assert!(matches!(r.step(10)[..], [Effect::StartPhone { .. }]));
    }

    #[test]
    fn missing_devices_is_no_output_without_open_attempt() {
        let mut r = Rig::new();
        r.missing = true;
        assert!(r.step(10).is_empty());
        assert_eq!(r.m.phase, Phase::NoOutput);
    }

    #[test]
    fn linux_message_has_no_cable_text() {
        let mut r = Rig::new();
        r.m.windows = false;
        r.out = OutputStatus::Failed("pactl missing".into());
        r.step(10);
        assert!(!r.m.message.contains("VB-Cable"));
        assert!(r.m.message.contains("pactl missing"));
    }

    #[test]
    fn live_only_with_audio() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.phone = Some(PhoneView { sample_rate: 48000, ..phone(PhoneMic::Streaming) });
        r.step(10);
        assert_eq!(r.m.phase, Phase::Starting);
        r.flowing = true;
        r.step(10);
        assert_eq!(r.m.phase, Phase::Live);
        r.flowing = false;
        r.step(10);
        assert_eq!(r.m.phase, Phase::Starting);
    }

    #[test]
    fn no_permission_and_error_map_and_retry() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.phone = Some(phone(PhoneMic::NoPermission));
        assert!(r.step(10).is_empty());
        assert_eq!(r.m.phase, Phase::NoPermission);
        assert!(r.m.message.contains("Allow microphone"));
        // Retry after the pause.
        let fx = r.step(PHONE_RETRY.as_millis() as u64 + 1);
        assert!(matches!(fx[..], [Effect::StartPhone { .. }]));
        r.phone = Some(PhoneView { error: "busy".into(), ..phone(PhoneMic::Error) });
        r.step(10);
        assert_eq!(r.m.phase, Phase::Error);
        assert!(r.m.message.contains("busy"));
    }

    #[test]
    fn rate_change_reopens_output() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.phone = Some(PhoneView { sample_rate: 44100, ..phone(PhoneMic::Starting) });
        let fx = r.step(10);
        assert_eq!(fx, vec![Effect::OpenOutput { device: "".into(), rate: 44100 }]);
        r.open(44100);
        assert!(r.step(10).is_empty());
    }

    #[test]
    fn setting_changes_reopen_or_restart() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.device = "dev2".into();
        assert_eq!(r.step(10), vec![Effect::OpenOutput { device: "dev2".into(), rate: 48000 }]);
        r.open(48000);
        assert!(r.step(10).is_empty());
        r.vp = false;
        assert_eq!(r.step(10), vec![Effect::StartPhone { session_id: "s1".into(), voice_processing: false }]);
    }

    #[test]
    fn stopped_after_grace_restarts() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        assert!(r.step(500).is_empty());
        let fx = r.step(STOPPED_GRACE.as_millis() as u64);
        assert!(matches!(fx[..], [Effect::StartPhone { .. }]));
    }

    #[test]
    fn new_phone_replaces_old() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.phone = Some(PhoneView { session_id: "s2".into(), ..phone(PhoneMic::Stopped) });
        let fx = r.step(10);
        assert_eq!(
            fx,
            vec![
                Effect::StopPhone { session_id: "s1".into() },
                Effect::StartPhone { session_id: "s2".into(), voice_processing: true }
            ]
        );
    }

    #[test]
    fn phone_gone_forgets_and_closes() {
        let mut r = Rig::new();
        r.open(48000);
        r.step(10);
        r.phone = None;
        let fx = r.step(10);
        assert_eq!(fx, vec![Effect::CloseOutput]);
        assert_eq!(r.m.started_session(), None);
    }

    #[test]
    fn session_choice_is_newest() {
        let c = |id: &str, order| Candidate { session_id: id.into(), order };
        assert_eq!(choose_session(&[]), None);
        assert_eq!(choose_session(&[c("a", 1), c("b", 3), c("c", 2)]), Some(1));
        assert_eq!(choose_session(&[c("a", 1), c("b", 1)]), Some(0));
    }

    #[test]
    fn device_resolution() {
        let d = |id: &str, cable| MicOutputDevice {
            id: id.into(),
            name: id.into(),
            virtual_cable: cable,
            capture_name: String::new(),
        };
        let list = vec![d("spk", false), d("cable", true)];
        assert_eq!(resolve_device(&list, "").unwrap().id, "cable");
        assert_eq!(resolve_device(&list, "spk").unwrap().id, "spk");
        assert!(resolve_device(&list, "nope").is_none());
        assert!(resolve_device(&[d("spk", false)], "").is_none());
    }

    // ---- packets, gain, level

    #[test]
    fn packet_validation() {
        assert!(validate_packet(1920).is_ok());
        assert!(validate_packet(0).is_err());
        assert!(validate_packet(3).is_err());
        assert!(validate_packet(MAX_PACKET_BYTES + 2).is_err());
        assert_eq!(pcm_from_le(&[1, 0, 0xff, 0xff, 9]), vec![1, -1]);
    }

    #[test]
    fn gain_saturates() {
        assert!((db_to_linear(0) - 1.0).abs() < 1e-6);
        assert!((db_to_linear(6) - 1.995).abs() < 0.01);
        assert!((db_to_linear(-20) - 0.1).abs() < 1e-3);
        let mut s = [1000i16, -1000, 30000, -30000];
        apply_gain(&mut s, db_to_linear(12));
        assert_eq!(s[0], 3981);
        assert_eq!(s[2], 32767);
        assert_eq!(s[3], -32768);
        let mut u = [5i16, -5];
        apply_gain(&mut u, 1.0);
        assert_eq!(u, [5, -5]);
    }

    #[test]
    fn level_window() {
        let mut m = LevelMeter::default();
        let t = Instant::now();
        assert_eq!(m.level(t), 0.0);
        m.feed(t, &[0, 16384, -100]);
        assert!((m.level(t + Duration::from_millis(50)) - 0.5).abs() < 1e-4);
        m.feed(t + Duration::from_millis(60), &[100]);
        assert!(m.level(t + Duration::from_millis(150)) < 0.01);
        assert_eq!(m.level(t + Duration::from_millis(400)), 0.0);
        assert_eq!(peak(&[-32768]), 1.0);
    }

    // ---- jitter

    const R: u32 = 48000;
    fn ms(n: u32) -> usize {
        (R * n / 1000) as usize
    }

    #[test]
    fn waits_for_prebuffer() {
        let mut j = Jitter::new(R);
        let t = Instant::now();
        j.push(t, &vec![1; ms(20)]);
        assert!(j.plan(t, 0).is_empty());
        j.push(t, &vec![1; ms(20)]);
        let out = j.plan(t, 0);
        assert_eq!(out.len(), ms(40)); // all that is there, up to the 60 ms target
        assert!(out.iter().all(|&s| s == 1));
    }

    #[test]
    fn short_target_keeps_30_ms() {
        let mut j = Jitter::with_target(R, 30);
        let t = Instant::now();
        j.push(t, &vec![1; ms(100)]);
        // Prebuffer is min(40, 30) = 30 ms; the device is topped up to 30 ms.
        assert_eq!(j.plan(t, 0).len(), ms(30));
        assert_eq!(j.plan(t, ms(20)).len(), ms(10));
        assert!(j.plan(t, ms(30)).is_empty());
        // Max is 30 + 90 = 120 ms: 30 in the device + 100 queued is dropped back to a 30 ms total.
        let mut j = Jitter::with_target(R, 30);
        j.push(t, &vec![1; ms(100)]);
        assert!(j.plan(t, ms(30)).is_empty());
        assert_eq!(j.counters.dropped_ms, 100);
    }

    #[test]
    fn keeps_device_queue_near_target() {
        let mut j = Jitter::new(R);
        let t = Instant::now();
        j.push(t, &vec![1; ms(100)]);
        // Device already has 50 ms: only 10 ms more.
        assert_eq!(j.plan(t, ms(50)).len(), ms(10));
        // Device full: nothing.
        assert!(j.plan(t, ms(60)).is_empty());
    }

    #[test]
    fn drops_oldest_above_max() {
        let mut j = Jitter::new(R);
        let t = Instant::now();
        let mut data: Vec<i16> = vec![1; ms(200)];
        data.extend(vec![2; ms(20)]);
        j.push(t, &data);
        // device 60 + queue 220 > 150: drop down to a 60 ms total, i.e. 220 ms dropped; nothing left to write
        let out = j.plan(t, ms(60));
        assert!(out.is_empty());
        assert_eq!(j.counters.dropped_ms, 220);
        assert_eq!(j.queued(), 0);
        // With an empty device queue the newest samples survive.
        let mut j = Jitter::new(R);
        j.push(t, &data);
        let out = j.plan(t, 0);
        assert_eq!(out.len(), ms(60));
        assert_eq!(*out.last().unwrap(), 2);
        assert_eq!(j.counters.dropped_ms, 160);
    }

    #[test]
    fn silence_after_starvation_and_underrun_counted_once() {
        let mut j = Jitter::new(R);
        let t = Instant::now();
        j.push(t, &vec![5; ms(40)]);
        assert_eq!(j.plan(t, 0).len(), ms(40));
        // 10 ms later nothing new: no silence yet (packets may be late).
        assert!(j.plan(t + Duration::from_millis(10), ms(30)).is_empty());
        assert_eq!(j.counters.underruns, 0);
        let later = t + Duration::from_millis(STARVE_MS + 5);
        let out = j.plan(later, ms(10));
        assert_eq!(out.len(), ms(50));
        assert!(out.iter().all(|&s| s == 0));
        assert_eq!(j.counters.underruns, 1);
        // Still starved: more silence, but the same underrun.
        let out = j.plan(later + Duration::from_millis(20), ms(10));
        assert!(out.iter().all(|&s| s == 0));
        assert_eq!(j.counters.underruns, 1);
        // Audio returns: held until the prebuffer is full again, then real samples flow.
        let back = later + Duration::from_millis(30);
        j.push(back, &vec![7; ms(20)]);
        assert!(j.plan(back, ms(59)).is_empty());
        j.push(back, &vec![7; ms(20)]);
        let out = j.plan(back, ms(20));
        assert_eq!(out.len(), ms(40));
        assert!(out.iter().all(|&s| s == 7));
    }

    #[test]
    fn no_underrun_before_first_data() {
        let mut j = Jitter::new(R);
        let t = Instant::now();
        assert!(j.plan(t + Duration::from_secs(1), 0).is_empty());
        assert_eq!(j.counters.underruns, 0);
    }

    #[test]
    fn refused_samples_count_as_dropped() {
        let mut j = Jitter::new(R);
        j.note_dropped(ms(20));
        assert_eq!(j.counters.dropped_ms, 20);
        assert_eq!(j.buffer_ms(ms(30)), 30);
    }

    // ---- guard

    fn ep(id: &str, name: &str, flow: DeviceFlow, cable: bool) -> AudioEndpoint {
        AudioEndpoint { id: id.into(), name: name.into(), flow, virtual_cable: cable }
    }

    fn eps() -> Vec<AudioEndpoint> {
        vec![
            ep("spk", "Speakers", DeviceFlow::Render, false),
            ep("cin", "CABLE Input", DeviceFlow::Render, true),
            ep("mic", "Headset Mic", DeviceFlow::Capture, false),
            ep("cout", "CABLE Output", DeviceFlow::Capture, true),
        ]
    }

    fn defs(r: &str, rc: &str, c: &str, cc: &str) -> DefaultDevices {
        DefaultDevices { render: r.into(), render_comm: rc.into(), capture: c.into(), capture_comm: cc.into() }
    }

    #[test]
    fn guard_remembers_non_cable_defaults() {
        let d = guard_decide(&GuardMemory::default(), &defs("spk", "spk", "mic", "mic"), &eps());
        assert_eq!(d.actions.len(), 4);
        assert!(d.warnings.is_empty());
        let mut mem = GuardMemory::default();
        for a in &d.actions {
            if let GuardAction::Remember { slot, device } = a {
                mem.slots.insert(*slot, device.clone());
            }
        }
        // Known already: nothing to do.
        assert!(guard_decide(&mem, &defs("spk", "spk", "mic", "mic"), &eps()).actions.is_empty());
        // Names work as keys too.
        assert!(guard_decide(&mem, &defs("Speakers", "Speakers", "Headset Mic", "Headset Mic"), &eps())
            .actions
            .is_empty());
    }

    #[test]
    fn guard_restores_when_default_became_a_cable() {
        let mut mem = GuardMemory::default();
        mem.slots.insert(Slot::Render, Remembered { id: "spk".into(), name: "Speakers".into() });
        mem.slots.insert(Slot::Capture, Remembered { id: "mic".into(), name: "Headset Mic".into() });
        let d = guard_decide(&mem, &defs("cin", "spk", "cout", "mic"), &eps());
        assert!(d.warnings.is_empty());
        let restores: Vec<_> = d.actions.iter().filter(|a| matches!(a, GuardAction::Restore { .. })).collect();
        assert_eq!(restores.len(), 2);
        assert_eq!(
            restores[0],
            &GuardAction::Restore {
                slot: Slot::Render,
                id: "spk".into(),
                from: "CABLE Input".into(),
                to: "Speakers".into()
            }
        );
    }

    #[test]
    fn guard_warns_when_nothing_remembered_or_gone() {
        let d = guard_decide(&GuardMemory::default(), &defs("cin", "", "", ""), &eps());
        assert!(d.actions.is_empty());
        assert_eq!(d.warnings.len(), 1);
        assert!(d.warnings[0].contains("CABLE Input"));
        // Remembered endpoint no longer active.
        let mut mem = GuardMemory::default();
        mem.slots.insert(Slot::Render, Remembered { id: "gone".into(), name: "Old".into() });
        let d = guard_decide(&mem, &defs("cin", "", "", ""), &eps());
        assert!(d.actions.is_empty());
        assert_eq!(d.warnings.len(), 1);
    }

    #[test]
    fn restored_log_keeps_last_ten() {
        let mut log = Vec::new();
        for i in 0..14 {
            push_restored(
                &mut log,
                Restored { flow: "render", role: "console", from: "a".into(), to: "b".into(), at: i },
            );
        }
        assert_eq!(log.len(), 10);
        assert_eq!(log[0].at, 4);
        assert_eq!(log[9].at, 13);
    }

    #[test]
    fn memory_roundtrips_as_json() {
        let mut mem = GuardMemory::default();
        mem.slots.insert(Slot::RenderComm, Remembered { id: "x".into(), name: "X".into() });
        let s = serde_json::to_string(&mem).unwrap();
        assert!(s.contains("render_comm"));
        assert_eq!(serde_json::from_str::<GuardMemory>(&s).unwrap(), mem);
    }
}
