// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone-camera control surface used by the camera deck actions and live data (SPEC §6.4 Camera).
//!
//! The agent implements [`CameraActions`] with its camera manager; core only sees the trait.

/// What a camera key or dial needs to display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraSnapshot {
    /// The webcam is on: a test/manual stream runs, or an app uses the camera and it is not paused.
    pub active: bool,
    pub torch: bool,
    pub front_lens: bool,
    /// Zoom ratio, 1.0 = no zoom.
    pub zoom: f32,
}

impl Default for CameraSnapshot {
    fn default() -> Self {
        Self { active: false, torch: false, front_lens: false, zoom: MIN_ZOOM }
    }
}

pub const MIN_ZOOM: f32 = 1.0;
pub const MAX_ZOOM: f32 = 8.0;

/// Zoom ratio (1..8) to a 0..1 dial value.
pub fn zoom_to_value(zoom: f32) -> f32 {
    ((zoom - MIN_ZOOM) / (MAX_ZOOM - MIN_ZOOM)).clamp(0.0, 1.0)
}

/// Camera operations behind the deck actions. Methods are synchronous and quick.
pub trait CameraActions: Send + Sync {
    /// Start/stop the manual (test) stream, or pause/resume it while an app uses the camera.
    /// Returns whether the camera is now on.
    fn toggle(&self) -> Result<bool, String>;
    /// Switch between the back and front lens. Returns true when the front lens is now selected.
    fn switch_lens(&self) -> Result<bool, String>;
    /// Toggle the torch. Returns the new state.
    fn toggle_torch(&self) -> Result<bool, String>;
    /// Change the zoom ratio by `delta` (clamped to 1..8). Returns the new ratio.
    fn zoom_by(&self, delta: f32) -> Result<f32, String>;
    fn snapshot(&self) -> CameraSnapshot;
}

/// Used when no camera manager is attached (tests, headless without camera).
pub struct NullCamera;

impl CameraActions for NullCamera {
    fn toggle(&self) -> Result<bool, String> {
        Err("the webcam is not available".into())
    }
    fn switch_lens(&self) -> Result<bool, String> {
        Err("the webcam is not available".into())
    }
    fn toggle_torch(&self) -> Result<bool, String> {
        Err("the webcam is not available".into())
    }
    fn zoom_by(&self, _delta: f32) -> Result<f32, String> {
        Err("the webcam is not available".into())
    }
    fn snapshot(&self) -> CameraSnapshot {
        CameraSnapshot::default()
    }
}
