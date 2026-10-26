// SPDX-License-Identifier: GPL-3.0-or-later
//! Website icons for "Open URL" keys, fetched from the site itself (no third-party favicon service).
//!
//! Steps: fetch the page, scan its `<link>` tags (and the web app manifest when the page has no big icon),
//! rank the candidates, and take the first one that downloads and sniffs as an image. `/favicon.ico` at the
//! origin is the last resort. ICO files are unpacked (largest entry; BMP entries become PNG).

use base64::Engine as _;
use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

const HTML_CAP: usize = 512 * 1024;
const IMAGE_CAP: usize = 2 * 1024 * 1024;
const MANIFEST_CAP: usize = 512 * 1024;
const MAX_REDIRECTS: u32 = 5;
const TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Candidates tried per site before giving up.
const MAX_ATTEMPTS: usize = 8;
const POSITIVE_TTL: Duration = Duration::from_secs(3600);
const NEGATIVE_TTL: Duration = Duration::from_secs(300);
const CACHE_CAP: usize = 32;
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct SiteIcon {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

type CacheMap = HashMap<String, (Instant, Option<SiteIcon>)>;

fn cache() -> &'static Mutex<CacheMap> {
    static CACHE: std::sync::OnceLock<Mutex<CacheMap>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Parse and validate a user URL: http(s) with a host.
pub fn check_url(raw: &str) -> Result<Url, String> {
    let u = Url::parse(raw.trim()).map_err(|_| "not a valid URL".to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("only http and https URLs are supported".into());
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err("the URL has no host".into());
    }
    Ok(u)
}

/// Icon for the site at `raw`. `Err` = bad URL, `Ok(None)` = the site has no usable icon.
/// Blocking (network); results are cached per origin.
pub fn fetch(raw: &str) -> Result<Option<SiteIcon>, String> {
    let url = check_url(raw)?;
    let key = url.origin().ascii_serialization();
    {
        let c = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, hit)) = c.get(&key) {
            let ttl = if hit.is_some() { POSITIVE_TTL } else { NEGATIVE_TTL };
            if at.elapsed() < ttl {
                return Ok(hit.clone());
            }
        }
    }
    let found = discover(&url);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= CACHE_CAP {
        c.retain(|_, (at, _)| at.elapsed() < NEGATIVE_TTL);
        if c.len() >= CACHE_CAP {
            c.clear();
        }
    }
    c.insert(key, (Instant::now(), found.clone()));
    Ok(found)
}

// ---- network ----

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout_read(TIMEOUT)
        .timeout_write(TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .redirects(MAX_REDIRECTS)
        .user_agent(USER_AGENT)
        .build()
}

/// Read at most `cap` bytes; the flag says the source had more.
fn read_capped(r: impl Read, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    r.take(cap as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    Ok((buf, truncated))
}

/// GET `url`; returns (final URL after redirects, body capped at `cap`, truncated).
/// `gzip` only for the manifest: some CDNs serve it compressed whatever we ask, while Cloudflare-fronted
/// pages (kick.com) answer 403 to a browser User-Agent that offers nothing but gzip.
fn http_get(agent: &ureq::Agent, url: &str, cap: usize, gzip: bool) -> Result<(String, Vec<u8>, bool), String> {
    let mut req = agent
        .get(url)
        .set("Accept", "text/html,application/xhtml+xml,application/json,image/*;q=0.8,*/*;q=0.5")
        .set("Accept-Language", "en-US,en;q=0.9");
    if !gzip {
        req = req.set("Accept-Encoding", "identity");
    }
    let resp = req.call().map_err(|e| e.to_string())?;
    let final_url = resp.get_url().to_string();
    let (body, truncated) = read_capped(resp.into_reader(), cap).map_err(|e| e.to_string())?;
    Ok((final_url, body, truncated))
}

fn discover(url: &Url) -> Option<SiteIcon> {
    let agent = agent();
    let mut cands: Vec<Candidate> = Vec::new();
    let mut manifest: Option<Url> = None;
    // The page may fail (403, bot walls); /favicon.ico below still gets its try.
    if let Ok((final_url, body, _)) = http_get(&agent, url.as_str(), HTML_CAP, false) {
        if let Ok(doc) = Url::parse(&final_url) {
            let html = String::from_utf8_lossy(&body);
            let parsed = parse_html(&html, &doc);
            cands = parsed.icons;
            manifest = parsed.manifest;
        }
    }
    // The manifest is one more request: only worth it when the page itself has no big icon.
    if !cands.iter().any(|c| c.tier().0 == 0) {
        if let Some(m) = manifest {
            if let Ok((final_url, body, _)) = http_get(&agent, m.as_str(), MANIFEST_CAP, true) {
                let base = Url::parse(&final_url).unwrap_or(m);
                cands.extend(parse_manifest(&body, &base));
            }
        }
    }
    let ranked = rank(cands, url);
    for c in ranked.iter().take(MAX_ATTEMPTS) {
        if let Some(icon) = load_candidate(&agent, c) {
            return Some(icon);
        }
    }
    None
}

fn load_candidate(agent: &ureq::Agent, c: &Candidate) -> Option<SiteIcon> {
    let bytes = if c.url.starts_with("data:") {
        decode_data_url(&c.url)?
    } else {
        let (_, body, truncated) = http_get(agent, &c.url, IMAGE_CAP, false).ok()?;
        if truncated {
            return None;
        }
        body
    };
    normalize_image(bytes)
}

fn decode_data_url(u: &str) -> Option<Vec<u8>> {
    let rest = u.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let bytes = if meta.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?
    } else {
        percent_decode(data)
    };
    (bytes.len() <= IMAGE_CAP).then_some(bytes)
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

// ---- image sniffing and ICO ----

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Kind {
    Png,
    Jpeg,
    Webp,
    Svg,
    Ico,
}

fn sniff(b: &[u8]) -> Option<Kind> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else if b.starts_with(&[0, 0, 1, 0]) {
        Some(Kind::Ico)
    } else {
        // SVG: text with an <svg root near the top (after BOM, XML prolog, comments, doctype).
        let head = &b[..b.len().min(4096)];
        let text = String::from_utf8_lossy(head).to_ascii_lowercase();
        (text.trim_start_matches('\u{feff}').trim_start().starts_with('<') && text.contains("<svg"))
            .then_some(Kind::Svg)
    }
}

/// Sniff the bytes and turn them into something the UI can import (ICO -> PNG).
fn normalize_image(bytes: Vec<u8>) -> Option<SiteIcon> {
    let content_type = match sniff(&bytes)? {
        Kind::Png => "image/png",
        Kind::Jpeg => "image/jpeg",
        Kind::Webp => "image/webp",
        Kind::Svg => "image/svg+xml",
        Kind::Ico => {
            return ico_to_png(&bytes).map(|bytes| SiteIcon { content_type: "image/png", bytes });
        }
    };
    Some(SiteIcon { content_type, bytes })
}

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Largest directory entry of an ICO; PNG entries are returned as is, BMP entries are converted.
fn ico_to_png(b: &[u8]) -> Option<Vec<u8>> {
    if u16le(b, 0)? != 0 || u16le(b, 2)? != 1 {
        return None;
    }
    let count = u16le(b, 4)? as usize;
    let mut best: Option<(u32, u16, usize, usize)> = None; // (pixels, bpp, offset, size)
    for i in 0..count.min(64) {
        let e = 6 + i * 16;
        let dim = |v: u8| if v == 0 { 256u32 } else { v as u32 };
        let (w, h) = (dim(*b.get(e)?), dim(*b.get(e + 1)?));
        let bpp = u16le(b, e + 6)?;
        let size = u32le(b, e + 8)? as usize;
        let off = u32le(b, e + 12)? as usize;
        if size == 0 || off.checked_add(size)? > b.len() {
            continue;
        }
        let key = (w * h, bpp);
        if best.is_none_or(|(p, bp, ..)| key > (p, bp)) {
            best = Some((w * h, bpp, off, size));
        }
    }
    let (_, _, off, size) = best?;
    let data = &b[off..off + size];
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(data.to_vec())
    } else {
        dib_to_png(data)
    }
}

/// BMP "DIB" as stored in ICO: BITMAPINFOHEADER, optional palette, XOR bitmap, 1-bit AND mask. Height is doubled.
fn dib_to_png(d: &[u8]) -> Option<Vec<u8>> {
    let hs = u32le(d, 0)? as usize;
    let w = u32le(d, 4)? as usize;
    let h = (u32le(d, 8)? as i32).unsigned_abs() as usize / 2;
    let bpp = u16le(d, 14)? as usize;
    let comp = u32le(d, 16)?;
    let clr_used = u32le(d, 32).unwrap_or(0) as usize;
    if hs < 40
        || comp != 0
        || !(1..=1024).contains(&w)
        || !(1..=1024).contains(&h)
        || !matches!(bpp, 1 | 4 | 8 | 24 | 32)
    {
        return None;
    }
    let ncolors = if bpp <= 8 {
        if clr_used > 0 {
            clr_used.min(1 << bpp)
        } else {
            1 << bpp
        }
    } else {
        0
    };
    let palette = d.get(hs..hs + ncolors * 4)?;
    let pix = hs + ncolors * 4;
    let stride = (w * bpp).div_ceil(32) * 4;
    let mstride = w.div_ceil(32) * 4;
    let xor = d.get(pix..pix + stride * h)?;
    let mask = d.get(pix + stride * h..pix + stride * h + mstride * h);
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = h - 1 - y; // bottom-up
        let row = &xor[src * stride..(src + 1) * stride];
        for x in 0..w {
            let (r, g, bl, a) = match bpp {
                32 => (row[x * 4 + 2], row[x * 4 + 1], row[x * 4], row[x * 4 + 3]),
                24 => (row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255),
                _ => {
                    let per = 8 / bpp;
                    let shift = 8 - bpp * (x % per + 1);
                    let idx = ((row[x / per] >> shift) as usize) & ((1 << bpp) - 1);
                    let p = palette.get(idx * 4..idx * 4 + 3).unwrap_or(&[0, 0, 0]);
                    (p[2], p[1], p[0], 255)
                }
            };
            let o = (y * w + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[r, g, bl, a]);
        }
    }
    if let Some(m) = mask {
        // 32-bit icons carry their own alpha; the mask only matters when that alpha is all zero.
        let has_alpha = bpp == 32 && rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
        if !has_alpha {
            for y in 0..h {
                let src = h - 1 - y;
                for x in 0..w {
                    let transparent = m[src * mstride + x / 8] & (0x80 >> (x % 8)) != 0;
                    rgba[(y * w + x) * 4 + 3] = if transparent { 0 } else { 255 };
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().ok()?;
    wr.write_image_data(&rgba).ok()?;
    wr.finish().ok()?;
    Some(out)
}

// ---- HTML / manifest parsing ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    /// Largest declared edge in pixels, if any.
    pub size: Option<u32>,
    pub svg: bool,
    pub apple: bool,
}

impl Candidate {
    /// (tier, key): lower sorts first. 0 = raster >= 96 px (closest to 256), 1 = SVG, 2 = smaller rasters.
    fn tier(&self) -> (u8, u32) {
        if self.svg {
            return (1, 0);
        }
        match self.size.or(self.apple.then_some(180)) {
            Some(s) if s >= 96 => (0, s.abs_diff(256)),
            Some(s) => (2, u32::MAX - s),
            None => (2, u32::MAX - 32),
        }
    }
}

#[derive(Debug, Default)]
pub struct ParsedHtml {
    pub icons: Vec<Candidate>,
    pub manifest: Option<Url>,
}

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, n: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Tolerant scanner: returns the `<link>` and `<base>` tags of `html`. Skips comments, scripts and styles.
fn scan_tags(html: &str) -> Vec<Tag> {
    let b = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let find = |from: usize, pat: &str| lower[from.min(lb.len())..].find(pat).map(|p| p + from);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if lb[i..].starts_with(b"<!--") {
            i = find(i + 4, "-->").map_or(b.len(), |p| p + 3);
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        let name = &lower[i + 1..j];
        if name.is_empty() {
            i += 1;
            continue;
        }
        if name == "script" || name == "style" {
            i = find(j, &format!("</{name}")).unwrap_or(b.len());
            if i < b.len() {
                i += 2;
            }
            continue;
        }
        if name != "link" && name != "base" {
            i = j;
            continue;
        }
        let mut attrs = Vec::new();
        loop {
            while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b'/') {
                j += 1;
            }
            if j >= b.len() {
                break;
            }
            if b[j] == b'>' {
                j += 1;
                break;
            }
            let ns = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/') {
                j += 1;
            }
            if j == ns {
                j += 1; // stray '=' or similar: make progress
                continue;
            }
            let aname = lower[ns..j].to_string();
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut val = String::new();
            if j < b.len() && b[j] == b'=' {
                j += 1;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                    let q = b[j];
                    j += 1;
                    let vs = j;
                    while j < b.len() && b[j] != q {
                        j += 1;
                    }
                    val = String::from_utf8_lossy(&b[vs..j]).into_owned();
                    j = (j + 1).min(b.len());
                } else {
                    let vs = j;
                    while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    val = String::from_utf8_lossy(&b[vs..j]).into_owned();
                }
            }
            attrs.push((aname, decode_entities(val.trim())));
        }
        out.push(Tag { name: name.to_string(), attrs });
        i = j;
    }
    out
}

/// Largest edge in a `sizes` attribute ("16x16 32x32", "any").
fn parse_sizes(s: &str) -> Option<u32> {
    s.split_whitespace()
        .filter_map(|t| {
            let (w, h) = t.to_ascii_lowercase().split_once('x').map(|(w, h)| (w.to_string(), h.to_string()))?;
            Some(w.parse::<u32>().ok()?.max(h.parse::<u32>().ok()?))
        })
        .max()
}

fn looks_svg(url: &str, ty: Option<&str>) -> bool {
    if ty.is_some_and(|t| t.to_ascii_lowercase().contains("svg")) {
        return true;
    }
    if let Some(rest) = url.strip_prefix("data:") {
        return rest.to_ascii_lowercase().starts_with("image/svg");
    }
    let path = url.split(['?', '#']).next().unwrap_or("");
    path.to_ascii_lowercase().ends_with(".svg")
}

fn resolve(base: &Url, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    if href.len() > 64 && href.starts_with("data:") {
        return Some(href.to_string());
    }
    let u = base.join(href).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

/// Icon candidates and the manifest link of a page. `doc` is the final page URL (after redirects).
pub fn parse_html(html: &str, doc: &Url) -> ParsedHtml {
    let tags = scan_tags(html);
    let base = tags
        .iter()
        .find(|t| t.name == "base")
        .and_then(|t| t.attr("href"))
        .and_then(|h| doc.join(h.trim()).ok())
        .unwrap_or_else(|| doc.clone());
    let mut out = ParsedHtml::default();
    for t in tags.iter().filter(|t| t.name == "link") {
        let (Some(rel), Some(href)) = (t.attr("rel"), t.attr("href")) else { continue };
        let rel: Vec<String> = rel.split_whitespace().map(str::to_ascii_lowercase).collect();
        if rel.iter().any(|r| r == "manifest") {
            if out.manifest.is_none() {
                out.manifest = base.join(href.trim()).ok().filter(|u| matches!(u.scheme(), "http" | "https"));
            }
            continue;
        }
        let apple = rel.iter().any(|r| r == "apple-touch-icon" || r == "apple-touch-icon-precomposed");
        if !apple && !rel.iter().any(|r| r == "icon") {
            continue; // mask-icon and others: not usable
        }
        let Some(url) = resolve(&base, href) else { continue };
        out.icons.push(Candidate {
            svg: looks_svg(&url, t.attr("type")),
            size: t.attr("sizes").and_then(parse_sizes),
            apple,
            url,
        });
    }
    out
}

/// `icons[]` of a web app manifest; `base` is the manifest URL. Monochrome icons are skipped.
pub fn parse_manifest(json: &[u8], base: &Url) -> Vec<Candidate> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(json) else { return Vec::new() };
    let Some(icons) = v.get("icons").and_then(|i| i.as_array()) else { return Vec::new() };
    icons
        .iter()
        .filter_map(|i| {
            let src = i.get("src")?.as_str()?;
            let purpose = i.get("purpose").and_then(|p| p.as_str()).unwrap_or("any");
            if purpose.split_whitespace().any(|p| p.eq_ignore_ascii_case("monochrome")) {
                return None;
            }
            let url = resolve(base, src)?;
            Some(Candidate {
                svg: looks_svg(&url, i.get("type").and_then(|t| t.as_str())),
                size: i.get("sizes").and_then(|s| s.as_str()).and_then(parse_sizes),
                apple: false,
                url,
            })
        })
        .collect()
}

/// Order candidates best first (stable: apple-touch-icons win ties, then document order), drop duplicates,
/// and end with `/favicon.ico` at the origin of `site`.
pub fn rank(mut cands: Vec<Candidate>, site: &Url) -> Vec<Candidate> {
    cands.sort_by_key(|c| (c.tier(), !c.apple));
    let mut seen = std::collections::HashSet::new();
    cands.retain(|c| seen.insert(c.url.clone()));
    if let Ok(fav) = site.join("/favicon.ico") {
        if seen.insert(fav.to_string()) {
            cands.push(Candidate { url: fav.to_string(), size: None, svg: false, apple: false });
        }
    }
    cands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn urls(p: &ParsedHtml) -> Vec<&str> {
        p.icons.iter().map(|c| c.url.as_str()).collect()
    }

    #[test]
    fn scheme_rejection() {
        assert!(check_url("https://example.com/x").is_ok());
        assert!(check_url("http://example.com").is_ok());
        assert!(check_url("ftp://example.com").is_err());
        assert!(check_url("file:///etc/passwd").is_err());
        assert!(check_url("javascript:alert(1)").is_err());
        assert!(check_url("not a url").is_err());
        assert!(check_url("").is_err());
        assert!(fetch("ftp://example.com").is_err());
    }

    #[test]
    fn link_attribute_orders_and_quotes() {
        let html = r#"<html><head>
            <link rel="icon" href="/a.png" sizes="32x32">
            <LINK HREF='/b.png' REL='apple-touch-icon' SIZES='180x180'>
            <link href=/c.png rel=icon sizes=64x64 />
            <link type="image/svg+xml" rel="shortcut icon" href="d.svg">
            <link rel="mask-icon" href="/mask.svg">
            <link rel="stylesheet" href="/s.css">
            <link rel="icon">
            </head></html>"#;
        let p = parse_html(html, &u("https://ex.com/dir/page"));
        assert_eq!(
            urls(&p),
            ["https://ex.com/a.png", "https://ex.com/b.png", "https://ex.com/c.png", "https://ex.com/dir/d.svg"]
        );
        assert_eq!(p.icons[0].size, Some(32));
        assert!(p.icons[1].apple && p.icons[1].size == Some(180));
        assert_eq!(p.icons[2].size, Some(64));
        assert!(p.icons[3].svg);
    }

    #[test]
    fn link_urls_relative_absolute_protocol_relative_and_base() {
        let html = r#"<link rel=icon href="//cdn.ex.com/i.png"><link rel=icon href="https://other.com/o.png">
            <link rel=icon href="rel.png"><link rel=icon href="javascript:x">"#;
        let p = parse_html(html, &u("https://ex.com/a/b/"));
        assert_eq!(urls(&p), ["https://cdn.ex.com/i.png", "https://other.com/o.png", "https://ex.com/a/b/rel.png"]);
        let html = r#"<base href="/static/"><link rel=icon href="rel.png"><link rel=manifest href="m.json">"#;
        let p = parse_html(html, &u("https://ex.com/a/b/"));
        assert_eq!(urls(&p), ["https://ex.com/static/rel.png"]);
        assert_eq!(p.manifest.unwrap().as_str(), "https://ex.com/static/m.json");
    }

    #[test]
    fn scanner_skips_comments_scripts_and_decodes_entities() {
        let html = r#"<!-- <link rel="icon" href="/no1.png"> -->
            <script>var s = '<link rel="icon" href="/no2.png">';</script>
            <STYLE>a{}</STYLE>
            <link rel="icon" href="/x.png?a=1&amp;b=2">"#;
        let p = parse_html(html, &u("https://ex.com/"));
        assert_eq!(urls(&p), ["https://ex.com/x.png?a=1&b=2"]);
        // Garbage must not hang or panic.
        let _ = parse_html("<link rel=icon href=\"unterminated", &u("https://ex.com/"));
        let _ = parse_html("<link === ==/ <", &u("https://ex.com/"));
    }

    #[test]
    fn manifest_parsing() {
        let json = br#"{"icons":[
            {"src":"i/192.png","sizes":"192x192","type":"image/png"},
            {"src":"/i/512.png","sizes":"512x512","purpose":"any maskable"},
            {"src":"mono.png","sizes":"512x512","purpose":"monochrome"},
            {"src":"https://c.com/s.svg","sizes":"any","type":"image/svg+xml"}]}"#;
        let c = parse_manifest(json, &u("https://ex.com/app/manifest.json"));
        let got: Vec<_> = c.iter().map(|c| (c.url.as_str(), c.size, c.svg)).collect();
        assert_eq!(
            got,
            [
                ("https://ex.com/app/i/192.png", Some(192), false),
                ("https://ex.com/i/512.png", Some(512), false),
                ("https://c.com/s.svg", None, true)
            ]
        );
        assert!(parse_manifest(b"not json", &u("https://ex.com/")).is_empty());
        assert!(parse_manifest(b"{}", &u("https://ex.com/")).is_empty());
    }

    fn cand(url: &str, size: Option<u32>, svg: bool, apple: bool) -> Candidate {
        Candidate { url: url.into(), size, svg, apple }
    }

    #[test]
    fn ranking() {
        let site = u("https://ex.com/page");
        let ranked = rank(
            vec![
                cand("https://ex.com/16.png", Some(16), false, false),
                cand("https://ex.com/s.svg", None, true, false),
                cand("https://ex.com/512.png", Some(512), false, false),
                cand("https://ex.com/apple.png", None, false, true),
                cand("https://ex.com/256.png", Some(256), false, false),
                cand("https://ex.com/48.png", Some(48), false, false),
                cand("https://ex.com/16.png", Some(16), false, false),
            ],
            &site,
        );
        let order: Vec<_> = ranked.iter().map(|c| c.url.rsplit('/').next().unwrap()).collect();
        assert_eq!(order, ["256.png", "apple.png", "512.png", "s.svg", "48.png", "16.png", "favicon.ico"]);
        // apple wins a tie at the same size
        let r = rank(
            vec![cand("https://ex.com/a", Some(180), false, false), cand("https://ex.com/b", None, false, true)],
            &site,
        );
        assert_eq!(r[0].url, "https://ex.com/b");
        // favicon.ico already present is not duplicated
        let r = rank(vec![cand("https://ex.com/favicon.ico", Some(32), false, false)], &site);
        assert_eq!(r.len(), 1);
    }

    fn test_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().unwrap();
        wr.write_image_data(&vec![200u8; (w * h * 4) as usize]).unwrap();
        wr.finish().unwrap();
        out
    }

    /// ICO with the given (width, height, payload) entries.
    fn make_ico(entries: &[(u8, u8, u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 0];
        out.extend((entries.len() as u16).to_le_bytes());
        let mut off = 6 + 16 * entries.len();
        for (w, h, bpp, data) in entries {
            out.extend([*w, *h, 0, 0, 1, 0]);
            out.extend(bpp.to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((off as u32).to_le_bytes());
            off += data.len();
        }
        for (.., data) in entries {
            out.extend(data);
        }
        out
    }

    /// 2x2 32-bit DIB: bottom row first. Pixels BGRA.
    fn dib32(px_bottom_up: [[u8; 4]; 4], mask: [u8; 2]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(40u32.to_le_bytes());
        d.extend(2i32.to_le_bytes());
        d.extend(4i32.to_le_bytes()); // doubled height
        d.extend(1u16.to_le_bytes());
        d.extend(32u16.to_le_bytes());
        d.extend([0u8; 24]); // compression .. important colors
        for p in px_bottom_up {
            d.extend(p);
        }
        for m in mask {
            d.extend([m, 0, 0, 0]); // each mask row padded to 4 bytes
        }
        d
    }

    fn decode(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut r = png::Decoder::new(std::io::Cursor::new(png_bytes)).read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn ico_png_entry_is_returned_as_is_and_largest_wins() {
        let small = test_png(16, 16);
        let big = test_png(64, 64);
        let ico = make_ico(&[(16, 16, 32, small), (64, 64, 32, big.clone())]);
        assert_eq!(ico_to_png(&ico).unwrap(), big);
        let icon = normalize_image(ico).unwrap();
        assert_eq!(icon.content_type, "image/png");
        assert_eq!(decode(&icon.bytes).0, 64);
    }

    #[test]
    fn ico_bmp_32bit_entry_becomes_png() {
        // Bottom-up: bottom row = red, green; top row = blue, transparent white.
        let dib = dib32([[0, 0, 255, 255], [0, 255, 0, 255], [255, 0, 0, 255], [255, 255, 255, 0]], [0, 0]);
        let ico = make_ico(&[(2, 2, 32, dib)]);
        let png_bytes = ico_to_png(&ico).unwrap();
        let (w, h, px) = decode(&png_bytes);
        assert_eq!((w, h), (2, 2));
        assert_eq!(&px[0..4], &[0, 0, 255, 255]); // top-left blue
        assert_eq!(px[7], 0); // top-right transparent
        assert_eq!(&px[8..12], &[255, 0, 0, 255]); // bottom-left red
        assert_eq!(&px[12..16], &[0, 255, 0, 255]); // bottom-right green
    }

    #[test]
    fn ico_bmp_24bit_uses_and_mask() {
        let mut d = Vec::new();
        d.extend(40u32.to_le_bytes());
        d.extend(2i32.to_le_bytes());
        d.extend(4i32.to_le_bytes());
        d.extend(1u16.to_le_bytes());
        d.extend(24u16.to_le_bytes());
        d.extend([0u8; 24]);
        // rows are 2*3=6 bytes padded to 8
        d.extend([0, 0, 255, 0, 0, 255, 0, 0]); // bottom: red red
        d.extend([255, 0, 0, 255, 0, 0, 0, 0]); // top: blue blue
        d.extend([0b0100_0000, 0, 0, 0]); // bottom mask: second pixel transparent
        d.extend([0, 0, 0, 0]);
        let png_bytes = ico_to_png(&make_ico(&[(2, 2, 24, d)])).unwrap();
        let (_, _, px) = decode(&png_bytes);
        assert_eq!(&px[0..4], &[0, 0, 255, 255]);
        assert_eq!(&px[8..12], &[255, 0, 0, 255]);
        assert_eq!(px[15], 0);
    }

    #[test]
    fn ico_garbage_is_rejected() {
        assert!(ico_to_png(&[0, 0, 1, 0, 5, 0]).is_none());
        assert!(ico_to_png(&make_ico(&[(2, 2, 32, vec![1, 2, 3])])).is_none());
        assert!(normalize_image(b"<html>nope</html>".to_vec()).is_none());
        assert!(normalize_image(Vec::new()).is_none());
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(&test_png(1, 1)), Some(Kind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Kind::Jpeg));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some(Kind::Webp));
        assert_eq!(sniff(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"x\"/>"), Some(Kind::Svg));
        assert_eq!(sniff(b"<!DOCTYPE html><html></html>"), None);
        assert_eq!(sniff(b"GIF89a"), None);
    }

    #[test]
    fn size_caps() {
        let data = vec![7u8; 100];
        let (got, truncated) = read_capped(std::io::Cursor::new(&data), 100).unwrap();
        assert_eq!((got.len(), truncated), (100, false));
        let (got, truncated) = read_capped(std::io::Cursor::new(&data), 99).unwrap();
        assert_eq!((got.len(), truncated), (99, true));
        // data: URLs obey the image cap too
        let big = base64::engine::general_purpose::STANDARD.encode(vec![0u8; IMAGE_CAP + 1]);
        assert!(decode_data_url(&format!("data:image/png;base64,{big}")).is_none());
        assert_eq!(decode_data_url("data:image/png;base64,AAEC"), Some(vec![0, 1, 2]));
        assert_eq!(decode_data_url("data:image/svg+xml,%3Csvg%3E"), Some(b"<svg>".to_vec()));
        assert_eq!(decode_data_url("data:text/plain,%é"), Some("%é".as_bytes().to_vec()));
    }

    /// Live network check: `cargo test -p nd-agent --lib site_icon::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live() {
        for site in [
            "https://www.youtube.com",
            "https://github.com",
            "https://www.twitch.tv",
            "https://kick.com",
            "https://www.reddit.com",
            "https://open.spotify.com",
        ] {
            let t = Instant::now();
            match fetch(site) {
                Ok(Some(i)) => {
                    let dims = if i.content_type == "image/png" { decode(&i.bytes).0.to_string() } else { "-".into() };
                    println!(
                        "{site}: {} {} bytes, png width {dims} ({:?})",
                        i.content_type,
                        i.bytes.len(),
                        t.elapsed()
                    );
                }
                other => println!("{site}: NONE ({other:?}) ({:?})", t.elapsed()),
            }
        }
    }
}
