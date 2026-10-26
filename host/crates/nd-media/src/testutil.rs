// SPDX-License-Identifier: GPL-3.0-or-later
//! Test helpers: synthetic frames and an OpenH264 encoder producing `MediaPacket`s.

use crate::{nal_units, MediaPacket, Nv12Frame};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod};
use openh264::formats::YUVBuffer;

/// I420 buffer with a horizontal luma gradient and constant chroma.
pub fn gradient_yuv(w: usize, h: usize, u: u8, v: u8) -> YUVBuffer {
    let mut buf = vec![0u8; w * h * 3 / 2];
    for r in 0..h {
        for c in 0..w {
            buf[r * w + c] = 40 + (c * 160 / w) as u8;
        }
    }
    buf[w * h..w * h + w * h / 4].fill(u);
    buf[w * h + w * h / 4..].fill(v);
    YUVBuffer::from_vec(buf, w, h)
}

pub struct TestEncoder {
    enc: Encoder,
    n: u64,
}

impl TestEncoder {
    pub fn new() -> Self {
        let cfg = EncoderConfig::new()
            .bitrate(BitRate::from_bps(4_000_000))
            .max_frame_rate(FrameRate::from_hz(30.0))
            .intra_frame_period(IntraFramePeriod::from_num_frames(30))
            .skip_frames(true);
        Self { enc: Encoder::with_api_config(openh264::OpenH264API::from_source(), cfg).unwrap(), n: 0 }
    }

    /// Encode one frame; returns [CONFIG packet (if SPS/PPS present)] + frame packet.
    pub fn encode(&mut self, yuv: &YUVBuffer) -> Vec<MediaPacket> {
        let bs = self.enc.encode(yuv).unwrap().to_vec();
        let pts = self.n * 33_333;
        self.n += 1;
        let mut cfg = Vec::new();
        let mut frame = Vec::new();
        let mut key = false;
        for nal in nal_units(&bs) {
            let dst = match nal[0] & 0x1f {
                7 | 8 => &mut cfg,
                t => {
                    key |= t == 5;
                    &mut frame
                }
            };
            dst.extend_from_slice(&[0, 0, 0, 1]);
            dst.extend_from_slice(nal);
        }
        let mut out = Vec::new();
        if !cfg.is_empty() {
            out.push(MediaPacket::new(0, true, false, cfg));
        }
        if !frame.is_empty() {
            out.push(MediaPacket::new(pts, false, key, frame));
        }
        out
    }
}

pub fn mean_yuv(f: &Nv12Frame) -> (f64, f64, f64) {
    let n = (f.width * f.height) as usize;
    let y = f.data[..n].iter().map(|&b| b as f64).sum::<f64>() / n as f64;
    let uv = &f.data[n..];
    let u = uv.iter().step_by(2).map(|&b| b as f64).sum::<f64>() / (n / 4) as f64;
    let v = uv.iter().skip(1).step_by(2).map(|&b| b as f64).sum::<f64>() / (n / 4) as f64;
    (y, u, v)
}
