// SPDX-License-Identifier: GPL-3.0-or-later
//! NV12 aspect-preserving fit (letterbox / pillarbox) with a separable triangle filter whose
//! support widens when downscaling (so >=2x downscales average instead of aliasing).

use crate::Nv12Frame;

const FP: i32 = 14;

struct Taps {
    start: Vec<u32>,
    count: Vec<u32>,
    offset: Vec<usize>,
    w: Vec<i16>,
}

fn taps(src: usize, dst: usize) -> Taps {
    let scale = src as f64 / dst as f64;
    let support = scale.max(1.0);
    let mut t = Taps { start: vec![], count: vec![], offset: vec![], w: vec![] };
    for i in 0..dst {
        let c = (i as f64 + 0.5) * scale;
        let lo = ((c - support).floor() as i64).max(0) as usize;
        let hi = (((c + support).ceil()) as i64).min(src as i64) as usize;
        let mut ws: Vec<f64> = (lo..hi).map(|x| (1.0 - ((x as f64 + 0.5 - c).abs() / support)).max(0.0)).collect();
        let sum: f64 = ws.iter().sum();
        if sum <= 0.0 {
            let nearest = ((c as usize).min(src - 1)).max(lo.min(src - 1));
            t.start.push(nearest as u32);
            t.count.push(1);
            t.offset.push(t.w.len());
            t.w.push(1 << FP);
            continue;
        }
        for w in &mut ws {
            *w /= sum;
        }
        let mut q: Vec<i32> = ws.iter().map(|w| (w * (1 << FP) as f64).round() as i32).collect();
        let diff = (1 << FP) - q.iter().sum::<i32>();
        let mid = q.len() / 2;
        q[mid] += diff;
        t.start.push(lo as u32);
        t.count.push(q.len() as u32);
        t.offset.push(t.w.len());
        t.w.extend(q.iter().map(|&v| v as i16));
    }
    t
}

/// Resize an interleaved plane (`ch` bytes per pixel) into `dst` (rows of `dst_stride` bytes).
#[allow(clippy::too_many_arguments)]
fn resize_plane(src: &[u8], sw: usize, sh: usize, ch: usize, dst: &mut [u8], dst_stride: usize, dw: usize, dh: usize) {
    let tx = taps(sw, dw);
    let ty = taps(sh, dh);
    // Only rows touched by the vertical taps need the horizontal pass.
    let row_len = dw * ch;
    let mut tmp = vec![0u8; sh * row_len];
    let mut needed = vec![false; sh];
    for i in 0..dh {
        for r in ty.start[i]..ty.start[i] + ty.count[i] {
            needed[r as usize] = true;
        }
    }
    for (y, _) in needed.iter().enumerate().filter(|(_, n)| **n) {
        let srow = &src[y * sw * ch..(y + 1) * sw * ch];
        let trow = &mut tmp[y * row_len..(y + 1) * row_len];
        for x in 0..dw {
            let (s, c, o) = (tx.start[x] as usize, tx.count[x] as usize, tx.offset[x]);
            let ws = &tx.w[o..o + c];
            for k in 0..ch {
                let mut acc = 1 << (FP - 1);
                for (j, &w) in ws.iter().enumerate() {
                    acc += w as i32 * srow[(s + j) * ch + k] as i32;
                }
                trow[x * ch + k] = (acc >> FP).clamp(0, 255) as u8;
            }
        }
    }
    let mut acc = vec![0i32; row_len];
    for y in 0..dh {
        let (s, c, o) = (ty.start[y] as usize, ty.count[y] as usize, ty.offset[y]);
        acc.iter_mut().for_each(|a| *a = 1 << (FP - 1));
        for j in 0..c {
            let w = ty.w[o + j] as i32;
            let row = &tmp[(s + j) * row_len..(s + j + 1) * row_len];
            for (a, &p) in acc.iter_mut().zip(row) {
                *a += w * p as i32;
            }
        }
        let out = &mut dst[y * dst_stride..y * dst_stride + row_len];
        for (o, a) in out.iter_mut().zip(&acc) {
            *o = (a >> FP).clamp(0, 255) as u8;
        }
    }
}

/// Fit `src` into a `width`x`height` frame keeping its aspect ratio; the rest is black
/// (Y 16, UV 128). Width/height are rounded down to even. Same-size input is copied unchanged.
pub fn scale_nv12(src: &Nv12Frame, width: u32, height: u32) -> Nv12Frame {
    let (w, h) = ((width & !1).max(2) as usize, (height & !1).max(2) as usize);
    let (sw, sh) = (src.width as usize, src.height as usize);
    if sw == w && sh == h {
        return src.clone();
    }
    let mut data = vec![0u8; w * h * 3 / 2];
    let (yp, uvp) = data.split_at_mut(w * h);
    yp.fill(16);
    uvp.fill(128);
    if sw >= 2 && sh >= 2 && src.data.len() >= sw * sh * 3 / 2 {
        let scale = (w as f64 / sw as f64).min(h as f64 / sh as f64);
        let dw = (((sw as f64 * scale).round() as usize) & !1).clamp(2, w);
        let dh = (((sh as f64 * scale).round() as usize) & !1).clamp(2, h);
        let x0 = ((w - dw) / 2) & !1;
        let y0 = ((h - dh) / 2) & !1;
        let (sy, suv) = src.data.split_at(sw * sh);
        resize_plane(sy, sw, sh, 1, &mut yp[y0 * w + x0..], w, dw, dh);
        resize_plane(suv, sw / 2, sh / 2, 2, &mut uvp[(y0 / 2) * w + x0..], w, dw / 2, dh / 2);
    }
    Nv12Frame { width: w as u32, height: h as u32, pts_us: src.pts_us, data }
}
