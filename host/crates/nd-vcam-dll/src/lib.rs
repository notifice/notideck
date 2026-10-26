// SPDX-License-Identifier: GPL-3.0-or-later
//! Media Foundation media source for the Notideck virtual camera (ADR 0005). Windows only:
//! on other platforms this crate only contains the platform-independent frame logic.
//!
//! The Windows Camera Frame Server loads this DLL (registered under HKLM by `nd_vcam::install`),
//! calls [`DllGetClassObject`] with `nd_vcam::shm::CLSID_STR` and activates the returned object.
//! Structure follows Microsoft's VirtualCamera sample and smourier/VCamSample: one object that is
//! both the `IMFActivate` and the `IMFMediaSourceEx`, one `IMFMediaStream2`.

pub mod frames;
pub use nd_vcam::placeholder;

#[cfg(windows)]
mod attrs;
#[cfg(windows)]
mod exports;
#[cfg(windows)]
pub mod log;
#[cfg(windows)]
mod source;
#[cfg(windows)]
mod stream;

#[cfg(windows)]
pub use exports::{DllCanUnloadNow, DllGetClassObject};
