// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone microphone output (round 25, SPEC §14): where the agent plays the phone's mic so other
//! apps (Discord, OBS, Zoom) can record it.
//!
//! * Windows: a render endpoint of a virtual cable (VB-Cable "CABLE Input"); apps record the
//!   cable's capture side ("CABLE Output"). Shared-mode WASAPI with automatic format conversion.
//! * Linux: a "Notideck Microphone" virtual source (`module-pipe-source` via `pactl`, works on
//!   PulseAudio and pipewire-pulse) fed through a FIFO.
//!
//! Samples are always mono `i16` at the rate given to [`MicSink::open`]; the backend converts.
//! The agent owns jitter and drift handling: it keeps [`MicStream::queued`] near a target.

use crate::Result;
use serde::Serialize;

/// One place the phone mic can be played into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MicOutputDevice {
    /// Stable id (Windows: endpoint id string; Linux: the source name `notideck_mic`).
    pub id: String,
    /// Shown in the UI, e.g. `CABLE Input (VB-Audio Virtual Cable)`.
    pub name: String,
    /// A known virtual cable (VB-Cable, VB-Cable A/B, VAC "Line N") or the Linux virtual source.
    /// `""` (auto) picks the first such device.
    pub virtual_cable: bool,
    /// What other apps must select as their microphone to hear the phone, e.g. `CABLE Output
    /// (VB-Audio Virtual Cable)` or `Notideck Microphone`. Empty when unknown.
    pub capture_name: String,
}

/// An open output. Dropping it stops playback and frees the device (Linux keeps the virtual
/// source loaded until [`MicSink::shutdown`], so apps don't lose their selected input).
pub trait MicStream: Send {
    /// Queue mono samples for playback. Never blocks for long: when the device buffer is full the
    /// backend drops what doesn't fit and returns how many samples it accepted.
    fn write(&mut self, samples: &[i16]) -> Result<usize>;
    /// Samples queued in the device but not yet played, at the stream's rate (for drift control).
    fn queued(&self) -> Result<usize>;
    /// How much the agent keeps queued in this device (ms). WASAPI keeps the round-25 60 ms; the
    /// Linux PipeWire stream asks for less because its pipe is drained every graph cycle.
    fn target_ms(&self) -> u32 {
        60
    }
    /// The device went away (unplugged, driver removed, endpoint disabled). The agent reopens.
    fn lost(&self) -> bool {
        false
    }
}

/// Default playback/recording endpoints, by role (Windows; empty strings on Linux).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DefaultDevices {
    /// eRender / eConsole ("Default Device" in the Sound panel).
    pub render: String,
    /// eRender / eCommunications ("Default Communication Device").
    pub render_comm: String,
    /// eCapture / eConsole.
    pub capture: String,
    /// eCapture / eCommunications.
    pub capture_comm: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceFlow {
    Render,
    Capture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    /// eConsole (also set eMultimedia together with it).
    Console,
    Communications,
}

/// An active endpoint, for the default-device guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioEndpoint {
    pub id: String,
    pub name: String,
    pub flow: DeviceFlow,
    /// Belongs to a known virtual cable (either side).
    pub virtual_cable: bool,
}

pub trait MicSink: Send + Sync {
    fn backend_name(&self) -> &'static str {
        "none"
    }
    /// Where the mic can go. Windows: every active render endpoint (cables first). Linux: the
    /// virtual source (created on demand, so it is listed even before the first `open`).
    fn devices(&self) -> Result<Vec<MicOutputDevice>>;
    /// Open `device` (an id from [`MicSink::devices`]; `""` = auto). `Err` when nothing fits
    /// (Windows without a virtual cable: `PlatformError::Other("no virtual cable found")`).
    fn open(&self, device: &str, sample_rate: u32) -> Result<Box<dyn MicStream>>;
    /// Current defaults. `Unsupported` where there is nothing to guard (Linux).
    fn defaults(&self) -> Result<DefaultDevices> {
        Err(crate::PlatformError::Unsupported("default audio devices"))
    }
    /// Active endpoints of both flows (for the guard to pick a replacement and to name things).
    fn endpoints(&self) -> Result<Vec<AudioEndpoint>> {
        Ok(Vec::new())
    }
    /// Make `id` the default for `role` (Windows: IPolicyConfig::SetDefaultEndpoint; Console also
    /// sets eMultimedia).
    fn set_default(&self, _id: &str, _role: DeviceRole) -> Result<()> {
        Err(crate::PlatformError::Unsupported("default audio devices"))
    }
    /// Agent exit: remove what `open` created (Linux: unload the pipe-source module, delete the FIFO).
    fn shutdown(&self) {}
}

/// Used when the OS has no backend (and in tests).
pub struct NullMicSink;

impl MicSink for NullMicSink {
    fn devices(&self) -> Result<Vec<MicOutputDevice>> {
        Ok(Vec::new())
    }
    fn open(&self, _: &str, _: u32) -> Result<Box<dyn MicStream>> {
        Err(crate::PlatformError::Unsupported("phone microphone output"))
    }
}

/// Endpoint names that belong to a virtual cable. Matches the friendly name or device
/// description, case-insensitive: VB-Audio ("CABLE Input", "CABLE Output", "CABLE-A Input",
/// "VB-Audio Virtual Cable", "VB-Audio Hi-Fi Cable"), Virtual Audio Cable ("Line 1 (Virtual Audio
/// Cable)"). VoiceMeeter's own endpoints are not cables (they are mixer buses).
pub fn is_virtual_cable_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains("voicemeeter") {
        return false;
    }
    n.contains("vb-audio virtual cable")
        || n.contains("vb-audio hi-fi cable")
        || n.starts_with("cable input")
        || n.starts_with("cable output")
        || n.starts_with("cable-")
        || n.contains("virtual audio cable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cable_names() {
        assert!(is_virtual_cable_name("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_virtual_cable_name("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(is_virtual_cable_name("CABLE-A Input (VB-Audio Cable A)"));
        assert!(is_virtual_cable_name("Line 1 (Virtual Audio Cable)"));
        assert!(!is_virtual_cable_name("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"));
        assert!(!is_virtual_cable_name("Speakers (Realtek(R) Audio)"));
        assert!(!is_virtual_cable_name("Headset Microphone (HyperX Cloud)"));
    }
}
