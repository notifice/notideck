// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenH264 (software) decoder backend.

use crate::{validate_annexb, H264Decoder, MediaError, MediaPacket, Nv12Frame};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

pub struct OpenH264Decoder {
    dec: Decoder,
}

impl OpenH264Decoder {
    pub fn new() -> Result<Self, MediaError> {
        let dec = Decoder::new().map_err(|e| MediaError::Unavailable(format!("openh264: {e}")))?;
        Ok(Self { dec })
    }
}

impl H264Decoder for OpenH264Decoder {
    fn decode(&mut self, packet: &MediaPacket) -> Result<Vec<Nv12Frame>, MediaError> {
        validate_annexb(&packet.data)?;
        let yuv = self.dec.decode(&packet.data).map_err(|e| MediaError::Decode(e.to_string()))?;
        let Some(yuv) = yuv else { return Ok(vec![]) };
        let (w, h) = yuv.dimensions();
        let (w, h) = (w & !1, h & !1);
        if w == 0 || h == 0 {
            return Ok(vec![]);
        }
        let (ys, us, vs) = yuv.strides();
        let (y, u, v) = (yuv.y(), yuv.u(), yuv.v());
        let mut data = Vec::with_capacity(w * h * 3 / 2);
        for r in 0..h {
            data.extend_from_slice(&y[r * ys..r * ys + w]);
        }
        for r in 0..h / 2 {
            let (ur, vr) = (&u[r * us..r * us + w / 2], &v[r * vs..r * vs + w / 2]);
            for (a, b) in ur.iter().zip(vr) {
                data.push(*a);
                data.push(*b);
            }
        }
        Ok(vec![Nv12Frame { width: w as u32, height: h as u32, pts_us: packet.header.pts_us, data }])
    }

    fn reset(&mut self) {
        if let Ok(d) = Decoder::new() {
            self.dec = d;
        }
    }

    fn name(&self) -> &'static str {
        "openh264"
    }
}
