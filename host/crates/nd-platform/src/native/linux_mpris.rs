// SPDX-License-Identifier: GPL-3.0-or-later
//! Now-playing info from MPRIS2 players on the session D-Bus.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::{MediaCommand, MediaInfo, NowPlaying, PlatformError, RepeatMode, Result};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

fn err(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Other(format!("mpris: {e}"))
}

/// MPRIS media source with a cached session-bus connection.
pub struct MprisMedia {
    conn: Connection,
    /// Art of the current track: (key, art).
    art: Mutex<ArtCache>,
}

type ArtCache = Option<(ArtKey, Option<Arc<[u8]>>)>;
type ArtKey = (String, String, String, String, String);

const MAX_ART_FILE: u64 = 5 * 1024 * 1024;
const MAX_ART_PX: u32 = 256;

/// Path of a `file://` art URL (percent-decoded), or `None` for other schemes.
fn art_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// PNG dimensions from the IHDR chunk, `None` when the bytes are not a PNG.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some((u32::from_be_bytes(bytes[16..20].try_into().ok()?), u32::from_be_bytes(bytes[20..24].try_into().ok()?)))
}

const MAX_ART_MEGAPIXELS: u64 = 40;

/// Decoded raster: `ch` is 3 (RGB) or 4 (RGBA), 8 bits per channel.
struct Raster {
    w: u32,
    h: u32,
    ch: usize,
    px: Vec<u8>,
}

fn too_big(w: u32, h: u32) -> bool {
    w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_ART_MEGAPIXELS * 1_000_000
}

fn decode_png(bytes: &[u8]) -> Option<Raster> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if too_big(w, h) {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    let (ch, px) = match frame.color_type {
        png::ColorType::Rgba => (4, buf),
        png::ColorType::Rgb => (3, buf),
        png::ColorType::Grayscale => (3, buf.iter().flat_map(|g| [*g, *g, *g]).collect()),
        png::ColorType::GrayscaleAlpha => {
            (4, buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect())
        }
        png::ColorType::Indexed => return None,
    };
    (px.len() == w as usize * h as usize * ch).then_some(Raster { w, h, ch, px })
}

fn decode_jpeg(bytes: &[u8]) -> Option<Raster> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    dec.decode_headers().ok()?;
    let info = dec.info()?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    if too_big(w, h) {
        return None;
    }
    let px = dec.decode().ok()?;
    (px.len() == w as usize * h as usize * 3).then_some(Raster { w, h, ch: 3, px })
}

/// Area-average (box) downscale so the image fits `max` x `max`; never upscales.
fn downscale(img: Raster, max: u32) -> Raster {
    if img.w <= max && img.h <= max {
        return img;
    }
    let scale = (f64::from(max) / f64::from(img.w)).min(f64::from(max) / f64::from(img.h));
    let tw = ((f64::from(img.w) * scale).round() as u32).clamp(1, max);
    let th = ((f64::from(img.h) * scale).round() as u32).clamp(1, max);
    let (w, h, ch) = (img.w as usize, img.h as usize, img.ch);
    let mut out = vec![0u8; tw as usize * th as usize * ch];
    for ty in 0..th as usize {
        let y0 = ty * h / th as usize;
        let y1 = ((ty + 1) * h / th as usize).max(y0 + 1).min(h);
        for tx in 0..tw as usize {
            let x0 = tx * w / tw as usize;
            let x1 = ((tx + 1) * w / tw as usize).max(x0 + 1).min(w);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * ch;
                    for (acc, v) in sum.iter_mut().zip(&img.px[i..i + ch]) {
                        *acc += u32::from(*v);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (ty * tw as usize + tx) * ch;
            for c in 0..ch {
                out[o + c] = ((sum[c] + n / 2) / n) as u8;
            }
        }
    }
    Raster { w: tw, h: th, ch, px: out }
}

fn encode_png(img: &Raster) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.w, img.h);
    enc.set_color(if img.ch == 4 { png::ColorType::Rgba } else { png::ColorType::Rgb });
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().ok()?;
    wr.write_image_data(&img.px).ok()?;
    wr.finish().ok()?;
    Some(out)
}

/// Art bytes for a `file://` URL. PNG files up to 5 MB and 256x256 pass through as-is; other PNGs and JPEGs
/// (up to 40 megapixels) are decoded, scaled to fit 256x256 and re-encoded as PNG. Anything else yields `None`.
fn load_art(url: &str) -> Option<Arc<[u8]>> {
    let path = art_path(url)?;
    let f = std::fs::File::open(path).ok()?;
    if f.metadata().ok()?.len() > MAX_ART_FILE {
        return None;
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(f, MAX_ART_FILE + 1), &mut bytes).ok()?;
    if bytes.len() as u64 > MAX_ART_FILE {
        return None;
    }
    if let Some((w, h)) = png_size(&bytes) {
        if w > 0 && h > 0 && w <= MAX_ART_PX && h <= MAX_ART_PX {
            return Some(Arc::from(bytes));
        }
        if too_big(w, h) {
            return None;
        }
        let img = downscale(decode_png(&bytes)?, MAX_ART_PX);
        return encode_png(&img).map(Arc::from);
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let img = downscale(decode_jpeg(&bytes)?, MAX_ART_PX);
        return encode_png(&img).map(Arc::from);
    }
    None
}

impl MprisMedia {
    pub fn open() -> Result<Self> {
        Ok(Self { conn: Connection::session().map_err(err)?, art: Mutex::new(None) })
    }

    fn player_proxy(&self, name: &str) -> zbus::Result<Proxy<'static>> {
        Proxy::new(&self.conn, name.to_owned(), PATH, PLAYER_IFACE)
    }

    fn status(&self, name: &str) -> Option<String> {
        self.player_proxy(name).ok()?.get_property::<String>("PlaybackStatus").ok()
    }

    /// The player `now_playing` reports: the first Playing one, else the first Paused one.
    fn pick_player(&self) -> Result<Option<(String, bool)>> {
        let names = DBusProxy::new(&self.conn).map_err(err)?.list_names().map_err(err)?;
        let mut paused: Option<String> = None;
        for n in names.iter().map(|n| n.as_str()).filter(|n| n.starts_with(PREFIX)) {
            match self.status(n).as_deref() {
                Some("Playing") => return Ok(Some((n.to_owned(), true))),
                Some("Paused") if paused.is_none() => paused = Some(n.to_owned()),
                _ => {}
            }
        }
        Ok(paused.map(|p| (p, false)))
    }
}

/// MPRIS `LoopStatus` to [`RepeatMode`].
fn repeat_from_loop_status(s: &str) -> Option<RepeatMode> {
    match s {
        "None" => Some(RepeatMode::Off),
        "Track" => Some(RepeatMode::Track),
        "Playlist" => Some(RepeatMode::List),
        _ => None,
    }
}

fn loop_status_for(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "None",
        RepeatMode::Track => "Track",
        RepeatMode::List => "Playlist",
    }
}

impl MediaInfo for MprisMedia {
    fn backend_name(&self) -> &'static str {
        "mpris"
    }
    fn now_playing(&self) -> Result<Option<NowPlaying>> {
        let Some((name, playing)) = self.pick_player()? else {
            return Ok(None);
        };
        let proxy = self.player_proxy(&name).map_err(err)?;
        let meta: HashMap<String, OwnedValue> = proxy.get_property("Metadata").map_err(err)?;
        let position: Option<i64> = proxy.get_property::<i64>("Position").ok();
        let app = name.strip_prefix(PREFIX).unwrap_or(&name).to_owned();
        let mut np = build_now_playing(&meta, playing, position, &app);
        np.shuffle = proxy.get_property::<bool>("Shuffle").ok();
        np.repeat = proxy.get_property::<String>("LoopStatus").ok().and_then(|s| repeat_from_loop_status(&s));
        np.can_seek = proxy.get_property::<bool>("CanSeek").unwrap_or(false);
        let url = meta_string(&meta, "mpris:artUrl");
        let key = (app, np.title.clone(), np.artist.clone(), np.album.clone(), url.clone());
        let mut cache = self.art.lock().unwrap_or_else(|e| e.into_inner());
        np.art = match cache.as_ref() {
            Some((k, a)) if *k == key => a.clone(),
            _ => {
                let a = load_art(&url);
                *cache = Some((key, a.clone()));
                a
            }
        };
        Ok(Some(np))
    }

    fn control(&self, cmd: MediaCommand) -> Result<()> {
        let Some((name, _)) = self.pick_player()? else {
            return Err(PlatformError::Other("no media player".into()));
        };
        let proxy = self.player_proxy(&name).map_err(err)?;
        match cmd {
            MediaCommand::PlayPause => proxy.call_method("PlayPause", &()).map(drop),
            MediaCommand::Play => proxy.call_method("Play", &()).map(drop),
            MediaCommand::Pause => proxy.call_method("Pause", &()).map(drop),
            MediaCommand::Next => proxy.call_method("Next", &()).map(drop),
            MediaCommand::Prev => proxy.call_method("Previous", &()).map(drop),
            MediaCommand::Stop => proxy.call_method("Stop", &()).map(drop),
            MediaCommand::Seek { position_s } => {
                if !position_s.is_finite() {
                    return Err(PlatformError::Other("invalid seek position".into()));
                }
                let meta: HashMap<String, OwnedValue> = proxy.get_property("Metadata").unwrap_or_default();
                let length = meta_length(&meta).filter(|l| *l > 0);
                let mut target = (position_s.max(0.0) * 1e6) as i64;
                if let Some(l) = length {
                    target = target.min(l);
                }
                match meta_trackid(&meta) {
                    Some(id) => proxy.call_method("SetPosition", &(id, target)).map(drop),
                    None => {
                        let cur = proxy.get_property::<i64>("Position").unwrap_or(0);
                        proxy.call_method("Seek", &(target - cur)).map(drop)
                    }
                }
            }
            MediaCommand::SeekBy { delta_s } => {
                if !delta_s.is_finite() {
                    return Err(PlatformError::Other("invalid seek offset".into()));
                }
                proxy.call_method("Seek", &((delta_s * 1e6) as i64)).map(drop)
            }
            MediaCommand::Shuffle { on } => proxy.set_property("Shuffle", on).map_err(zbus::Error::from),
            MediaCommand::Repeat { mode } => {
                proxy.set_property("LoopStatus", loop_status_for(mode)).map_err(zbus::Error::from)
            }
        }
        .map_err(err)
    }
}

fn meta_trackid(meta: &HashMap<String, OwnedValue>) -> Option<ObjectPath<'static>> {
    match meta.get("mpris:trackid").map(|v| &**v)? {
        Value::ObjectPath(p) => Some(p.to_owned()),
        Value::Str(s) => ObjectPath::try_from(s.as_str()).ok().map(|p| p.to_owned()),
        _ => None,
    }
}

fn meta_string(meta: &HashMap<String, OwnedValue>, key: &str) -> String {
    match meta.get(key).map(|v| &**v) {
        Some(Value::Str(s)) => s.to_string(),
        _ => String::new(),
    }
}

fn meta_artist(meta: &HashMap<String, OwnedValue>) -> String {
    match meta.get("xesam:artist").map(|v| &**v) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Str(s)) => s.to_string(),
        _ => String::new(),
    }
}

/// `mpris:length` is an int64 (spec) but some players send uint64 or int32.
fn meta_length(meta: &HashMap<String, OwnedValue>) -> Option<i64> {
    match meta.get("mpris:length").map(|v| &**v)? {
        Value::I64(n) => Some(*n),
        Value::U64(n) => i64::try_from(*n).ok(),
        Value::I32(n) => Some(i64::from(*n)),
        Value::U32(n) => Some(i64::from(*n)),
        _ => None,
    }
}

/// Pure assembly of [`NowPlaying`] from MPRIS metadata and position (both µs). `art` is left unset.
pub(crate) fn build_now_playing(
    meta: &HashMap<String, OwnedValue>,
    playing: bool,
    position_us: Option<i64>,
    app: &str,
) -> NowPlaying {
    let length = meta_length(meta).filter(|l| *l > 0);
    let progress = match (position_us, length) {
        (Some(p), Some(l)) => Some((p as f64 / l as f64).clamp(0.0, 1.0) as f32),
        _ => None,
    };
    let length_s = length.map(|l| l as f64 / 1e6);
    let position_s = position_us.map(|p| {
        let p = (p.max(0)) as f64 / 1e6;
        length_s.map_or(p, |l| p.min(l))
    });
    NowPlaying {
        title: meta_string(meta, "xesam:title"),
        artist: meta_artist(meta),
        album: meta_string(meta, "xesam:album"),
        playing,
        progress,
        position_s,
        length_s,
        app: app.to_owned(),
        art: None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ov<'a, T: Into<Value<'a>>>(v: T) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    #[test]
    fn full_metadata() {
        let mut m = HashMap::new();
        m.insert("xesam:title".to_string(), ov("Song"));
        m.insert("xesam:album".to_string(), ov("Album"));
        m.insert("xesam:artist".to_string(), ov(vec!["A", "B"]));
        m.insert("mpris:length".to_string(), ov(200_000_000i64));
        let np = build_now_playing(&m, true, Some(50_000_000), "vlc");
        assert_eq!(np.title, "Song");
        assert_eq!(np.artist, "A, B");
        assert_eq!(np.album, "Album");
        assert!(np.playing);
        assert!((np.progress.unwrap() - 0.25).abs() < 1e-6);
        assert_eq!(np.position_s, Some(50.0));
        assert_eq!(np.length_s, Some(200.0));
        assert_eq!(np.app, "vlc");
    }

    #[test]
    fn missing_keys() {
        let np = build_now_playing(&HashMap::new(), false, Some(5), "");
        assert_eq!(np.title, "");
        assert_eq!(np.length_s, None);
        assert_eq!(np.progress, None);
    }

    #[test]
    fn uint_length_and_overrun() {
        let mut m = HashMap::new();
        m.insert("mpris:length".to_string(), ov(100u64));
        assert_eq!(build_now_playing(&m, true, Some(500), "").progress, Some(1.0));
        assert_eq!(build_now_playing(&m, true, None, "").progress, None);
    }

    #[test]
    fn art_url_and_png_size() {
        assert_eq!(art_path("file:///tmp/a%20b.png").as_deref(), Some("/tmp/a b.png"));
        assert_eq!(art_path("https://x/y.png"), None);
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&200u32.to_be_bytes());
        png.extend_from_slice(&100u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((200, 100)));
        assert_eq!(png_size(b"\xff\xd8\xff not png at all, padding padding"), None);
    }

    #[test]
    fn loop_status_mapping() {
        assert_eq!(repeat_from_loop_status("None"), Some(RepeatMode::Off));
        assert_eq!(repeat_from_loop_status("Track"), Some(RepeatMode::Track));
        assert_eq!(repeat_from_loop_status("Playlist"), Some(RepeatMode::List));
        assert_eq!(repeat_from_loop_status("bogus"), None);
        for m in [RepeatMode::Off, RepeatMode::Track, RepeatMode::List] {
            assert_eq!(repeat_from_loop_status(loop_status_for(m)), Some(m));
        }
    }

    fn png_dims(bytes: &[u8]) -> (u32, u32) {
        png_size(bytes).expect("output is a png")
    }

    #[test]
    fn load_art_decodes_and_scales() {
        let dir = std::env::temp_dir().join(format!("nd-art-dec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 600x400 RGBA PNG.
        let big = dir.join("big.png");
        let mut raw = Vec::new();
        for y in 0..400u32 {
            for x in 0..600u32 {
                raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
            }
        }
        let mut enc = png::Encoder::new(std::fs::File::create(&big).unwrap(), 600, 400);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&raw).unwrap();
        let out = load_art(&format!("file://{}", big.display())).expect("png scaled");
        let (w, h) = png_dims(&out);
        assert_eq!((w, h), (256, 171));
        // 500x300 JPEG.
        let jpg = dir.join("a.jpg");
        let rgb: Vec<u8> = (0..500 * 300).flat_map(|i| [(i % 251) as u8, 90, 200]).collect();
        jpeg_encoder::Encoder::new_file(&jpg, 90)
            .unwrap()
            .encode(&rgb, 500, 300, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let out = load_art(&format!("file://{}", jpg.display())).expect("jpeg scaled");
        assert_eq!(png_dims(&out), (256, 154));
        // Small JPEG is converted but not enlarged.
        let small = dir.join("s.jpg");
        let rgb: Vec<u8> = (0..64 * 32).flat_map(|_| [10u8, 20, 30]).collect();
        jpeg_encoder::Encoder::new_file(&small, 90)
            .unwrap()
            .encode(&rgb, 64, 32, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let out = load_art(&format!("file://{}", small.display())).expect("small jpeg");
        assert_eq!(png_dims(&out), (64, 32));
        // Garbage is rejected.
        let junk = dir.join("junk.bin");
        std::fs::write(&junk, b"not an image").unwrap();
        assert!(load_art(&format!("file://{}", junk.display())).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_art_limits() {
        let dir = std::env::temp_dir().join(format!("nd-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |w: u32, h: u32| {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
            v.extend_from_slice(b"IHDR");
            v.extend_from_slice(&w.to_be_bytes());
            v.extend_from_slice(&h.to_be_bytes());
            v
        };
        let small = dir.join("s.png");
        let big = dir.join("b.png");
        std::fs::write(&small, mk(256, 200)).unwrap();
        std::fs::write(&big, mk(257, 10)).unwrap();
        assert!(load_art(&format!("file://{}", small.display())).is_some());
        assert!(load_art(&format!("file://{}", big.display())).is_none());
        assert!(load_art("file:///nonexistent/x.png").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
