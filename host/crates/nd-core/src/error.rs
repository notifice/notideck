// SPDX-License-Identifier: GPL-3.0-or-later
//! Error types for nd-core.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("platform error: {0}")]
    Platform(#[from] nd_platform::PlatformError),

    #[error("profile error: {0}")]
    Profile(String),

    #[error("page not found: {0}")]
    PageNotFound(String),

    #[error("control '{control_id}' not found on page '{page_id}'")]
    ControlNotFound { page_id: String, control_id: String },

    #[error("camera: {0}")]
    Camera(String),
    #[error("microphone: {0}")]
    Mic(String),

    #[error("OBS: {0}")]
    Obs(String),

    #[error("unsupported action: {0}")]
    UnsupportedAction(String),

    #[error("invalid migration from v{from} to v{to}: {message}")]
    MigrationError { from: u32, to: u32, message: String },

    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;
