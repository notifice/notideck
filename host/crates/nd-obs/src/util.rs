// SPDX-License-Identifier: GPL-3.0-or-later
//! Small helpers shared by the client and the fake server.

use base64::{engine::general_purpose::STANDARD, Engine};
use nd_core::obs::{ObsSnapshot, OutputState, SpecialInput};
use sha2::{Digest, Sha256};
use std::time::Instant;

/// Output indices used by the client and the fake: stream, record, replay buffer, virtual camera.
pub(crate) const OUT_COUNT: usize = 4;

pub(crate) const SPECIALS: [SpecialInput; 6] = [
    SpecialInput::Desktop1,
    SpecialInput::Desktop2,
    SpecialInput::Mic1,
    SpecialInput::Mic2,
    SpecialInput::Mic3,
    SpecialInput::Mic4,
];

pub(crate) fn out_mut(s: &mut ObsSnapshot, i: usize) -> &mut OutputState {
    match i {
        0 => &mut s.stream,
        1 => &mut s.record,
        2 => &mut s.replay_buffer,
        _ => &mut s.virtual_cam,
    }
}

/// obs-websocket authentication: `base64(sha256(base64(sha256(password + salt)) + challenge))`.
pub(crate) fn auth_string(password: &str, salt: &str, challenge: &str) -> String {
    let secret = STANDARD.encode(Sha256::digest(format!("{password}{salt}")));
    STANDARD.encode(Sha256::digest(format!("{secret}{challenge}")))
}

/// Random alphanumeric string (salts, challenges, passwords).
pub(crate) fn random_alnum(n: usize) -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng().sample_iter(&Alphanumeric).take(n).map(char::from).collect()
}

/// Running time of an output, excluding pauses.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Clock {
    acc_ms: u64,
    since: Option<Instant>,
}

impl Clock {
    /// Start (or restart) running with `ms` already elapsed.
    pub fn start_from(&mut self, ms: u64) {
        self.acc_ms = ms;
        self.since = Some(Instant::now());
    }
    pub fn pause(&mut self) {
        if let Some(s) = self.since.take() {
            self.acc_ms += s.elapsed().as_millis() as u64;
        }
    }
    pub fn resume(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }
    pub fn stop(&mut self) {
        *self = Clock::default();
    }
    pub fn ms(&self) -> u64 {
        self.acc_ms + self.since.map_or(0, |s| s.elapsed().as_millis() as u64)
    }
}
