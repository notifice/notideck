// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded "latest wins" packet queue between the media reader and the decode thread.
//!
//! When the decoder falls behind, queued delta frames are dropped (they cannot be decoded
//! without their predecessors anyway) and everything is discarded until the next key frame.
//! The producer is told so it can ask the phone for one.

use nd_proto::media::MediaPacket;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

pub const QUEUE_CAP: usize = 3;

struct Q {
    items: VecDeque<(Instant, MediaPacket)>,
    closed: bool,
    /// Discarding delta frames until the next key frame.
    wait_key: bool,
}

pub struct PacketQueue {
    inner: Mutex<Q>,
    cv: Condvar,
    cap: usize,
}

pub enum Pop {
    Packet(Instant, MediaPacket),
    Timeout,
    Closed,
}

impl PacketQueue {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Q { items: VecDeque::new(), closed: false, wait_key: false }),
            cv: Condvar::new(),
            cap,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Q> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a packet. Returns how many packets were dropped (0 = none); non-zero means the
    /// decoder needs a key frame.
    pub fn push(&self, arrival: Instant, p: MediaPacket) -> usize {
        let mut q = self.lock();
        if q.closed {
            return 0;
        }
        let mut dropped = 0;
        if p.header.config {
            // Only the newest SPS/PPS matters.
            q.items.retain(|(_, x)| !x.header.config);
        } else if p.header.key_frame {
            q.wait_key = false;
        } else if q.wait_key {
            return 1;
        }
        if !p.header.config && q.items.len() >= self.cap {
            let before = q.items.len();
            q.items.retain(|(_, x)| x.header.config);
            dropped = before - q.items.len();
            if !p.header.key_frame {
                // The new delta frame has lost its references.
                q.wait_key = true;
                dropped += 1;
                self.cv.notify_one();
                return dropped;
            }
        }
        q.items.push_back((arrival, p));
        self.cv.notify_one();
        dropped
    }

    /// Wait up to `timeout` for the next packet.
    pub fn pop(&self, timeout: Duration) -> Pop {
        let mut q = self.lock();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((t, p)) = q.items.pop_front() {
                return Pop::Packet(t, p);
            }
            if q.closed {
                return Pop::Closed;
            }
            let now = Instant::now();
            if now >= deadline {
                return Pop::Timeout;
            }
            q = self.cv.wait_timeout(q, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.cv.notify_all();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(pts: u64, config: bool, key: bool) -> MediaPacket {
        MediaPacket::new(pts, config, key, vec![0, 0, 0, 1, 0x65])
    }

    fn drain(q: &PacketQueue) -> Vec<(u64, bool, bool)> {
        let mut v = Vec::new();
        while let Pop::Packet(_, p) = q.pop(Duration::from_millis(1)) {
            v.push((p.header.pts_us, p.header.config, p.header.key_frame));
        }
        v
    }

    #[test]
    fn keeps_order_when_not_behind() {
        let q = PacketQueue::new(3);
        let t = Instant::now();
        assert_eq!(q.push(t, pkt(0, true, false)), 0);
        assert_eq!(q.push(t, pkt(1, false, true)), 0);
        assert_eq!(q.push(t, pkt(2, false, false)), 0);
        assert_eq!(drain(&q), vec![(0, true, false), (1, false, true), (2, false, false)]);
    }

    #[test]
    fn behind_drops_deltas_and_waits_for_key() {
        let q = PacketQueue::new(3);
        let t = Instant::now();
        q.push(t, pkt(0, true, false));
        q.push(t, pkt(1, false, true));
        q.push(t, pkt(2, false, false));
        // Full: this delta frame clears the queue (keeping CONFIG) and is itself dropped.
        let dropped = q.push(t, pkt(3, false, false));
        assert_eq!(dropped, 3, "key + delta queued, plus the new one");
        assert_eq!(q.len(), 1);
        // Deltas are ignored until a key frame arrives.
        assert_eq!(q.push(t, pkt(4, false, false)), 1);
        assert_eq!(q.push(t, pkt(5, false, true)), 0);
        assert_eq!(q.push(t, pkt(6, false, false)), 0);
        assert_eq!(drain(&q), vec![(0, true, false), (5, false, true), (6, false, false)]);
    }

    #[test]
    fn key_frame_when_full_replaces_the_backlog() {
        let q = PacketQueue::new(2);
        let t = Instant::now();
        q.push(t, pkt(1, false, true));
        q.push(t, pkt(2, false, false));
        assert_eq!(q.push(t, pkt(3, false, true)), 2);
        assert_eq!(drain(&q), vec![(3, false, true)]);
    }

    #[test]
    fn newest_config_replaces_older() {
        let q = PacketQueue::new(3);
        let t = Instant::now();
        q.push(t, pkt(0, true, false));
        q.push(t, pkt(0, true, false));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn pop_times_out_and_close_wakes() {
        let q = std::sync::Arc::new(PacketQueue::new(3));
        assert!(matches!(q.pop(Duration::from_millis(10)), Pop::Timeout));
        let q2 = q.clone();
        let h = std::thread::spawn(move || matches!(q2.pop(Duration::from_secs(5)), Pop::Closed));
        std::thread::sleep(Duration::from_millis(30));
        q.close();
        assert!(h.join().unwrap());
        assert_eq!(q.push(Instant::now(), pkt(1, false, true)), 0);
    }
}
