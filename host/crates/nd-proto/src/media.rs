// SPDX-License-Identifier: GPL-3.0-or-later
//! Media channel framing (scrcpy-style, SPEC §4.5):
//! `pts: u64 BE` | `flags: u32 BE`... compacted into 12 bytes as below.
//!
//! Layout (12 bytes, big-endian):
//! * bytes 0..8  — `pts_and_flags: u64`. Top 2 bits are flags, low 62 bits are PTS in µs.
//!   * bit 63: CONFIG packet (SPS/PPS, no PTS)
//!   * bit 62: KEY_FRAME
//! * bytes 8..12 — `len: u32`, payload length (H.264 Annex-B).

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MEDIA_HEADER_LEN: usize = 12;

/// Largest accepted media payload (one encoded frame). Anything bigger is a protocol error.
pub const MAX_MEDIA_PAYLOAD: u32 = 8 * 1024 * 1024;

const FLAG_CONFIG: u64 = 1 << 63;
const FLAG_KEY_FRAME: u64 = 1 << 62;
const PTS_MASK: u64 = FLAG_KEY_FRAME - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub pts_us: u64,
    pub config: bool,
    pub key_frame: bool,
    pub len: u32,
}

impl MediaHeader {
    pub fn encode(&self) -> [u8; MEDIA_HEADER_LEN] {
        let mut v = self.pts_us & PTS_MASK;
        if self.config {
            v |= FLAG_CONFIG;
        }
        if self.key_frame {
            v |= FLAG_KEY_FRAME;
        }
        let mut out = [0u8; MEDIA_HEADER_LEN];
        out[..8].copy_from_slice(&v.to_be_bytes());
        out[8..].copy_from_slice(&self.len.to_be_bytes());
        out
    }

    pub fn decode(b: &[u8; MEDIA_HEADER_LEN]) -> Self {
        let v = u64::from_be_bytes(b[..8].try_into().unwrap());
        MediaHeader {
            pts_us: v & PTS_MASK,
            config: v & FLAG_CONFIG != 0,
            key_frame: v & FLAG_KEY_FRAME != 0,
            len: u32::from_be_bytes(b[8..].try_into().unwrap()),
        }
    }
}

/// One media packet: header plus H.264 Annex-B payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPacket {
    pub header: MediaHeader,
    pub data: Bytes,
}

impl MediaPacket {
    pub fn new(pts_us: u64, config: bool, key_frame: bool, data: impl Into<Bytes>) -> Self {
        let data = data.into();
        MediaPacket { header: MediaHeader { pts_us, config, key_frame, len: data.len() as u32 }, data }
    }
}

/// Read one packet. `Ok(None)` on a clean EOF before a header.
pub async fn read_packet<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<MediaPacket>> {
    let mut hdr = [0u8; MEDIA_HEADER_LEN];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let header = MediaHeader::decode(&hdr);
    if header.len > MAX_MEDIA_PAYLOAD {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "media packet too large"));
    }
    let mut data = vec![0u8; header.len as usize];
    r.read_exact(&mut data).await?;
    Ok(Some(MediaPacket { header, data: data.into() }))
}

/// Write one packet (header `len` is taken from the payload).
pub async fn write_packet<W: AsyncWrite + Unpin>(w: &mut W, p: &MediaPacket) -> std::io::Result<()> {
    let header = MediaHeader { len: p.data.len() as u32, ..p.header };
    w.write_all(&header.encode()).await?;
    w.write_all(&p.data).await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let h = MediaHeader { pts_us: 123_456_789, config: false, key_frame: true, len: 4096 };
        assert_eq!(MediaHeader::decode(&h.encode()), h);
        let c = MediaHeader { pts_us: 0, config: true, key_frame: false, len: 30 };
        assert_eq!(MediaHeader::decode(&c.encode()), c);
    }

    #[tokio::test]
    async fn packet_roundtrip() {
        let p = MediaPacket::new(42, false, true, vec![0, 0, 0, 1, 0x65, 0xaa]);
        let mut buf = Vec::new();
        write_packet(&mut buf, &p).await.unwrap();
        let mut r = &buf[..];
        assert_eq!(read_packet(&mut r).await.unwrap(), Some(p));
        assert_eq!(read_packet(&mut r).await.unwrap(), None);
    }
}
