// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure clock/timer/stopwatch logic (no I/O, time is passed in).
use std::time::{Duration, Instant};

use chrono::{NaiveDateTime, Timelike};

/// `13:05`, `13:05:09`, `1:05 PM`, `1:05:09 PM`.
pub fn format_time(t: &NaiveDateTime, h24: bool, seconds: bool) -> String {
    let (h, m, s) = (t.hour(), t.minute(), t.second());
    if h24 {
        if seconds {
            format!("{h:02}:{m:02}:{s:02}")
        } else {
            format!("{h:02}:{m:02}")
        }
    } else {
        let (pm, h12) = (h >= 12, if h % 12 == 0 { 12 } else { h % 12 });
        let suffix = if pm { "PM" } else { "AM" };
        if seconds {
            format!("{h12}:{m:02}:{s:02} {suffix}")
        } else {
            format!("{h12}:{m:02} {suffix}")
        }
    }
}

/// `Tue 14 Oct`.
pub fn format_date(t: &NaiveDateTime) -> String {
    t.format("%a %-d %b").to_string()
}

/// `m:ss`, or `h:mm:ss` from one hour up.
pub fn format_hms(total_secs: u64) -> String {
    let (h, m, s) = (total_secs / 3600, total_secs / 60 % 60, total_secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `m:ss` where minutes are not wrapped into hours (timer: 90 minutes shows `90:00`).
pub fn format_ms(total_secs: u64) -> String {
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// Countdown timer state machine.
#[derive(Debug, Clone)]
pub struct Timer {
    total: Duration,
    elapsed: Duration,
    started: Option<Instant>,
    done: bool,
}

impl Timer {
    pub fn new(total_secs: u64) -> Timer {
        Timer { total: Duration::from_secs(total_secs), elapsed: Duration::ZERO, started: None, done: false }
    }

    #[cfg(test)]
    pub fn total_secs(&self) -> u64 {
        self.total.as_secs()
    }
    pub fn set_total(&mut self, secs: u64) {
        self.total = Duration::from_secs(secs);
    }
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.started.is_some()
    }
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn elapsed_at(&self, now: Instant) -> Duration {
        self.elapsed + self.started.map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
    }

    /// Seconds left, rounded up.
    pub fn remaining_secs(&self, now: Instant) -> u64 {
        let left = self.total.saturating_sub(self.elapsed_at(now));
        left.as_secs() + u64::from(left.subsec_nanos() > 0)
    }

    /// Tap: start, pause, resume; after finishing it clears the "done" state.
    pub fn toggle(&mut self, now: Instant) {
        if self.done {
            self.reset();
        } else if let Some(s) = self.started.take() {
            self.elapsed += now.saturating_duration_since(s);
        } else if self.total > self.elapsed {
            self.started = Some(now);
        } else if !self.total.is_zero() {
            // paused with nothing left (total shortened): start over
            self.elapsed = Duration::ZERO;
            self.started = Some(now);
        }
    }

    pub fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
        self.started = None;
        self.done = false;
    }

    /// Advance; returns true exactly once, when the countdown reaches zero.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.started.is_some() && self.remaining_secs(now) == 0 {
            self.started = None;
            self.elapsed = self.total;
            self.done = true;
            return true;
        }
        false
    }

    pub fn title(&self, now: Instant) -> String {
        if self.done {
            "Done".into()
        } else {
            format_ms(self.remaining_secs(now))
        }
    }
}

/// Stopwatch.
#[derive(Debug, Clone, Default)]
pub struct Stopwatch {
    elapsed: Duration,
    started: Option<Instant>,
}

impl Stopwatch {
    pub fn toggle(&mut self, now: Instant) {
        if let Some(s) = self.started.take() {
            self.elapsed += now.saturating_duration_since(s);
        } else {
            self.started = Some(now);
        }
    }
    pub fn reset(&mut self) {
        *self = Stopwatch::default();
    }
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.started.is_some()
    }
    pub fn elapsed_secs(&self, now: Instant) -> u64 {
        (self.elapsed + self.started.map_or(Duration::ZERO, |s| now.saturating_duration_since(s))).as_secs()
    }
    pub fn title(&self, now: Instant) -> String {
        format_hms(self.elapsed_secs(now))
    }
}

/// Which action a protocol `action` string names (`timer` or `dev.notideck.clock/timer`).
pub fn action_name(action: &str) -> &str {
    action.rsplit(['/', ':']).next().unwrap_or(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 14).unwrap().and_hms_opt(h, m, s).unwrap()
    }

    #[test]
    fn time_formats() {
        assert_eq!(format_time(&at(13, 5, 9), true, false), "13:05");
        assert_eq!(format_time(&at(13, 5, 9), true, true), "13:05:09");
        assert_eq!(format_time(&at(13, 5, 9), false, false), "1:05 PM");
        assert_eq!(format_time(&at(0, 5, 9), false, true), "12:05:09 AM");
        assert_eq!(format_time(&at(12, 0, 0), false, false), "12:00 PM");
        assert_eq!(format_date(&at(1, 0, 0)), "Wed 14 Oct");
    }

    #[test]
    fn durations() {
        assert_eq!(format_hms(65), "1:05");
        assert_eq!(format_hms(3600), "1:00:00");
        assert_eq!(format_ms(5400), "90:00");
        assert_eq!(format_ms(0), "0:00");
    }

    #[test]
    fn action_names() {
        assert_eq!(action_name("timer"), "timer");
        assert_eq!(action_name("dev.notideck.clock/timer"), "timer");
        assert_eq!(action_name("dev.notideck.clock:stopwatch"), "stopwatch");
    }

    #[test]
    fn timer_runs_pauses_and_finishes() {
        let t0 = Instant::now();
        let s = |n| t0 + Duration::from_secs(n);
        let mut t = Timer::new(10);
        assert_eq!(t.title(t0), "0:10");
        t.toggle(t0);
        assert!(t.is_running());
        assert_eq!(t.title(s(3)), "0:07");
        assert!(!t.tick(s(3)));
        t.toggle(s(4)); // pause at 6 left
        assert!(!t.is_running());
        assert_eq!(t.title(s(100)), "0:06");
        t.toggle(s(100)); // resume
        assert_eq!(t.title(s(102)), "0:04");
        assert!(!t.tick(s(105)));
        assert!(t.tick(s(106)));
        assert!(!t.tick(s(107)), "done fires once");
        assert!(t.is_done());
        assert_eq!(t.title(s(107)), "Done");
        t.toggle(s(108)); // tap clears done
        assert!(!t.is_done() && !t.is_running());
        assert_eq!(t.title(s(108)), "0:10");
    }

    #[test]
    fn timer_reset_and_zero() {
        let t0 = Instant::now();
        let mut t = Timer::new(0);
        t.toggle(t0);
        assert!(!t.is_running(), "zero timer does not start");
        t.set_total(65);
        t.toggle(t0);
        t.reset();
        assert!(!t.is_running());
        assert_eq!(t.title(t0), "1:05");
        assert_eq!(t.total_secs(), 65);
    }

    #[test]
    fn timer_rounds_up() {
        let t0 = Instant::now();
        let mut t = Timer::new(5);
        t.toggle(t0);
        assert_eq!(t.remaining_secs(t0 + Duration::from_millis(500)), 5);
        assert_eq!(t.remaining_secs(t0 + Duration::from_millis(1500)), 4);
    }

    #[test]
    fn stopwatch() {
        let t0 = Instant::now();
        let s = |n| t0 + Duration::from_secs(n);
        let mut w = Stopwatch::default();
        assert_eq!(w.title(t0), "0:00");
        w.toggle(t0);
        assert_eq!(w.title(s(75)), "1:15");
        w.toggle(s(80));
        assert_eq!(w.title(s(500)), "1:20");
        w.toggle(s(500));
        assert_eq!(w.title(s(3600 + 500)), "1:01:20");
        w.reset();
        assert!(!w.is_running());
        assert_eq!(w.title(s(9)), "0:00");
    }
}
