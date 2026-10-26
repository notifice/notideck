// SPDX-License-Identifier: GPL-3.0-or-later
//! The playback thread: PCM from the media reader -> jitter / drift control -> gain, mute -> sink.
//! The sink's write path may block briefly, so none of this runs on the async runtime.

use super::logic::{self, Jitter, LevelMeter};
use nd_platform::MicStream;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Where the media reader drops decoded samples; `None` while no output is open.
pub type AudioSlot = Arc<Mutex<Option<Sender<Vec<i16>>>>>;

const POLL: Duration = Duration::from_millis(5);

/// State the thread publishes and the manager reads (all relaxed atomics).
#[derive(Default)]
pub struct Shared {
    pub stop: AtomicBool,
    pub lost: AtomicBool,
    pub gain_bits: AtomicU32,
    pub level_bits: AtomicU32,
    pub buffer_ms: AtomicU32,
    pub underruns: AtomicU64,
    pub dropped_ms: AtomicU64,
}

impl Shared {
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
    pub fn set_gain(&self, g: f32) {
        self.gain_bits.store(g.to_bits(), Ordering::Relaxed);
    }
}

pub struct Playback {
    pub shared: Arc<Shared>,
    pub tx: Sender<Vec<i16>>,
    join: Option<JoinHandle<()>>,
}

impl Playback {
    pub fn spawn(stream: Box<dyn MicStream>, rate: u32, muted: Arc<AtomicBool>, gain: f32) -> Playback {
        let shared = Arc::new(Shared::default());
        shared.set_gain(gain);
        let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let s2 = shared.clone();
        let join = std::thread::Builder::new()
            .name("nd-mic-playback".into())
            .spawn(move || run(stream, rate, rx, s2, muted))
            .ok();
        if join.is_none() {
            shared.lost.store(true, Ordering::Relaxed);
        }
        Playback { shared, tx, join }
    }

    pub fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::Relaxed)
    }

    /// Stop and wait for the thread, so the device is free again when this returns.
    pub fn close(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn run(mut stream: Box<dyn MicStream>, rate: u32, rx: Receiver<Vec<i16>>, sh: Arc<Shared>, muted: Arc<AtomicBool>) {
    let mut jitter = Jitter::adaptive(rate, stream.target_ms(), logic::TARGET_CEIL_MS.max(stream.target_ms()));
    let mut meter = LevelMeter::default();
    let mut publish_at = Instant::now();
    while !sh.stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(POLL) {
            Ok(v) => jitter.push(Instant::now(), &v),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(v) = rx.try_recv() {
            jitter.push(Instant::now(), &v);
        }
        if stream.lost() {
            warn!("the mic output device went away");
            sh.lost.store(true, Ordering::Relaxed);
            return;
        }
        let queued = match stream.queued() {
            Ok(q) => q,
            Err(e) => {
                warn!("mic output: queued(): {e}");
                sh.lost.store(true, Ordering::Relaxed);
                return;
            }
        };
        let now = Instant::now();
        let mut out = jitter.plan(now, queued);
        if !out.is_empty() {
            if muted.load(Ordering::Relaxed) {
                out.iter_mut().for_each(|s| *s = 0);
            } else {
                logic::apply_gain(&mut out, f32::from_bits(sh.gain_bits.load(Ordering::Relaxed)));
            }
            meter.feed(now, &out);
            match stream.write(&out) {
                Ok(n) if n < out.len() => jitter.note_dropped(out.len() - n),
                Ok(_) => {}
                Err(e) => {
                    warn!("mic output write failed: {e}");
                    sh.lost.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
        if now.duration_since(publish_at) >= Duration::from_millis(50) {
            publish_at = now;
            sh.level_bits.store(meter.level(now).to_bits(), Ordering::Relaxed);
            sh.buffer_ms.store(jitter.buffer_ms(queued), Ordering::Relaxed);
            sh.underruns.store(jitter.counters.underruns, Ordering::Relaxed);
            sh.dropped_ms.store(jitter.counters.dropped_ms, Ordering::Relaxed);
        }
    }
    debug!("mic playback thread ended");
}
