// SPDX-License-Identifier: GPL-3.0-or-later
//! Media connection -> decode thread -> scale -> shared memory.

use super::queue::{PacketQueue, Pop, QUEUE_CAP};
use super::stats::{Reporter, StatCounters, REPORT_INTERVAL};
use nd_media::{scale_nv12, H264Decoder, MediaError, Nv12Frame};
use nd_transport::MediaStream;
use nd_vcam::HasShm;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub type DecoderFactory = Arc<dyn Fn() -> Result<Box<dyn H264Decoder>, MediaError> + Send + Sync>;

/// The shared-memory region, once it exists (Windows: when the DLL has created it).
#[derive(Clone, Default)]
pub struct ShmSlot(Arc<Mutex<Option<Arc<dyn HasShm>>>>);

impl ShmSlot {
    pub fn get(&self) -> Option<Arc<dyn HasShm>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn set(&self, shm: Arc<dyn HasShm>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(shm);
    }
    pub fn is_set(&self) -> bool {
        self.get().is_some()
    }
}

/// Frame size the consumer wants (even numbers; 0 = none yet).
#[derive(Default)]
pub struct Target {
    pub w: AtomicU32,
    pub h: AtomicU32,
}

impl Target {
    pub fn set(&self, w: u32, h: u32) {
        self.w.store(w & !1, Ordering::Relaxed);
        self.h.store(h & !1, Ordering::Relaxed);
    }
    pub fn get(&self) -> (u32, u32) {
        (self.w.load(Ordering::Relaxed), self.h.load(Ordering::Relaxed))
    }
}

pub const PREVIEW_W: usize = 320;

/// Latest frame, downscaled to RGB, kept only while someone looks at the preview.
#[derive(Default)]
pub struct Preview {
    wanted_until_ms: AtomicU64,
    last_ms: AtomicU64,
    img: Mutex<Option<PreviewImage>>,
}

#[derive(Clone)]
pub struct PreviewImage {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

fn mono_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

impl Preview {
    /// Called by the API: keep producing previews for the next few seconds.
    pub fn request(&self) -> Option<PreviewImage> {
        self.wanted_until_ms.store(mono_ms() + 5000, Ordering::Relaxed);
        self.img.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn clear(&self) {
        *self.img.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn offer(&self, f: &Nv12Frame) {
        let now = mono_ms();
        if now > self.wanted_until_ms.load(Ordering::Relaxed) || now < self.last_ms.load(Ordering::Relaxed) + 400 {
            return;
        }
        self.last_ms.store(now, Ordering::Relaxed);
        *self.img.lock().unwrap_or_else(|e| e.into_inner()) = Some(nv12_to_rgb_small(f, PREVIEW_W));
    }
}

/// Nearest-neighbour downscale + BT.601 limited-range YUV -> RGB. About 60k pixels: cheap.
pub fn nv12_to_rgb_small(f: &Nv12Frame, out_w: usize) -> PreviewImage {
    let (sw, sh) = (f.width as usize, f.height as usize);
    let out_w = out_w.min(sw).max(2) & !1;
    let out_h = ((out_w * sh / sw.max(1)).max(2)) & !1;
    let mut rgb = vec![0u8; out_w * out_h * 3];
    let (y_plane, uv) = f.data.split_at(sw * sh);
    for oy in 0..out_h {
        let sy = oy * sh / out_h;
        for ox in 0..out_w {
            let sx = ox * sw / out_w;
            let y = f32::from(y_plane[sy * sw + sx]) - 16.0;
            let ui = (sy / 2) * sw + (sx / 2) * 2;
            let (u, v) = (f32::from(uv[ui]) - 128.0, f32::from(uv[ui + 1]) - 128.0);
            let o = (oy * out_w + ox) * 3;
            rgb[o] = (1.164 * y + 1.596 * v).clamp(0.0, 255.0) as u8;
            rgb[o + 1] = (1.164 * y - 0.392 * u - 0.813 * v).clamp(0.0, 255.0) as u8;
            rgb[o + 2] = (1.164 * y + 2.017 * u).clamp(0.0, 255.0) as u8;
        }
    }
    PreviewImage { w: out_w, h: out_h, rgb }
}

/// Counters shared with the status reporter.
#[derive(Default)]
pub struct Stats {
    pub packets_in: AtomicU64,
    pub bytes_in: AtomicU64,
    pub frames_decoded: AtomicU64,
    pub frames_out: AtomicU64,
    pub dropped: AtomicU64,
    pub decode_errors: AtomicU64,
    /// Last decode call, microseconds.
    pub decode_us: AtomicU64,
    /// Arrival -> written to shared memory, microseconds, of the last frame.
    pub age_us: AtomicU64,
    /// Windowed measurements for the periodic `camera stats` log line.
    pub window: StatCounters,
}

/// Everything the decode thread reads from the manager.
#[derive(Clone)]
pub struct PipelineCtx {
    pub shm: ShmSlot,
    pub target: Arc<Target>,
    /// False while paused: decode (to keep the stream healthy) but do not publish frames.
    pub write_enabled: Arc<AtomicBool>,
    pub preview: Arc<Preview>,
    pub factory: DecoderFactory,
}

pub struct Pipeline {
    pub session_id: String,
    pub stats: Arc<Stats>,
    /// The decoder needs a key frame (dropped packets, decode error, fresh attach).
    pub need_keyframe: Arc<AtomicBool>,
    /// The media connection ended.
    pub ended: Arc<AtomicBool>,
    pub decoder_error: Arc<Mutex<Option<String>>>,
    queue: Arc<PacketQueue>,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    reporter: JoinHandle<()>,
}

impl Pipeline {
    /// Must be called inside a tokio runtime.
    pub fn spawn(mut ms: MediaStream, ctx: PipelineCtx) -> Self {
        let session_id = ms.session_id.clone();
        let stats = Arc::new(Stats::default());
        let need_keyframe = Arc::new(AtomicBool::new(true));
        let ended = Arc::new(AtomicBool::new(false));
        let decoder_error = Arc::new(Mutex::new(None));
        let queue = Arc::new(PacketQueue::new(QUEUE_CAP));
        let stop = Arc::new(AtomicBool::new(false));

        let (q, st, nk, en) = (queue.clone(), stats.clone(), need_keyframe.clone(), ended.clone());
        let reader = tokio::spawn(async move {
            loop {
                match ms.next_packet().await {
                    Ok(Some(p)) => {
                        st.packets_in.fetch_add(1, Ordering::Relaxed);
                        st.bytes_in.fetch_add(p.data.len() as u64, Ordering::Relaxed);
                        st.window.packets.fetch_add(1, Ordering::Relaxed);
                        st.window.bytes.fetch_add(p.data.len() as u64, Ordering::Relaxed);
                        let dropped = q.push(Instant::now(), p);
                        if dropped > 0 {
                            st.dropped.fetch_add(dropped as u64, Ordering::Relaxed);
                            st.window.drops.fetch_add(dropped as u64, Ordering::Relaxed);
                            nk.store(true, Ordering::Relaxed);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        debug!("media read ended: {e}");
                        break;
                    }
                }
            }
            q.close();
            en.store(true, Ordering::Relaxed);
        });

        let st = stats.clone();
        let reporter = tokio::spawn(async move {
            let mut rep = Reporter::default();
            let mut tick = tokio::time::interval(REPORT_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            let mut last = Instant::now();
            loop {
                tick.tick().await;
                let now = Instant::now();
                rep.report(st.window.snapshot(), now - last).log();
                last = now;
            }
        });

        let (q, st, nk, stop2, derr) =
            (queue.clone(), stats.clone(), need_keyframe.clone(), stop.clone(), decoder_error.clone());
        let en = ended.clone();
        let spawned = std::thread::Builder::new().name("nd-decode".into()).spawn(move || {
            decode_loop(&q, &st, &nk, &stop2, &derr, &ctx);
            en.store(true, Ordering::Relaxed);
        });
        if let Err(e) = spawned {
            *decoder_error.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(format!("cannot start the decode thread: {e}"));
        }
        Pipeline { session_id, stats, need_keyframe, ended, decoder_error, queue, stop, reader, reporter }
    }

    pub fn decoder_error(&self) -> Option<String> {
        self.decoder_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Stop everything; the decode thread exits on its own shortly after.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.reader.abort();
        self.reporter.abort();
        self.queue.close();
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn decode_loop(
    queue: &PacketQueue,
    stats: &Stats,
    need_keyframe: &AtomicBool,
    stop: &AtomicBool,
    decoder_error: &Mutex<Option<String>>,
    ctx: &PipelineCtx,
) {
    let mut dec = match (ctx.factory)() {
        Ok(d) => {
            info!(decoder = d.name(), "H.264 decoder ready");
            d
        }
        Err(e) => {
            warn!("cannot create the H.264 decoder: {e}");
            *decoder_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            return;
        }
    };
    let mut last_err_log = Instant::now() - Duration::from_secs(10);
    while !stop.load(Ordering::Relaxed) {
        let (arrival, packet) = match queue.pop(Duration::from_millis(100)) {
            Pop::Packet(t, p) => (t, p),
            Pop::Timeout => continue,
            Pop::Closed => break,
        };
        let t0 = Instant::now();
        let result = dec.decode(&packet);
        let took = t0.elapsed();
        stats.decode_us.store(took.as_micros() as u64, Ordering::Relaxed);
        match result {
            Ok(frames) => {
                if !packet.header.config {
                    stats.window.record_decode(took, frames.len() as u64);
                }
                for f in frames {
                    stats.frames_decoded.fetch_add(1, Ordering::Relaxed);
                    publish(&f, ctx, &stats.window);
                    stats.frames_out.fetch_add(1, Ordering::Relaxed);
                    stats.age_us.store(arrival.elapsed().as_micros() as u64, Ordering::Relaxed);
                }
            }
            Err(MediaError::Decode(e)) => {
                stats.decode_errors.fetch_add(1, Ordering::Relaxed);
                need_keyframe.store(true, Ordering::Relaxed);
                if last_err_log.elapsed() > Duration::from_secs(5) {
                    last_err_log = Instant::now();
                    warn!("H.264 decode error: {e}");
                }
            }
            Err(MediaError::Unavailable(e)) => {
                *decoder_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                break;
            }
        }
    }
    queue.close();
}

/// Scale to the consumer's size and write to shared memory (when allowed and mapped).
fn publish(f: &Nv12Frame, ctx: &PipelineCtx, win: &StatCounters) {
    let (tw, th) = ctx.target.get();
    let scaled;
    let out: &Nv12Frame = if tw >= 2 && th >= 2 && (f.width != tw || f.height != th) {
        let t0 = Instant::now();
        scaled = scale_nv12(f, tw, th);
        win.record_scale(t0.elapsed());
        &scaled
    } else {
        f
    };
    ctx.preview.offer(out);
    if !ctx.write_enabled.load(Ordering::Relaxed) || tw < 2 || th < 2 {
        return;
    }
    if let Some(shm) = ctx.shm.get() {
        if shm.shm().write_frame(out.width, out.height, f.pts_us, &out.data) {
            win.record_published(out.width, out.height);
        }
    }
}

/// Encode a preview as PNG (the `png` crate is already a dependency).
pub fn preview_png(img: &PreviewImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.w as u32, img.h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().ok()?;
    w.write_image_data(&img.rgb).ok()?;
    w.finish().ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_conversion_and_png() {
        // White (Y=235) and black (Y=16) halves, neutral chroma.
        let (w, h) = (64usize, 36usize);
        let mut data = vec![0u8; w * h * 3 / 2];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = if x < w / 2 { 235 } else { 16 };
            }
        }
        data[w * h..].fill(128);
        let f = Nv12Frame { width: w as u32, height: h as u32, pts_us: 0, data };
        let img = nv12_to_rgb_small(&f, 32);
        assert_eq!((img.w, img.h), (32, 18));
        assert!(img.rgb[0] > 240 && img.rgb[(31) * 3] < 5);
        let png = preview_png(&img).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn target_rounds_to_even() {
        let t = Target::default();
        t.set(641, 361);
        assert_eq!(t.get(), (640, 360));
    }
}
