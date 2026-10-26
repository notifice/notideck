// SPDX-License-Identifier: GPL-3.0-or-later
//! Control channel framing: `u32` big-endian length prefix + protobuf `Envelope`.

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Envelope;

/// Upper bound for a single control frame. Icons are the largest payload.
pub const MAX_FRAME_LEN: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame too large: {0} bytes")]
    TooLarge(usize),
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
}

/// Encode an envelope into a complete frame (length prefix included).
pub fn encode_frame(env: &Envelope) -> Vec<u8> {
    let len = env.encoded_len();
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_be_bytes());
    env.encode(&mut buf).expect("Vec has capacity");
    buf
}

/// Decode one frame from the front of `buf`.
/// Returns `Ok(None)` if more bytes are needed, otherwise the envelope and bytes consumed.
pub fn decode_frame(buf: &[u8]) -> Result<Option<(Envelope, usize)>, FrameError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let env = Envelope::decode(&buf[4..4 + len])?;
    Ok(Some((env, 4 + len)))
}

/// Read one frame. Returns `Ok(None)` on clean EOF before a length prefix.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Envelope>, FrameError> {
    let mut hdr = [0u8; 4];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(hdr) as usize;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(Envelope::decode(body.as_slice())?))
}

/// Write one frame and flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, env: &Envelope) -> Result<(), FrameError> {
    w.write_all(&encode_frame(env)).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::{Hello, Ping};
    use crate::Msg;

    #[test]
    fn roundtrip_sync() {
        let env = Envelope::new(7, Msg::Ping(Ping { nonce: 42 }));
        let frame = encode_frame(&env);
        assert_eq!(&frame[..4], &((frame.len() - 4) as u32).to_be_bytes());
        let (got, used) = decode_frame(&frame).unwrap().unwrap();
        assert_eq!(got, env);
        assert_eq!(used, frame.len());
        assert!(decode_frame(&frame[..frame.len() - 1]).unwrap().is_none());
    }

    #[test]
    fn rejects_oversize() {
        let buf = ((MAX_FRAME_LEN + 1) as u32).to_be_bytes();
        assert!(matches!(decode_frame(&buf), Err(FrameError::TooLarge(_))));
    }

    #[tokio::test]
    async fn roundtrip_async() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let env = Envelope::new(1, Msg::Hello(Hello { proto_version: 1, device_id: "x".into(), ..Default::default() }));
        write_frame(&mut a, &env).await.unwrap();
        drop(a);
        assert_eq!(read_frame(&mut b).await.unwrap(), Some(env));
        assert_eq!(read_frame(&mut b).await.unwrap(), None);
    }
}
