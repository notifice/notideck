// SPDX-License-Identifier: GPL-3.0-or-later
//! H.264 decode and NV12 frame handling for the virtual camera (SPEC §7.1, ADR 0005).

pub use nd_proto::media::MediaPacket;

/// One decoded frame, NV12, tightly packed: Y plane (`width*height`) then interleaved UV
/// (`width*height/2`). Width and height are even.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    pub pts_us: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("no H.264 decoder available: {0}")]
    Unavailable(String),
    /// Corrupt or undecodable input; the caller should request a keyframe and continue.
    #[error("decode error: {0}")]
    Decode(String),
}

/// Streaming H.264 (Annex-B) decoder. Feed every packet in order, CONFIG packets included.
pub trait H264Decoder: Send {
    /// Returns zero or more frames that became ready. Must not block for long (< 1 frame time).
    fn decode(&mut self, packet: &MediaPacket) -> Result<Vec<Nv12Frame>, MediaError>;
    /// Drop all state (new stream, new SPS/PPS expected).
    fn reset(&mut self);
    /// Short name for logs and the selftest, e.g. "mf", "mf-dxva", "openh264".
    fn name(&self) -> &'static str;
}

mod annexb;
mod scale;
pub use annexb::{contains_nal_type, nal_units};
pub use scale::scale_nv12;

#[cfg(any(feature = "openh264", not(windows)))]
mod openh264_dec;
#[cfg(any(feature = "openh264", not(windows)))]
pub use openh264_dec::OpenH264Decoder;

#[cfg(windows)]
mod mf;
#[cfg(windows)]
pub use mf::MfDecoder;

#[cfg(test)]
mod testutil;

/// Best available decoder for this platform. Windows: Media Foundation, falling back to OpenH264
/// when compiled in (`openh264` feature); elsewhere OpenH264.
pub fn default_decoder() -> Result<Box<dyn H264Decoder>, MediaError> {
    #[cfg(windows)]
    {
        match MfDecoder::new() {
            Ok(d) => Ok(Box::new(d)),
            #[cfg(feature = "openh264")]
            Err(e) => {
                tracing::warn!("MF H.264 decoder unavailable ({e}); falling back to OpenH264");
                Ok(Box::new(OpenH264Decoder::new()?))
            }
            #[cfg(not(feature = "openh264"))]
            Err(e) => Err(e),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(Box::new(OpenH264Decoder::new()?))
    }
}

/// Reject payloads that cannot be Annex-B H.264 before handing them to a decoder (some decoders
/// silently swallow garbage).
pub(crate) fn validate_annexb(data: &[u8]) -> Result<(), MediaError> {
    let mut n = 0;
    for nal in nal_units(data) {
        n += 1;
        let h = nal[0];
        let ty = h & 0x1f;
        if h & 0x80 != 0 || ty == 0 || ty > 12 {
            return Err(MediaError::Decode(format!("invalid NAL header {h:#04x}")));
        }
    }
    if n == 0 {
        return Err(MediaError::Decode("no Annex-B start code / NAL units in payload".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
