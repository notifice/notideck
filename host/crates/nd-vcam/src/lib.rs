// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual camera sinks (SPEC §7.3, ADR 0005).
//!
//! * [`shm`] — shared-memory contract with the Windows media source DLL (`nd-vcam-dll`).
//! * [`registration`] — platform-independent install/registration logic (paths, hashing, state,
//!   with the registry abstracted away so it can be unit-tested).
//! * `win` (Windows only) — the real mapping ([`ShmMapping`]), HKLM registration
//!   ([`install()`], [`uninstall`], [`install_state`]) and [`VirtualCamera`].
//! * `mem` (other platforms) — [`MemShm`], the in-memory frame slot (fed to the Linux sink).
//! * [`placeholder`] — the "phone not connected" frame (NV12).
//! * `v4l2` (Linux only) — the v4l2loopback sink.

pub mod placeholder;
pub mod registration;
pub mod shm;

#[cfg(target_os = "linux")]
pub mod v4l2;

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{install, install_state, uninstall, Scope, ShmMapping, VirtualCamera};

#[cfg(not(windows))]
pub mod mem;
#[cfg(not(windows))]
pub use mem::MemShm;

pub use registration::InstallState;

/// Anything that exposes the shared region: the real Windows mapping or [`MemShm`].
/// Lets the DLL's frame logic be tested without a real section.
pub trait HasShm: Send + Sync {
    fn shm(&self) -> &shm::Shm;
}

#[cfg(windows)]
impl HasShm for ShmMapping {
    fn shm(&self) -> &shm::Shm {
        ShmMapping::shm(self)
    }
}

#[cfg(not(windows))]
impl HasShm for MemShm {
    fn shm(&self) -> &shm::Shm {
        MemShm::shm(self)
    }
}
