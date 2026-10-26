// SPDX-License-Identifier: GPL-3.0-or-later
//! Camera pipeline measurements: lock-free counters written by the reader and decode threads,
//! and a reporter that turns them into one `camera stats` line per window (5 s).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

pub const REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// Cumulative counters (sums are never reset; maxima are reset by [`StatCounters::snapshot`]).
#[derive(Default)]
pub struct StatCounters {
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    /// Latest-wins queue discards.
    pub drops: AtomicU64,
    /// Non-config packets handed to the decoder.
    pub fed: AtomicU64,
    /// Frames the decoder returned.
    pub decoded: AtomicU64,
    pub decode_ns: AtomicU64,
    pub decode_max_ns: AtomicU64,
    pub scaled: AtomicU64,
    pub scale_ns: AtomicU64,
    pub scale_max_ns: AtomicU64,
    /// Frames written to shared memory.
    pub published: AtomicU64,
    /// Last output size, `w << 32 | h`.
    pub out_size: AtomicU64,
}

impl StatCounters {
    pub fn record_decode(&self, d: Duration, frames_out: u64) {
        let ns = d.as_nanos() as u64;
        self.fed.fetch_add(1, Relaxed);
        self.decoded.fetch_add(frames_out, Relaxed);
        self.decode_ns.fetch_add(ns, Relaxed);
        self.decode_max_ns.fetch_max(ns, Relaxed);
    }

    pub fn record_scale(&self, d: Duration) {
        let ns = d.as_nanos() as u64;
        self.scaled.fetch_add(1, Relaxed);
        self.scale_ns.fetch_add(ns, Relaxed);
        self.scale_max_ns.fetch_max(ns, Relaxed);
    }

    pub fn record_published(&self, w: u32, h: u32) {
        self.published.fetch_add(1, Relaxed);
        self.out_size.store(u64::from(w) << 32 | u64::from(h), Relaxed);
    }

    /// Read everything; the two maxima restart for the next window.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            packets: self.packets.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            drops: self.drops.load(Relaxed),
            fed: self.fed.load(Relaxed),
            decoded: self.decoded.load(Relaxed),
            decode_ns: self.decode_ns.load(Relaxed),
            decode_max_ns: self.decode_max_ns.swap(0, Relaxed),
            scaled: self.scaled.load(Relaxed),
            scale_ns: self.scale_ns.load(Relaxed),
            scale_max_ns: self.scale_max_ns.swap(0, Relaxed),
            published: self.published.load(Relaxed),
            out_size: self.out_size.load(Relaxed),
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub packets: u64,
    pub bytes: u64,
    pub drops: u64,
    pub fed: u64,
    pub decoded: u64,
    pub decode_ns: u64,
    pub decode_max_ns: u64,
    pub scaled: u64,
    pub scale_ns: u64,
    pub scale_max_ns: u64,
    pub published: u64,
    pub out_size: u64,
}

/// One window's worth of rates and averages.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Report {
    pub packets_per_s: f64,
    pub kbit_per_s: f64,
    pub decoded_per_s: f64,
    pub drops: u64,
    pub decode_ms_avg: f64,
    pub decode_ms_max: f64,
    /// Frames fed minus frames out, cumulative: stays near 0 when the decoder does not buffer.
    pub decode_lag_frames: i64,
    pub scale_ms_avg: f64,
    pub scale_ms_max: f64,
    pub published_per_s: f64,
    pub out_w: u32,
    pub out_h: u32,
}

fn avg_ms(sum_ns: u64, n: u64) -> f64 {
    if n == 0 {
        0.0
    } else {
        sum_ns as f64 / n as f64 / 1e6
    }
}

/// Turns successive snapshots into per-window reports.
#[derive(Default)]
pub struct Reporter {
    prev: Snapshot,
}

impl Reporter {
    pub fn report(&mut self, now: Snapshot, window: Duration) -> Report {
        let p = self.prev;
        self.prev = now;
        let secs = window.as_secs_f64().max(1e-3);
        let d = |a: u64, b: u64| a.saturating_sub(b);
        Report {
            packets_per_s: d(now.packets, p.packets) as f64 / secs,
            kbit_per_s: d(now.bytes, p.bytes) as f64 * 8.0 / 1000.0 / secs,
            decoded_per_s: d(now.decoded, p.decoded) as f64 / secs,
            drops: d(now.drops, p.drops),
            decode_ms_avg: avg_ms(d(now.decode_ns, p.decode_ns), d(now.fed, p.fed)),
            decode_ms_max: now.decode_max_ns as f64 / 1e6,
            decode_lag_frames: now.fed as i64 - now.decoded as i64,
            scale_ms_avg: avg_ms(d(now.scale_ns, p.scale_ns), d(now.scaled, p.scaled)),
            scale_ms_max: now.scale_max_ns as f64 / 1e6,
            published_per_s: d(now.published, p.published) as f64 / secs,
            out_w: (now.out_size >> 32) as u32,
            out_h: now.out_size as u32,
        }
    }
}

impl Report {
    pub fn log(&self) {
        tracing::info!(
            target: "nd_agent::camera",
            packets_per_s = format_args!("{:.1}", self.packets_per_s),
            kbit_per_s = format_args!("{:.0}", self.kbit_per_s),
            decoded_per_s = format_args!("{:.1}", self.decoded_per_s),
            queue_drops = self.drops,
            decode_ms_avg = format_args!("{:.2}", self.decode_ms_avg),
            decode_ms_max = format_args!("{:.2}", self.decode_ms_max),
            decode_lag_frames = self.decode_lag_frames,
            scale_ms_avg = format_args!("{:.2}", self.scale_ms_avg),
            scale_ms_max = format_args!("{:.2}", self.scale_ms_max),
            published_per_s = format_args!("{:.1}", self.published_per_s),
            output = format_args!("{}x{}", self.out_w, self.out_h),
            "camera stats"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_rates_and_averages() {
        let c = StatCounters::default();
        let mut r = Reporter::default();
        c.packets.fetch_add(150, Relaxed);
        c.bytes.fetch_add(250_000, Relaxed);
        c.drops.fetch_add(2, Relaxed);
        for ms in [2u64, 4, 12] {
            c.record_decode(Duration::from_millis(ms), 1);
        }
        c.record_scale(Duration::from_millis(5));
        c.record_scale(Duration::from_millis(9));
        c.record_published(1280, 720);
        let a = r.report(c.snapshot(), Duration::from_secs(5));
        assert_eq!(a.packets_per_s, 30.0);
        assert_eq!(a.kbit_per_s, 400.0);
        assert_eq!(a.drops, 2);
        assert!((a.decode_ms_avg - 6.0).abs() < 1e-9);
        assert!((a.decode_ms_max - 12.0).abs() < 1e-9);
        assert!((a.scale_ms_avg - 7.0).abs() < 1e-9);
        assert!((a.scale_ms_max - 9.0).abs() < 1e-9);
        assert_eq!((a.out_w, a.out_h), (1280, 720));
        assert_eq!(a.decode_lag_frames, 0);
        assert!((a.published_per_s - 0.2).abs() < 1e-9);

        // Next window only sees its own activity; maxima restart.
        c.packets.fetch_add(10, Relaxed);
        c.record_decode(Duration::from_millis(1), 0); // decoder buffered one
        let b = r.report(c.snapshot(), Duration::from_secs(5));
        assert_eq!(b.packets_per_s, 2.0);
        assert_eq!(b.drops, 0);
        assert!((b.decode_ms_avg - 1.0).abs() < 1e-9);
        assert!((b.decode_ms_max - 1.0).abs() < 1e-9);
        assert_eq!(b.scale_ms_avg, 0.0);
        assert_eq!(b.scale_ms_max, 0.0);
        assert_eq!(b.decode_lag_frames, 1);
        assert_eq!(b.published_per_s, 0.0);
    }

    #[test]
    fn empty_window_is_zero() {
        let mut r = Reporter::default();
        let a = r.report(Snapshot::default(), Duration::from_secs(5));
        assert_eq!(a, Report::default());
    }
}
