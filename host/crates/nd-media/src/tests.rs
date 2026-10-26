// SPDX-License-Identifier: GPL-3.0-or-later
use crate::testutil::*;
use crate::*;
use std::time::Instant;

fn decoders() -> Vec<Box<dyn H264Decoder>> {
    #[allow(unused_mut)]
    let mut v = vec![default_decoder().expect("default decoder")];
    #[cfg(all(windows, feature = "openh264"))]
    v.push(Box::new(OpenH264Decoder::new().unwrap()));
    v
}

fn clip(w: usize, h: usize, n: usize) -> Vec<MediaPacket> {
    let mut enc = TestEncoder::new();
    let yuv = gradient_yuv(w, h, 90, 200);
    (0..n).flat_map(|_| enc.encode(&yuv)).collect()
}

fn feed(dec: &mut dyn H264Decoder, pkts: &[MediaPacket]) -> Vec<Nv12Frame> {
    pkts.iter().flat_map(|p| dec.decode(p).expect("decode")).collect()
}

fn check_plausible(f: &Nv12Frame) {
    let (y, u, v) = mean_yuv(f);
    assert!((y - 119.5).abs() < 6.0, "Y mean {y}");
    assert!((u - 90.0).abs() < 4.0, "U mean {u}");
    assert!((v - 200.0).abs() < 4.0, "V mean {v}");
    // Horizontal gradient: left darker than right.
    let w = f.width as usize;
    assert!(f.data[8] + 40 < f.data[w - 8], "gradient lost");
}

#[test]
fn decodes_640x360() {
    for mut d in decoders() {
        let frames = feed(d.as_mut(), &clip(640, 360, 10));
        assert!(frames.len() >= 8, "{}: only {} frames", d.name(), frames.len());
        for f in &frames {
            assert_eq!((f.width, f.height), (640, 360), "{}", d.name());
            assert_eq!(f.data.len(), 640 * 360 * 3 / 2);
            check_plausible(f);
        }
        println!("{}: 640x360 ok ({} frames)", d.name(), frames.len());
    }
}

#[test]
fn decodes_1080p_cropped_and_times_it() {
    let pkts = clip(1920, 1080, 30);
    for mut d in decoders() {
        let mut frames = Vec::new();
        let mut worst = 0.0f64;
        let t0 = Instant::now();
        for p in &pkts {
            let t = Instant::now();
            frames.extend(d.decode(p).expect("decode"));
            worst = worst.max(t.elapsed().as_secs_f64() * 1e3);
        }
        let total = t0.elapsed().as_secs_f64() * 1e3;
        println!(
            "{}: 1080p {} frames, avg {:.2} ms/packet, worst {:.2} ms",
            d.name(),
            frames.len(),
            total / pkts.len() as f64,
            worst
        );
        assert!(frames.len() >= 28, "{}: only {} frames", d.name(), frames.len());
        for f in &frames {
            assert_eq!((f.width, f.height), (1920, 1080), "{}", d.name());
            check_plausible(f);
        }
        // pts passes through (frame N has N*33333 us).
        assert_eq!(frames[3].pts_us / 1000, 3 * 33333 / 1000);
    }
}

#[test]
fn resolution_switch_mid_stream() {
    for mut d in decoders() {
        let mut pkts = clip(640, 360, 5);
        pkts.extend(clip(1280, 720, 5));
        pkts.extend(clip(640, 360, 3));
        let frames = feed(d.as_mut(), &pkts);
        let n = |w, h| frames.iter().filter(|f| (f.width, f.height) == (w, h)).count();
        println!("{}: switch -> {}x640p {}x720p {}", d.name(), n(640, 360), n(1280, 720), frames.len());
        assert!(n(640, 360) >= 6 && n(1280, 720) >= 4, "{}: {} / {}", d.name(), n(640, 360), n(1280, 720));
        assert_eq!(frames.len(), n(640, 360) + n(1280, 720));
        // Order: first frames small, middle large.
        assert_eq!((frames[0].width, frames[0].height), (640, 360));
        for f in &frames {
            check_plausible(f);
        }
    }
}

#[test]
fn garbage_errors_then_recovers_after_reset() {
    for mut d in decoders() {
        let good = clip(640, 360, 4);
        assert!(!feed(d.as_mut(), &good).is_empty());
        let garbage = [
            MediaPacket::new(1, false, false, vec![0x13u8, 0x37, 0xde, 0xad, 0xbe, 0xef, 0x00, 0x42]),
            MediaPacket::new(2, false, true, vec![0, 0, 0, 1, 0xff, 0xee, 0xdd]),
            MediaPacket::new(3, false, false, Vec::<u8>::new()),
        ];
        for g in &garbage {
            assert!(matches!(d.decode(g), Err(MediaError::Decode(_))), "{}: garbage accepted", d.name());
        }
        // Corrupted slice data must not panic (error or nothing is fine).
        let mut bad = good.last().unwrap().data.to_vec();
        for b in bad.iter_mut().skip(8) {
            *b = b.wrapping_mul(31).wrapping_add(7);
        }
        let _ = d.decode(&MediaPacket::new(9, false, false, bad));
        d.reset();
        let frames = feed(d.as_mut(), &clip(640, 360, 4));
        assert!(frames.len() >= 3, "{}: no recovery ({} frames)", d.name(), frames.len());
        frames.iter().for_each(check_plausible);
    }
}

#[test]
fn config_separate_and_key_frame_without_inline_sps() {
    // clip() already yields CONFIG as its own packet and frames without inline SPS/PPS.
    let pkts = clip(640, 360, 2);
    assert!(pkts[0].header.config && pkts[1].header.key_frame);
    assert!(!contains_nal_type(&pkts[1].data, 7));
    let mut d = default_decoder().unwrap();
    assert!(!feed(d.as_mut(), &pkts).is_empty());
}

// ---- scaler ----

fn solid(w: u32, h: u32, y: u8, u: u8, v: u8) -> Nv12Frame {
    let mut data = vec![y; (w * h) as usize];
    for _ in 0..(w * h / 4) {
        data.push(u);
        data.push(v);
    }
    Nv12Frame { width: w, height: h, pts_us: 77, data }
}

#[test]
fn scale_identity() {
    let f = solid(640, 360, 100, 60, 200);
    assert_eq!(scale_nv12(&f, 640, 360), f);
}

#[test]
fn scale_exact_sizes_and_pts() {
    let f = solid(1920, 1080, 100, 60, 200);
    for (w, h) in [(1280, 720), (640, 360), (1920, 1080), (800, 600), (320, 240), (3840, 2160), (500, 500)] {
        let o = scale_nv12(&f, w, h);
        assert_eq!((o.width, o.height), (w, h));
        assert_eq!(o.data.len(), (w * h * 3 / 2) as usize);
        assert_eq!(o.pts_us, 77);
    }
}

#[test]
fn scale_letterbox_and_pillarbox_black() {
    let f = solid(1920, 1080, 200, 60, 200);
    // 16:9 into 4:3 -> bars top and bottom.
    let o = scale_nv12(&f, 800, 600);
    let w = 800usize;
    assert_eq!(o.data[0], 16);
    assert_eq!(o.data[w * 599 + 10], 16);
    assert_eq!(o.data[w * 300 + 400], 200);
    let uv = &o.data[w * 600..];
    assert_eq!((uv[0], uv[1]), (128, 128));
    assert_eq!((uv[w * 150 + 400], uv[w * 150 + 401]), (60, 200));
    // 16:9 into 9:16 -> pillarbox in portrait means bars top/bottom; 4:3 source into 16:9 -> side bars.
    let s = solid(640, 480, 200, 60, 200);
    let o = scale_nv12(&s, 1280, 720);
    let w = 1280usize;
    assert_eq!(o.data[w * 360 + 5], 16);
    assert_eq!(o.data[w * 360 + 1275], 16);
    assert_eq!(o.data[w * 360 + 640], 200);
    // Bar width = (1280 - 960)/2 = 160.
    assert_eq!(o.data[w * 360 + 158], 16);
    assert_eq!(o.data[w * 360 + 162], 200);
}

#[test]
fn scale_preserves_gradient_down_and_up() {
    let mut f = solid(640, 360, 0, 128, 128);
    for r in 0..360 {
        for c in 0..640 {
            f.data[r * 640 + c] = (c * 255 / 639) as u8;
        }
    }
    for (w, h) in [(160u32, 90u32), (1280, 720)] {
        let o = scale_nv12(&f, w, h);
        let row = &o.data[(h as usize / 2) * w as usize..][..w as usize];
        assert!(row.windows(2).all(|p| p[1] as i32 + 2 >= p[0] as i32), "monotonic");
        let exp = |c: usize| (c * 255 / (w as usize - 1)) as i32;
        for c in [w as usize / 8, w as usize / 2, w as usize * 7 / 8] {
            assert!((row[c] as i32 - exp(c)).abs() < 6, "{w}: col {c} = {} vs {}", row[c], exp(c));
        }
    }
}

#[test]
fn scale_benchmark() {
    let mut f = solid(1920, 1080, 100, 60, 200);
    for (i, b) in f.data.iter_mut().enumerate().take(1920 * 1080) {
        *b = (i % 251) as u8;
    }
    for (w, h) in [(1280u32, 720u32), (640, 480), (1920, 1080), (3840, 2160)] {
        let n = 10;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(scale_nv12(&f, w, h));
        }
        println!("scale 1920x1080 -> {w}x{h}: {:.2} ms/frame", t.elapsed().as_secs_f64() * 1e3 / n as f64);
    }
}

/// A decoder that sits on frames adds a frame time of latency per buffered frame: every
/// non-config input must produce its output before the next one is fed.
#[test]
fn no_output_lag_n_in_n_out() {
    let pkts = clip(1280, 720, 40);
    for mut d in decoders() {
        let (mut fed, mut out) = (0usize, 0usize);
        let mut max_lag = 0usize;
        for p in &pkts {
            let n = d.decode(p).expect("decode").len();
            if !p.header.config {
                fed += 1;
            }
            out += n;
            max_lag = max_lag.max(fed - out);
        }
        println!("{}: fed {fed} out {out} max lag {max_lag}", d.name());
        assert_eq!(out, fed, "{}: decoder holds back frames", d.name());
        assert_eq!(max_lag, 0, "{}: output lagged input", d.name());
    }
}

/// The phone sends 1280x720; Windows Camera may ask for 1080p (upscale) or 360p (downscale).
#[test]
fn scale_benchmark_from_720p() {
    let mut f = solid(1280, 720, 100, 60, 200);
    for (i, b) in f.data.iter_mut().enumerate().take(1280 * 720) {
        *b = (i % 251) as u8;
    }
    for (w, h) in [(1920u32, 1080u32), (640, 360)] {
        let n = 10;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(scale_nv12(&f, w, h));
        }
        println!("scale 1280x720 -> {w}x{h}: {:.2} ms/frame", t.elapsed().as_secs_f64() * 1e3 / n as f64);
    }
}
