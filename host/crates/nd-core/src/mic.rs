// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone-microphone control surface (round 25) used by the mic deck actions and live data.
//!
//! The agent implements [`MicActions`] with its mic manager; core only sees the trait.

/// What a mic key needs to display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MicSnapshot {
    /// The user turned the phone mic on (it may still be starting or waiting for the phone).
    pub on: bool,
    /// Audio is flowing into the output device right now.
    pub live: bool,
    /// Muted on the PC side (silence is written, the phone keeps streaming).
    pub muted: bool,
}

/// Mic operations behind the deck actions. Methods are synchronous and quick.
pub trait MicActions: Send + Sync {
    /// Turn the phone mic on or off. Returns whether it is now on.
    fn toggle(&self) -> Result<bool, String>;
    /// Mute or unmute. Returns whether it is now muted.
    fn toggle_mute(&self) -> Result<bool, String>;
    fn snapshot(&self) -> MicSnapshot;
}

/// Used when no mic manager is attached (tests, headless).
pub struct NullMic;

impl MicActions for NullMic {
    fn toggle(&self) -> Result<bool, String> {
        Err("the phone microphone is not available".into())
    }
    fn toggle_mute(&self) -> Result<bool, String> {
        Err("the phone microphone is not available".into())
    }
    fn snapshot(&self) -> MicSnapshot {
        MicSnapshot::default()
    }
}
