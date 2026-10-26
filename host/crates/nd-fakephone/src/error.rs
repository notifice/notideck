// SPDX-License-Identifier: GPL-3.0-or-later

use nd_proto::v1::GoodbyeReason;
use thiserror::Error;

/// Errors produced by `FakePhone` or `MockHost`.
#[derive(Debug, Error)]
pub enum FakePhoneError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("frame error: {0}")]
    Frame(#[from] nd_proto::framing::FrameError),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("connection closed by remote peer")]
    ConnectionClosed,

    #[error("host said goodbye ({reason:?}): {message}")]
    Goodbye { reason: GoodbyeReason, message: String },

    #[error("pairing rejected: {0}")]
    PairingRejected(String),

    #[error("timeout waiting for {0}")]
    Timeout(&'static str),

    #[error("unexpected message received: expected {expected}, got {got}")]
    UnexpectedMessage { expected: &'static str, got: String },

    #[error("channel send error")]
    ChannelClosed,

    #[error("other error: {0}")]
    Other(String),
}
