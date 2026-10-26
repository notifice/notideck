// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-stream frame selection: shared-memory frame, last frame again, or the placeholder.

use std::sync::Arc;

use nd_vcam::shm::{nv12_len, Peek, ReadResult};
use nd_vcam::HasShm;

use crate::placeholder;

pub struct FrameProvider {
    shm: Option<Arc<dyn HasShm>>,
    last_seq: u64,
    last: Vec<u8>,
    has_last: bool,
    /// Age of the shm frame when the last `Fresh` one was copied (producer stamp -> now).
    last_age_us: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Source {
    Fresh,
    Repeated,
    Placeholder,
}

impl FrameProvider {
    pub fn new(shm: Option<Arc<dyn HasShm>>) -> Self {
        FrameProvider { shm, last_seq: 0, last: Vec::new(), has_last: false, last_age_us: None }
    }

    pub fn set_shm(&mut self, shm: Option<Arc<dyn HasShm>>) {
        self.shm = shm;
    }

    /// A new media type was selected: forget the previous frame.
    pub fn reset(&mut self) {
        self.last_seq = 0;
        self.has_last = false;
        self.last.clear();
    }

    /// Is a frame newer than the one last served waiting in shared memory?
    pub fn peek(&self) -> Peek {
        match &self.shm {
            Some(s) => s.shm().peek(self.last_seq),
            None => Peek::NoProducer,
        }
    }

    /// Age (us) of the frame served by the last `fill` when it returned `Fresh`.
    pub fn last_age_us(&self) -> Option<u64> {
        self.last_age_us
    }

    /// Fill `out` (exactly `nv12_len(width, height)` bytes) with the frame to send.
    pub fn fill(&mut self, width: u32, height: u32, out: &mut [u8]) -> Source {
        debug_assert_eq!(out.len(), nv12_len(width, height));
        let result = match &self.shm {
            Some(s) => s.shm().read_frame(self.last_seq, width, height, out),
            None => ReadResult::NoProducer,
        };
        match result {
            ReadResult::Frame(seq) => {
                self.last_seq = seq;
                self.last_age_us = self.shm.as_ref().and_then(|s| s.shm().frame_age_us());
                self.last.clear();
                self.last.extend_from_slice(out);
                self.has_last = true;
                Source::Fresh
            }
            ReadResult::Unchanged | ReadResult::Torn if self.has_last && self.last.len() == out.len() => {
                out.copy_from_slice(&self.last);
                Source::Repeated
            }
            ReadResult::NoProducer => {
                self.has_last = false;
                self.last_seq = 0;
                out.copy_from_slice(&placeholder::cached(width, height));
                Source::Placeholder
            }
            _ => {
                out.copy_from_slice(&placeholder::cached(width, height));
                Source::Placeholder
            }
        }
    }
}

/// Per-window delivery counters for the periodic vcam.log line.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryStats {
    pub fresh: u32,
    pub repeated: u32,
    pub placeholder: u32,
    age_sum_us: u64,
    age_n: u32,
}

impl DeliveryStats {
    pub fn record(&mut self, src: Source, age_us: Option<u64>) {
        match src {
            Source::Fresh => {
                self.fresh += 1;
                if let Some(a) = age_us {
                    self.age_sum_us += a;
                    self.age_n += 1;
                }
            }
            Source::Repeated => self.repeated += 1,
            Source::Placeholder => self.placeholder += 1,
        }
    }

    pub fn delivered(&self) -> u32 {
        self.fresh + self.repeated + self.placeholder
    }

    /// Average age in ms of the fresh frames that carried a producer stamp.
    pub fn age_ms_avg(&self) -> Option<f64> {
        (self.age_n > 0).then(|| self.age_sum_us as f64 / self.age_n as f64 / 1000.0)
    }

    /// Log line body; takes the counters (the window restarts).
    pub fn take_line(&mut self, secs: f64) -> String {
        let s = std::mem::take(self);
        let age = match s.age_ms_avg() {
            Some(a) => format!("{a:.1}"),
            None => "n/a".into(),
        };
        format!(
            "stats: delivered={} ({:.1}/s) fresh={} repeated={} placeholder={} shm_age_ms_avg={age}",
            s.delivered(),
            f64::from(s.delivered()) / secs.max(1e-3),
            s.fresh,
            s.repeated,
            s.placeholder
        )
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn delivery_stats_window() {
        let mut d = DeliveryStats::default();
        d.record(Source::Fresh, Some(2_000));
        d.record(Source::Fresh, Some(6_000));
        d.record(Source::Fresh, None);
        d.record(Source::Repeated, None);
        d.record(Source::Placeholder, None);
        assert_eq!(d.delivered(), 5);
        assert_eq!(d.age_ms_avg(), Some(4.0));
        let line = d.take_line(5.0);
        assert!(line.contains("delivered=5 (1.0/s) fresh=3 repeated=1 placeholder=1 shm_age_ms_avg=4.0"), "{line}");
        assert_eq!(d, DeliveryStats::default());
        assert!(d.take_line(5.0).contains("shm_age_ms_avg=n/a"));
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use nd_vcam::MemShm;

    #[test]
    fn placeholder_then_frame_then_repeat_then_placeholder() {
        let mem = Arc::new(MemShm::new());
        let mut p = FrameProvider::new(Some(mem.clone()));
        let n = nv12_len(640, 360);
        let mut out = vec![0u8; n];
        assert_eq!(p.fill(640, 360, &mut out), Source::Placeholder);
        assert_eq!(out[0], placeholder::BG_Y);

        mem.shm().producer_heartbeat();
        assert_eq!(p.fill(640, 360, &mut out), Source::Placeholder); // heartbeat but no frame yet

        let frame: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        assert!(mem.shm().write_frame(640, 360, 1, &frame));
        assert_eq!(p.peek(), Peek::New);
        assert_eq!(p.fill(640, 360, &mut out), Source::Fresh);
        assert!(p.last_age_us().is_some());
        assert_eq!(p.peek(), Peek::Unchanged);
        assert_eq!(out, frame);
        out.fill(0);
        assert_eq!(p.fill(640, 360, &mut out), Source::Repeated);
        assert_eq!(out, frame);

        // Wrong size frame from the producer: placeholder.
        let big = vec![1u8; nv12_len(1280, 720)];
        assert!(mem.shm().write_frame(1280, 720, 2, &big));
        assert_eq!(p.fill(640, 360, &mut out), Source::Placeholder);
        assert_eq!(out[0], placeholder::BG_Y);
    }

    #[test]
    fn no_shm_is_placeholder() {
        let mut p = FrameProvider::new(None);
        let mut out = vec![0u8; nv12_len(640, 360)];
        assert_eq!(p.fill(640, 360, &mut out), Source::Placeholder);
    }
}
