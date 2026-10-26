// SPDX-License-Identifier: GPL-3.0-or-later
//! Transport error definitions.

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("framing error: {0}")]
    Framing(#[from] nd_proto::framing::FrameError),

    #[error("handshake error: {0}")]
    Handshake(String),

    #[error("session closed")]
    SessionClosed,

    #[error("adb error: {0}")]
    Adb(String),

    #[error("no active session")]
    NoActiveSession,

    #[error("tls: {0}")]
    Tls(String),

    #[error("send error: {0}")]
    Send(String),
}

pub type Result<T> = std::result::Result<T, TransportError>;
