// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared memory between the app (frame producer) and the virtual camera DLL (consumer side).
//! ADR 0005 "Shared memory layout (v1)". Both sides use this module; only the mapping itself
//! (create in the DLL, open in the app) is platform code.
//!
//! The region is `HEADER_BYTES` of [`Header`] followed by one NV12 frame slot of
//! [`MAX_FRAME_BYTES`]. The frame slot is guarded by a seqlock on `frame_seq`.

use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

/// Section name (the DLL creates it, the app opens it).
pub const SECTION_NAME: &str = "Global\\NotideckVirtualCamera-v1";
pub const MAGIC: u32 = 0x4356_444E; // "NDVC" little-endian
pub const VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 256;
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const MAX_FRAME_BYTES: usize = (MAX_WIDTH * MAX_HEIGHT * 3 / 2) as usize;
pub const REGION_BYTES: usize = HEADER_BYTES + MAX_FRAME_BYTES;
/// A producer heartbeat older than this makes the DLL serve the placeholder.
pub const PRODUCER_STALE_MS: u64 = 1000;

/// COM class id of the media source (`notideck_vcam.dll`).
pub const CLSID_STR: &str = "{E4DB89AD-622C-4168-8CD3-639FD6947B4A}";
/// Friendly name shown in camera pickers.
pub const FRIENDLY_NAME: &str = "Notideck Camera";

/// Media types the DLL advertises (NV12, 30 fps), in preference order.
pub const MODES: [(u32, u32); 3] = [(1920, 1080), (1280, 720), (640, 360)];
pub const FPS: u32 = 30;

/// NV12 size of a `width`×`height` frame (Y plane + interleaved UV at half resolution).
pub const fn nv12_len(width: u32, height: u32) -> usize {
    (width * height + 2 * (width / 2) * (height / 2)) as usize
}

#[repr(C)]
pub struct Header {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub fps: AtomicU32,
    pub consumer_active: AtomicU32,
    pub consumer_heartbeat_ms: AtomicU64,
    pub producer_heartbeat_ms: AtomicU64,
    pub frame_seq: AtomicU64,
    pub frame_width: AtomicU32,
    pub frame_height: AtomicU32,
    pub frame_pts_us: AtomicU64,
    /// Wall-clock microseconds since the Unix epoch when the producer finished writing the
    /// current frame (0 = unknown, e.g. an older app). Appended inside the reserved header
    /// space, so layout version 1 stays compatible in both directions.
    pub frame_publish_us: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<Header>() <= HEADER_BYTES);

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn now_us() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

/// A view over a mapped region of at least [`REGION_BYTES`] bytes, 8-byte aligned.
///
/// # Safety contract
/// The constructor is unsafe: `base` must stay mapped and valid for `REGION_BYTES` for the
/// lifetime of the view. Concurrent access from the other process goes through the atomics in
/// [`Header`]; the frame slot is only read/written through [`Shm::write_frame`] / [`Shm::read_frame`].
pub struct Shm {
    base: *mut u8,
}

unsafe impl Send for Shm {}
unsafe impl Sync for Shm {}

/// Result of [`Shm::read_frame`].
#[derive(Debug, PartialEq, Eq)]
pub enum ReadResult {
    /// A complete frame of the requested size was copied; carries its seq.
    Frame(u64),
    /// No frame newer than `last_seq`.
    Unchanged,
    /// The producer is stale, or the frame has a different size: serve the placeholder.
    NoProducer,
    /// The producer was writing; try again or reuse the previous frame.
    Torn,
}

/// Result of [`Shm::peek`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Peek {
    NoProducer,
    Unchanged,
    New,
}

impl Shm {
    /// # Safety
    /// See the type-level contract.
    pub unsafe fn from_raw(base: *mut u8) -> Self {
        debug_assert_eq!(base as usize % 8, 0);
        Shm { base }
    }

    pub fn header(&self) -> &Header {
        unsafe { &*(self.base as *const Header) }
    }

    fn slot(&self) -> *mut u8 {
        unsafe { self.base.add(HEADER_BYTES) }
    }

    /// DLL: stamp a fresh region (after creating the section).
    pub fn init(&self) {
        let h = self.header();
        h.version.store(VERSION, Ordering::Relaxed);
        h.width.store(MODES[1].0, Ordering::Relaxed);
        h.height.store(MODES[1].1, Ordering::Relaxed);
        h.fps.store(FPS, Ordering::Relaxed);
        h.magic.store(MAGIC, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        let h = self.header();
        h.magic.load(Ordering::Acquire) == MAGIC && h.version.load(Ordering::Relaxed) == VERSION
    }

    /// DLL: the consumer picked a media type / started / stopped.
    pub fn set_consumer(&self, active: bool, width: u32, height: u32, fps: u32) {
        let h = self.header();
        h.width.store(width, Ordering::Relaxed);
        h.height.store(height, Ordering::Relaxed);
        h.fps.store(fps, Ordering::Relaxed);
        h.consumer_heartbeat_ms.store(now_ms(), Ordering::Relaxed);
        h.consumer_active.store(active as u32, Ordering::Release);
    }

    /// App: the size the consumer wants, or `None` when no consumer is active.
    pub fn wanted(&self) -> Option<(u32, u32, u32)> {
        let h = self.header();
        if h.consumer_active.load(Ordering::Acquire) == 0 {
            return None;
        }
        Some((h.width.load(Ordering::Relaxed), h.height.load(Ordering::Relaxed), h.fps.load(Ordering::Relaxed)))
    }

    /// App: write one NV12 frame. `nv12.len()` must be `nv12_len(width, height)` and fit the slot.
    pub fn write_frame(&self, width: u32, height: u32, pts_us: u64, nv12: &[u8]) -> bool {
        if nv12.len() != nv12_len(width, height) || nv12.len() > MAX_FRAME_BYTES {
            return false;
        }
        let h = self.header();
        let seq = h.frame_seq.load(Ordering::Relaxed);
        let start = if seq.is_multiple_of(2) { seq + 1 } else { seq + 2 };
        h.frame_seq.store(start, Ordering::Relaxed);
        fence(Ordering::Release);
        unsafe { std::ptr::copy_nonoverlapping(nv12.as_ptr(), self.slot(), nv12.len()) };
        h.frame_width.store(width, Ordering::Relaxed);
        h.frame_height.store(height, Ordering::Relaxed);
        h.frame_pts_us.store(pts_us, Ordering::Relaxed);
        h.frame_publish_us.store(now_us(), Ordering::Relaxed);
        h.frame_seq.store(start + 1, Ordering::Release);
        h.producer_heartbeat_ms.store(now_ms(), Ordering::Release);
        true
    }

    /// DLL: has the producer published a frame newer than `last_seq`? Cheap (no copy); lets the
    /// pacing loop wait briefly for a frame that is about to arrive instead of repeating one.
    pub fn peek(&self, last_seq: u64) -> Peek {
        let h = self.header();
        let beat = h.producer_heartbeat_ms.load(Ordering::Acquire);
        if now_ms().saturating_sub(beat) > PRODUCER_STALE_MS {
            return Peek::NoProducer;
        }
        let s = h.frame_seq.load(Ordering::Acquire);
        if s != last_seq && s & 1 == 0 {
            Peek::New
        } else {
            Peek::Unchanged
        }
    }

    /// DLL: age in microseconds of the frame last read, if the producer stamps frames.
    pub fn frame_age_us(&self) -> Option<u64> {
        match self.header().frame_publish_us.load(Ordering::Relaxed) {
            0 => None,
            t => Some(now_us().saturating_sub(t)),
        }
    }

    /// App: keep the producer alive while idle (no frames, e.g. while the phone starts).
    pub fn producer_heartbeat(&self) {
        self.header().producer_heartbeat_ms.store(now_ms(), Ordering::Release);
    }

    /// DLL: copy the latest frame into `out` if it is newer than `last_seq` and `out.len()`
    /// matches the selected size.
    pub fn read_frame(&self, last_seq: u64, width: u32, height: u32, out: &mut [u8]) -> ReadResult {
        let h = self.header();
        h.consumer_heartbeat_ms.store(now_ms(), Ordering::Relaxed);
        let beat = h.producer_heartbeat_ms.load(Ordering::Acquire);
        if now_ms().saturating_sub(beat) > PRODUCER_STALE_MS {
            return ReadResult::NoProducer;
        }
        let s1 = h.frame_seq.load(Ordering::Acquire);
        if s1 % 2 == 1 {
            return ReadResult::Torn;
        }
        if s1 == last_seq {
            return ReadResult::Unchanged;
        }
        if h.frame_width.load(Ordering::Relaxed) != width
            || h.frame_height.load(Ordering::Relaxed) != height
            || out.len() != nv12_len(width, height)
        {
            return ReadResult::NoProducer;
        }
        unsafe { std::ptr::copy_nonoverlapping(self.slot(), out.as_mut_ptr(), out.len()) };
        fence(Ordering::Acquire);
        if h.frame_seq.load(Ordering::Relaxed) != s1 {
            return ReadResult::Torn;
        }
        ReadResult::Frame(s1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> Vec<u64> {
        vec![0u64; REGION_BYTES / 8 + 1]
    }

    #[test]
    fn write_then_read() {
        let mut mem = region();
        let shm = unsafe { Shm::from_raw(mem.as_mut_ptr() as *mut u8) };
        shm.init();
        assert!(shm.is_valid());
        assert_eq!(shm.wanted(), None);
        shm.set_consumer(true, 640, 360, 30);
        assert_eq!(shm.wanted(), Some((640, 360, 30)));

        let mut out = vec![0u8; nv12_len(640, 360)];
        assert_eq!(shm.read_frame(0, 640, 360, &mut out), ReadResult::NoProducer);

        let frame = vec![7u8; nv12_len(640, 360)];
        assert!(shm.write_frame(640, 360, 99, &frame));
        let ReadResult::Frame(seq) = shm.read_frame(0, 640, 360, &mut out) else { panic!() };
        assert_eq!(out, frame);
        assert_eq!(seq % 2, 0);
        assert_eq!(shm.read_frame(seq, 640, 360, &mut out), ReadResult::Unchanged);

        // Wrong size: the DLL must not use it.
        assert!(shm.write_frame(1280, 720, 100, &vec![1u8; nv12_len(1280, 720)]));
        assert_eq!(shm.read_frame(seq, 640, 360, &mut out), ReadResult::NoProducer);
        assert!(!shm.write_frame(640, 360, 0, &[0u8; 10]));
    }

    #[test]
    fn peek_and_age() {
        let mut mem = region();
        let shm = unsafe { Shm::from_raw(mem.as_mut_ptr() as *mut u8) };
        shm.init();
        assert_eq!(shm.peek(0), Peek::NoProducer);
        assert_eq!(shm.frame_age_us(), None);
        shm.producer_heartbeat();
        assert_eq!(shm.peek(0), Peek::Unchanged);
        let frame = vec![3u8; nv12_len(640, 360)];
        assert!(shm.write_frame(640, 360, 1, &frame));
        assert_eq!(shm.peek(0), Peek::New);
        let mut out = vec![0u8; frame.len()];
        let ReadResult::Frame(seq) = shm.read_frame(0, 640, 360, &mut out) else { panic!() };
        assert_eq!(shm.peek(seq), Peek::Unchanged);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let age = shm.frame_age_us().unwrap();
        assert!((4_000..1_000_000).contains(&age), "age {age}");
    }
}
