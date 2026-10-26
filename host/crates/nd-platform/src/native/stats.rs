// SPDX-License-Identifier: GPL-3.0-or-later
//! Cross-platform host load sampling built on `sysinfo`.

use std::sync::Mutex;
use std::time::Instant;

use sysinfo::{Networks, System};

use crate::{PlatformError, Result, SystemSnapshot, SystemStats};

struct State {
    sys: System,
    nets: Networks,
    last: Instant,
}

/// CPU / memory / network sampler. Rates cover the time since the previous [`snapshot`](SystemStats::snapshot).
pub struct SysinfoStats {
    state: Mutex<State>,
}

impl Default for SysinfoStats {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoStats {
    pub fn new() -> Self {
        let mut sys = System::new();
        // CPU usage is a delta between two refreshes: prime the first one now.
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let nets = Networks::new_with_refreshed_list();
        Self { state: Mutex::new(State { sys, nets, last: Instant::now() }) }
    }
}

/// True for loopback interfaces on Linux ("lo") and Windows ("Loopback Pseudo-Interface 1").
fn is_loopback(name: &str) -> bool {
    name == "lo" || name.to_ascii_lowercase().contains("loopback")
}

/// Bytes over an interval to bytes/sec; zero-length intervals yield 0.
fn per_sec(bytes: u64, secs: f64) -> u64 {
    if secs <= 0.0 {
        0
    } else {
        (bytes as f64 / secs).round() as u64
    }
}

impl SystemStats for SysinfoStats {
    fn snapshot(&self) -> Result<SystemSnapshot> {
        let mut st = self.state.lock().map_err(|_| PlatformError::Other("stats state poisoned".into()))?;
        let now = Instant::now();
        let secs = now.duration_since(st.last).as_secs_f64();
        st.last = now;

        st.sys.refresh_cpu_usage();
        st.sys.refresh_memory();
        st.nets.refresh(true);

        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in st.nets.iter() {
            if is_loopback(name) {
                continue;
            }
            // `received()`/`transmitted()` are deltas since the previous refresh.
            rx = rx.saturating_add(data.received());
            tx = tx.saturating_add(data.transmitted());
        }

        Ok(SystemSnapshot {
            cpu_percent: st.sys.global_cpu_usage().clamp(0.0, 100.0),
            mem_used_bytes: st.sys.used_memory(),
            mem_total_bytes: st.sys.total_memory(),
            net_rx_bytes_per_sec: per_sec(rx, secs),
            net_tx_bytes_per_sec: per_sec(tx, secs),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("lo"));
        assert!(is_loopback("Loopback Pseudo-Interface 1"));
        assert!(!is_loopback("eth0"));
    }

    #[test]
    fn rates() {
        assert_eq!(per_sec(1000, 2.0), 500);
        assert_eq!(per_sec(1000, 0.0), 0);
    }

    #[test]
    fn snapshot_works() {
        let s = SysinfoStats::new();
        std::thread::sleep(std::time::Duration::from_millis(250));
        let snap = s.snapshot().unwrap();
        assert!(snap.mem_total_bytes > 0);
        assert!((0.0..=100.0).contains(&snap.cpu_percent));
    }
}
