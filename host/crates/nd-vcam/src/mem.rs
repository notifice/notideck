// SPDX-License-Identifier: GPL-3.0-or-later
//! In-memory stand-in for the Windows shared-memory mapping, so the agent's webcam path can be
//! tested end to end on Linux (the "DLL side" is simulated by calling [`Shm::set_consumer`]).

use std::sync::Arc;

use crate::shm::{Shm, REGION_BYTES};

/// Heap-backed, initialised region. Cheap to clone (shared); same API as the Windows
/// `ShmMapping`: [`MemShm::shm`].
#[derive(Clone)]
pub struct MemShm {
    inner: Arc<Inner>,
}

struct Inner {
    // Keeps the allocation alive; u64 for 8-byte alignment. Never accessed except through `shm`.
    _mem: Box<[u64]>,
    shm: Shm,
}

impl MemShm {
    pub fn new() -> Self {
        let mut mem = vec![0u64; REGION_BYTES / 8 + 1].into_boxed_slice();
        // SAFETY: the box is owned by `Inner` and never moved or freed while `shm` lives.
        let shm = unsafe { Shm::from_raw(mem.as_mut_ptr() as *mut u8) };
        shm.init();
        MemShm { inner: Arc::new(Inner { _mem: mem, shm }) }
    }

    pub fn shm(&self) -> &Shm {
        &self.inner.shm
    }

    /// Mirrors `ShmMapping::open()` (always available in memory).
    pub fn open() -> Option<Self> {
        Some(Self::new())
    }
}

impl Default for MemShm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::{nv12_len, ReadResult};

    #[test]
    fn round_trip() {
        let m = MemShm::new();
        let m2 = m.clone();
        assert!(m.shm().is_valid());
        m.shm().set_consumer(true, 640, 360, 30);
        assert_eq!(m2.shm().wanted(), Some((640, 360, 30)));
        let frame = vec![9u8; nv12_len(640, 360)];
        assert!(m.shm().write_frame(640, 360, 1, &frame));
        let mut out = vec![0u8; frame.len()];
        assert!(matches!(m2.shm().read_frame(0, 640, 360, &mut out), ReadResult::Frame(_)));
        assert_eq!(out, frame);
    }
}
