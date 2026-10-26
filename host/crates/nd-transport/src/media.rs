// SPDX-License-Identifier: GPL-3.0-or-later
//! Media connections (ADR 0005): a second TLS connection to the control port whose first frame
//! is `MediaAttach{session_id}`. After `MediaAttached{ok}` it carries only phone -> host
//! [`MediaPacket`]s.

use std::net::SocketAddr;
use std::time::Duration;

use nd_proto::framing::write_frame;
use nd_proto::media::{read_packet, MediaPacket};
use nd_proto::v1::{MediaAttached, MediaKind};
use nd_proto::{Envelope, Msg};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::timeout;
use tokio_rustls::server::TlsStream;

use crate::error::Result;
use crate::pairing::Fingerprint;
use crate::session::ServerCtx;
use crate::trust::Link;

/// No packet for this long ends the stream.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// An accepted media connection. Dropping it (or [`MediaStream::close`]) closes the socket.
pub struct MediaStream {
    pub session_id: String,
    pub fp: Fingerprint,
    pub link: Link,
    /// Round 25: video (camera) or audio (mic).
    pub kind: MediaKind,
    stream: TlsStream<TcpStream>,
    closed: watch::Receiver<bool>,
    done: bool,
}

impl std::fmt::Debug for MediaStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaStream")
            .field("session_id", &self.session_id)
            .field("link", &self.link)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl MediaStream {
    /// Next packet. `Ok(None)` when the phone closed the stream, it was superseded, its control
    /// session ended, or nothing arrived for ~10 s. Not cancel-safe: after a dropped future the
    /// stream may have lost sync, so only use it in a loop that ends when this future is dropped.
    pub async fn next_packet(&mut self) -> std::io::Result<Option<MediaPacket>> {
        if self.done || *self.closed.borrow() {
            self.done = true;
            return Ok(None);
        }
        let res = tokio::select! {
            biased;
            _ = self.closed.changed() => Ok(None),
            r = timeout(READ_TIMEOUT, read_packet(&mut self.stream)) => match r {
                Ok(r) => r,
                Err(_) => Ok(None),
            },
        };
        if !matches!(res, Ok(Some(_))) {
            self.done = true;
        }
        res
    }

    /// Close the connection.
    pub fn close(self) {}
}

async fn answer(tls: &mut TlsStream<TcpStream>, ok: bool, error: &str) -> bool {
    let env = Envelope::new(0, Msg::MediaAttached(MediaAttached { ok, error: error.to_string() }));
    matches!(timeout(WRITE_TIMEOUT, write_frame(tls, &env)).await, Ok(Ok(())))
}

async fn refuse(mut tls: TlsStream<TcpStream>, error: &str) -> Result<()> {
    let _ = answer(&mut tls, false, error).await;
    let _ = timeout(Duration::from_secs(2), tokio::io::AsyncWriteExt::shutdown(&mut tls)).await;
    Ok(())
}

pub(crate) async fn handle_attach(
    ctx: &ServerCtx,
    mut tls: TlsStream<TcpStream>,
    fp: Fingerprint,
    remote: SocketAddr,
    session_id: String,
    kind: MediaKind,
) -> Result<()> {
    let Some(session) = ctx.shared.session_for_media(&session_id, &fp) else {
        // Same answer for "unknown id" and "other phone's id": reveal nothing.
        tracing::info!(%remote, fp = %fp.short(), "Media attach refused: no such session for this phone");
        return refuse(tls, "unknown session").await;
    };
    let (tx, unsupported) = match kind {
        MediaKind::Video => (&ctx.media_tx, "camera not supported by this PC"),
        MediaKind::Audio => (&ctx.audio_tx, "microphone not supported by this PC"),
    };
    if tx.is_closed() {
        return refuse(tls, unsupported).await;
    }
    let Ok(permit) = tx.try_reserve() else {
        return refuse(tls, "media receiver busy").await;
    };
    let closed = ctx.shared.attach_media(&session_id, kind);
    if !answer(&mut tls, true, "").await {
        return Ok(());
    }
    // The control session may have ended while we were answering.
    if !session.is_connected() {
        ctx.shared.detach_media_if_closed(&session_id, kind);
        return Ok(());
    }
    tracing::info!(session_id = %session_id, link = ?session.link(), ?kind, "Media stream attached");
    permit.send(MediaStream { session_id, fp, link: (ctx.classify)(&remote), kind, stream: tls, closed, done: false });
    Ok(())
}
