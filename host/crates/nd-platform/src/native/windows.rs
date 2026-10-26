// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows platform backends: `SendInput` keyboard injection and `ShellExecuteW` launcher.
//! Compiled only on `cfg(windows)`; cannot be exercised from WSL/Linux.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use windows::core::{Interface, GUID};
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
    ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as GsmtcManager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as GsmtcProps,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Media::MediaPlaybackAutoRepeatMode;
use windows::Storage::Streams::IRandomAccessStream;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
use windows::Win32::Media::Audio::{eCapture, eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

use crate::{
    AudioControl, AudioTarget, InputInjector, Key, KeyCombo, Launcher, MediaCommand, MediaInfo, NowPlaying,
    PlatformError, RepeatMode, Result,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Maps a [`Key`] to a Windows virtual-key code and whether it needs the extended-key flag.
pub fn to_vk(key: Key) -> (u16, bool) {
    match key {
        Key::Ctrl => (0xA2, false),  // VK_LCONTROL
        Key::Shift => (0xA0, false), // VK_LSHIFT
        Key::Alt => (0xA4, false),   // VK_LMENU
        Key::Meta => (0x5B, true),   // VK_LWIN

        Key::A => (0x41, false),
        Key::B => (0x42, false),
        Key::C => (0x43, false),
        Key::D => (0x44, false),
        Key::E => (0x45, false),
        Key::F => (0x46, false),
        Key::G => (0x47, false),
        Key::H => (0x48, false),
        Key::I => (0x49, false),
        Key::J => (0x4A, false),
        Key::K => (0x4B, false),
        Key::L => (0x4C, false),
        Key::M => (0x4D, false),
        Key::N => (0x4E, false),
        Key::O => (0x4F, false),
        Key::P => (0x50, false),
        Key::Q => (0x51, false),
        Key::R => (0x52, false),
        Key::S => (0x53, false),
        Key::T => (0x54, false),
        Key::U => (0x55, false),
        Key::V => (0x56, false),
        Key::W => (0x57, false),
        Key::X => (0x58, false),
        Key::Y => (0x59, false),
        Key::Z => (0x5A, false),

        Key::Num0 => (0x30, false),
        Key::Num1 => (0x31, false),
        Key::Num2 => (0x32, false),
        Key::Num3 => (0x33, false),
        Key::Num4 => (0x34, false),
        Key::Num5 => (0x35, false),
        Key::Num6 => (0x36, false),
        Key::Num7 => (0x37, false),
        Key::Num8 => (0x38, false),
        Key::Num9 => (0x39, false),

        Key::F1 => (0x70, false),
        Key::F2 => (0x71, false),
        Key::F3 => (0x72, false),
        Key::F4 => (0x73, false),
        Key::F5 => (0x74, false),
        Key::F6 => (0x75, false),
        Key::F7 => (0x76, false),
        Key::F8 => (0x77, false),
        Key::F9 => (0x78, false),
        Key::F10 => (0x79, false),
        Key::F11 => (0x7A, false),
        Key::F12 => (0x7B, false),
        Key::F13 => (0x7C, false),
        Key::F14 => (0x7D, false),
        Key::F15 => (0x7E, false),
        Key::F16 => (0x7F, false),
        Key::F17 => (0x80, false),
        Key::F18 => (0x81, false),
        Key::F19 => (0x82, false),
        Key::F20 => (0x83, false),
        Key::F21 => (0x84, false),
        Key::F22 => (0x85, false),
        Key::F23 => (0x86, false),
        Key::F24 => (0x87, false),

        Key::Enter => (0x0D, false),
        Key::Escape => (0x1B, false),
        Key::Backspace => (0x08, false),
        Key::Tab => (0x09, false),
        Key::Space => (0x20, false),
        Key::Delete => (0x2E, true),
        Key::Insert => (0x2D, true),
        Key::Home => (0x24, true),
        Key::End => (0x23, true),
        Key::PageUp => (0x21, true),
        Key::PageDown => (0x22, true),
        Key::Up => (0x26, true),
        Key::Down => (0x28, true),
        Key::Left => (0x25, true),
        Key::Right => (0x27, true),
        Key::PrintScreen => (0x2C, true), // VK_SNAPSHOT
        Key::CapsLock => (0x14, false),

        Key::Minus => (0xBD, false),        // VK_OEM_MINUS
        Key::Equal => (0xBB, false),        // VK_OEM_PLUS
        Key::LeftBracket => (0xDB, false),  // VK_OEM_4
        Key::RightBracket => (0xDD, false), // VK_OEM_6
        Key::Backslash => (0xDC, false),    // VK_OEM_5
        Key::Semicolon => (0xBA, false),    // VK_OEM_1
        Key::Apostrophe => (0xDE, false),   // VK_OEM_7
        Key::Grave => (0xC0, false),        // VK_OEM_3
        Key::Comma => (0xBC, false),        // VK_OEM_COMMA
        Key::Period => (0xBE, false),       // VK_OEM_PERIOD
        Key::Slash => (0xBF, false),        // VK_OEM_2

        Key::MediaPlayPause => (0xB3, true),
        Key::MediaNext => (0xB0, true),
        Key::MediaPrev => (0xB1, true),
        Key::MediaStop => (0xB2, true),
        Key::VolumeUp => (0xAF, true),
        Key::VolumeDown => (0xAE, true),
        Key::VolumeMute => (0xAD, true),
    }
}

fn vk_input(key: Key, up: bool) -> INPUT {
    let (vk, ext) = to_vk(key);
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if ext {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn unicode_input(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        // Typically blocked by UIPI (target window is elevated) or another thread's input hook.
        return Err(PlatformError::Other(format!(
            "SendInput injected {sent} of {} events (blocked by UIPI / elevated window?)",
            inputs.len()
        )));
    }
    Ok(())
}

/// Keyboard injector backed by `SendInput`.
#[derive(Debug, Default)]
pub struct WindowsInputInjector;

impl WindowsInputInjector {
    pub fn new() -> Self {
        Self
    }
}

/// Builds the event sequence for a combo: modifiers down, key down/up, modifiers up (reverse).
fn combo_events(combo: &KeyCombo) -> Vec<INPUT> {
    let mut ev = Vec::with_capacity(combo.modifiers.len() * 2 + 2);
    for &m in &combo.modifiers {
        ev.push(vk_input(m, false));
    }
    ev.push(vk_input(combo.key, false));
    ev.push(vk_input(combo.key, true));
    for &m in combo.modifiers.iter().rev() {
        ev.push(vk_input(m, true));
    }
    ev
}

impl InputInjector for WindowsInputInjector {
    fn key_combo(&self, combo: &KeyCombo) -> Result<()> {
        // One SendInput call so the sequence cannot be interleaved with real user input.
        send(&combo_events(combo))
    }

    fn type_text(&self, text: &str) -> Result<()> {
        let mut ev: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
        let mut units = [0u16; 2];
        for c in text.chars() {
            match c {
                '\r' => continue,
                '\n' => {
                    ev.push(vk_input(Key::Enter, false));
                    ev.push(vk_input(Key::Enter, true));
                }
                '\t' => {
                    ev.push(vk_input(Key::Tab, false));
                    ev.push(vk_input(Key::Tab, true));
                }
                _ => {
                    // Surrogate pairs are sent as two consecutive unit events.
                    for &u in c.encode_utf16(&mut units).iter() {
                        ev.push(unicode_input(u, false));
                        ev.push(unicode_input(u, true));
                    }
                }
            }
        }
        // Chunk to keep individual calls modest for very long strings.
        for chunk in ev.chunks(512) {
            send(chunk)?;
        }
        Ok(())
    }
}

/// Launcher using `ShellExecuteW` for URLs/files and `std::process::Command` for executables+args.
#[derive(Debug, Default)]
pub struct WindowsLauncher;

impl WindowsLauncher {
    pub fn new() -> Self {
        Self
    }
}

fn wide(s: &str) -> Result<Vec<u16>> {
    if s.contains('\0') {
        return Err(PlatformError::Other("string contains NUL".into()));
    }
    Ok(s.encode_utf16().chain(std::iter::once(0)).collect())
}

/// True when `url` looks like `scheme:...` with a scheme of 2+ chars (so `C:\x` is not a URL).
fn has_url_scheme(url: &str) -> bool {
    match url.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() >= 2
                && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        }
        None => false,
    }
}

fn shell_open(target: &str) -> Result<()> {
    let file = wide(target)?;
    let verb = wide("open")?;
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value > 32 on success.
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(PlatformError::Other(format!("ShellExecuteW failed for '{target}' (code {code})")))
    }
}

impl Launcher for WindowsLauncher {
    fn open_url(&self, url: &str) -> Result<()> {
        if !has_url_scheme(url) {
            return Err(PlatformError::Other(format!("not a URL: '{url}'")));
        }
        if url.len() >= 5 && url[..5].eq_ignore_ascii_case("file:") {
            return Err(PlatformError::PermissionDenied("file: URLs are not allowed".into()));
        }
        tracing::info!(url, "Opening URL with ShellExecuteW");
        shell_open(url)
    }

    fn launch(&self, target: &str, args: &[String]) -> Result<()> {
        tracing::info!(target, ?args, "Launching target");
        if args.is_empty() {
            // Default handler for files/folders/apps; also resolves App Paths and .lnk files.
            return shell_open(target);
        }
        // Args are passed as separate argv entries; no shell is involved.
        Command::new(target)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(PlatformError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vk_mapping_basics() {
        assert_eq!(to_vk(Key::A), (0x41, false));
        assert_eq!(to_vk(Key::F13), (0x7C, false));
        assert_eq!(to_vk(Key::Left), (0x25, true));
        assert_eq!(to_vk(Key::VolumeMute), (0xAD, true));
    }

    #[test]
    fn combo_event_order() {
        let combo = KeyCombo { modifiers: vec![Key::Ctrl, Key::Shift], key: Key::T };
        assert_eq!(combo_events(&combo).len(), 6);
    }

    #[test]
    fn url_scheme_detection() {
        assert!(has_url_scheme("https://example.com"));
        assert!(has_url_scheme("mailto:a@b.c"));
        assert!(!has_url_scheme("C:\\Windows\\notepad.exe"));
        assert!(!has_url_scheme("notepad.exe"));
    }
}

// ---------------------------------------------------------------- audio (WASAPI)

fn win_err(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Other(format!("{what}: {e}"))
}

/// RAII COM initialisation for the current thread. Only uninitialises if init succeeded;
/// a different already-set apartment model (`RPC_E_CHANGED_MODE`) is tolerated.
struct ComGuard(bool);

impl ComGuard {
    fn new() -> Self {
        // S_OK and S_FALSE both need a matching CoUninitialize; errors do not.
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Master volume / mute of the default WASAPI endpoints. Holds no COM pointers; every call
/// sets up COM and looks up the current default device (so device switches are followed).
pub struct WasapiAudio;

impl WasapiAudio {
    pub fn open() -> Result<Self> {
        let a = Self;
        a.volume(AudioTarget::Output)?;
        Ok(a)
    }

    fn with_endpoint<T>(
        target: AudioTarget,
        f: impl FnOnce(&IAudioEndpointVolume) -> windows::core::Result<T>,
    ) -> Result<T> {
        let _com = ComGuard::new();
        let flow = match target {
            AudioTarget::Output => eRender,
            AudioTarget::Input => eCapture,
        };
        let run = || -> windows::core::Result<T> {
            let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eMultimedia)? };
            let vol: IAudioEndpointVolume = unsafe { device.Activate(CLSCTX_ALL, None)? };
            f(&vol)
        };
        // COM objects created in `run` drop before `_com`.
        run().map_err(|e| win_err("wasapi", e))
    }
}

impl AudioControl for WasapiAudio {
    fn backend_name(&self) -> &'static str {
        "wasapi"
    }
    fn volume(&self, target: AudioTarget) -> Result<f32> {
        Self::with_endpoint(target, |v| unsafe { v.GetMasterVolumeLevelScalar() })
    }

    fn set_volume(&self, target: AudioTarget, volume: f32) -> Result<()> {
        let volume = if volume.is_nan() { 0.0 } else { volume.clamp(0.0, 1.0) };
        Self::with_endpoint(target, |v| unsafe { v.SetMasterVolumeLevelScalar(volume, std::ptr::null::<GUID>()) })
    }

    fn muted(&self, target: AudioTarget) -> Result<bool> {
        Self::with_endpoint(target, |v| unsafe { v.GetMute() }.map(|b| b.as_bool()))
    }

    fn set_muted(&self, target: AudioTarget, muted: bool) -> Result<()> {
        Self::with_endpoint(target, |v| unsafe { v.SetMute(muted, std::ptr::null::<GUID>()) })
    }

    fn peak(&self, target: AudioTarget) -> Result<f32> {
        // Re-activated per call (follows default-device changes); a few hundred microseconds.
        let _com = ComGuard::new();
        let flow = match target {
            AudioTarget::Output => eRender,
            AudioTarget::Input => eCapture,
        };
        let run = || -> windows::core::Result<f32> {
            let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eMultimedia)? };
            let meter: IAudioMeterInformation = unsafe { device.Activate(CLSCTX_ALL, None)? };
            unsafe { meter.GetPeakValue() }
        };
        run().map(|p| p.clamp(0.0, 1.0)).map_err(|e| win_err("wasapi", e))
    }
}

// ---------------------------------------------------------------- media (GSMTC)

/// Per-track thumbnail cache entry.
struct ArtCache {
    key: (String, String, String, String),
    art: Option<Arc<[u8]>>,
    tries: u8,
    last_try: Instant,
}

/// Now-playing via Global System Media Transport Controls. The session manager is
/// re-requested on every poll; the struct only caches the album art of the current track.
pub struct GsmtcMedia {
    art: Mutex<Option<ArtCache>>,
}

/// Windows FILETIME epoch (1601) to Unix epoch, in 100 ns ticks.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
const ART_SIZE: u32 = 256;

/// Read the session thumbnail, scale it to fit 256x256 and encode it as PNG.
fn read_thumbnail(props: &GsmtcProps) -> windows::core::Result<Option<Vec<u8>>> {
    let Ok(reference) = props.Thumbnail() else {
        return Ok(None);
    };
    let stream = reference.OpenReadAsync()?.get()?;
    let stream: IRandomAccessStream = stream.cast()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let (w, h) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    if w == 0 || h == 0 {
        return Ok(None);
    }
    let scale = (ART_SIZE as f64 / w as f64).min(ART_SIZE as f64 / h as f64).min(1.0);
    let (tw, th) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(tw)?;
    transform.SetScaledHeight(th)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Straight,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )?
        .get()?;
    let px = data.DetachPixelData()?;
    if px.len() < (tw * th * 4) as usize {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, tw, th);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let ok = enc
        .write_header()
        .and_then(|mut wr| wr.write_image_data(&px[..(tw * th * 4) as usize]).and_then(|_| wr.finish()))
        .is_ok();
    Ok(ok.then_some(out))
}

impl GsmtcMedia {
    pub fn open() -> Result<Self> {
        // WinRT activation needs an initialised apartment on this thread.
        let _com = ComGuard::new();
        GsmtcManager::RequestAsync().and_then(|op| op.get()).map_err(|e| win_err("gsmtc", e))?;
        Ok(Self { art: Mutex::new(None) })
    }

    /// Cached art for this track; reads the thumbnail on a track change, and retries a missing
    /// one at most 3 times, 3 s apart.
    fn art_for(&self, key: (String, String, String, String), props: &GsmtcProps) -> Option<Arc<[u8]>> {
        let mut guard = self.art.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(c)
                if c.key == key
                    && (c.art.is_some() || c.tries >= 3 || c.last_try.elapsed() < Duration::from_secs(3)) =>
            {
                return c.art.clone();
            }
            _ => {}
        }
        let tries = guard.as_ref().filter(|c| c.key == key).map_or(0, |c| c.tries) + 1;
        let art = read_thumbnail(props).ok().flatten().map(Arc::<[u8]>::from);
        *guard = Some(ArtCache { key, art: art.clone(), tries, last_try: Instant::now() });
        art
    }

    fn poll(&self) -> windows::core::Result<Option<NowPlaying>> {
        let manager = GsmtcManager::RequestAsync()?.get()?;
        let Ok(session) = manager.GetCurrentSession() else {
            return Ok(None);
        };
        let props = session.TryGetMediaPropertiesAsync()?.get()?;
        let info = session.GetPlaybackInfo().ok();
        let playing =
            info.as_ref().and_then(|i| i.PlaybackStatus().ok()).map(|s| s == PlaybackStatus::Playing).unwrap_or(false);
        let rate = info
            .as_ref()
            .and_then(|i| i.PlaybackRate().ok())
            .and_then(|r| r.Value().ok())
            .filter(|r| r.is_finite() && *r > 0.0)
            .unwrap_or(1.0);
        let (position_s, length_s, progress) = match timeline(&session, playing, rate) {
            Some((pos, len, _)) => (Some(pos), Some(len), Some((pos / len) as f32)),
            None => (None, None, None),
        };
        let shuffle = info.as_ref().and_then(|i| i.IsShuffleActive().ok()).and_then(|r| r.Value().ok());
        let repeat =
            info.as_ref().and_then(|i| i.AutoRepeatMode().ok()).and_then(|r| r.Value().ok()).map(|m| match m {
                MediaPlaybackAutoRepeatMode::Track => RepeatMode::Track,
                MediaPlaybackAutoRepeatMode::List => RepeatMode::List,
                _ => RepeatMode::Off,
            });
        let can_seek = info
            .as_ref()
            .and_then(|i| i.Controls().ok())
            .and_then(|c| c.IsPlaybackPositionEnabled().ok())
            .unwrap_or(false);
        let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
        let album = props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default();
        let app = session.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
        let art = self.art_for((app.clone(), title.clone(), artist.clone(), album.clone()), &props);
        Ok(Some(NowPlaying {
            title,
            artist,
            album,
            playing,
            progress,
            position_s,
            length_s,
            app,
            art,
            shuffle,
            repeat,
            can_seek,
        }))
    }

    fn run(&self, cmd: MediaCommand) -> Result<()> {
        let to_err = |e| win_err("gsmtc", e);
        let manager = GsmtcManager::RequestAsync().and_then(|o| o.get()).map_err(to_err)?;
        let session = manager.GetCurrentSession().map_err(|_| PlatformError::Other("no media player".into()))?;
        let (name, accepted) = match cmd {
            MediaCommand::PlayPause => ("play/pause", session.TryTogglePlayPauseAsync().and_then(|o| o.get())),
            MediaCommand::Play => ("play", session.TryPlayAsync().and_then(|o| o.get())),
            MediaCommand::Pause => ("pause", session.TryPauseAsync().and_then(|o| o.get())),
            MediaCommand::Next => ("next", session.TrySkipNextAsync().and_then(|o| o.get())),
            MediaCommand::Prev => ("prev", session.TrySkipPreviousAsync().and_then(|o| o.get())),
            MediaCommand::Stop => ("stop", session.TryStopAsync().and_then(|o| o.get())),
            MediaCommand::Seek { .. } | MediaCommand::SeekBy { .. } => {
                let playing = session
                    .GetPlaybackInfo()
                    .ok()
                    .and_then(|i| i.PlaybackStatus().ok())
                    .map(|s| s == PlaybackStatus::Playing)
                    .unwrap_or(false);
                let rate = session
                    .GetPlaybackInfo()
                    .ok()
                    .and_then(|i| i.PlaybackRate().ok())
                    .and_then(|r| r.Value().ok())
                    .filter(|r| r.is_finite() && *r > 0.0)
                    .unwrap_or(1.0);
                let t = session.GetTimelineProperties().map_err(to_err)?;
                let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
                let end = t.EndTime().map(|d| d.Duration).unwrap_or(start);
                let target_s = match cmd {
                    MediaCommand::Seek { position_s } => position_s,
                    MediaCommand::SeekBy { delta_s } => {
                        let (pos, _, _) = timeline(&session, playing, rate)
                            .ok_or_else(|| PlatformError::Other("player does not report a position".into()))?;
                        pos + delta_s
                    }
                    _ => unreachable!(),
                };
                if !target_s.is_finite() {
                    return Err(PlatformError::Other("invalid seek position".into()));
                }
                let ticks = (start as f64 + target_s * 1e7).round().clamp(start as f64, end.max(start) as f64) as i64;
                ("seek", session.TryChangePlaybackPositionAsync(ticks).and_then(|o| o.get()))
            }
            MediaCommand::Shuffle { on } => ("shuffle", session.TryChangeShuffleActiveAsync(on).and_then(|o| o.get())),
            MediaCommand::Repeat { mode } => {
                let m = match mode {
                    RepeatMode::Off => MediaPlaybackAutoRepeatMode::None,
                    RepeatMode::Track => MediaPlaybackAutoRepeatMode::Track,
                    RepeatMode::List => MediaPlaybackAutoRepeatMode::List,
                };
                ("repeat", session.TryChangeAutoRepeatModeAsync(m).and_then(|o| o.get()))
            }
        };
        if accepted.map_err(to_err)? {
            Ok(())
        } else {
            Err(PlatformError::Other(format!("player refused {name}")))
        }
    }
}

/// Interpolated `(position_s, length_s, start_ticks)` of a session, `None` without a usable timeline.
/// Positions are relative to the timeline start and advance with the player's last update while playing.
fn timeline(
    session: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
    playing: bool,
    rate: f64,
) -> Option<(f64, f64, i64)> {
    let t = session.GetTimelineProperties().ok()?;
    let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
    let end = t.EndTime().ok()?.Duration;
    let len = end - start;
    if len <= 0 {
        return None;
    }
    let len_s = len as f64 / 1e7;
    let p = t.Position().ok()?;
    let mut pos = (p.Duration - start) as f64 / 1e7;
    if playing {
        if let (Ok(upd), Ok(now)) = (t.LastUpdatedTime(), SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)) {
            let upd_unix = upd.UniversalTime - UNIX_EPOCH_TICKS;
            let elapsed = now.as_secs_f64() - upd_unix as f64 / 1e7;
            if elapsed > 0.0 {
                pos += elapsed * rate;
            }
        }
    }
    Some((pos.clamp(0.0, len_s), len_s, start))
}

impl MediaInfo for GsmtcMedia {
    fn backend_name(&self) -> &'static str {
        "gsmtc"
    }
    fn now_playing(&self) -> Result<Option<NowPlaying>> {
        let _com = ComGuard::new();
        self.poll().map_err(|e| win_err("gsmtc", e))
    }
    fn control(&self, cmd: MediaCommand) -> Result<()> {
        let _com = ComGuard::new();
        self.run(cmd)
    }
}

#[cfg(test)]
mod media_tests {
    use super::*;

    /// Prints shuffle/repeat/can_seek of the current session (read-only; sends no commands).
    #[test]
    #[ignore]
    fn gsmtc_controls_smoke() {
        let media = GsmtcMedia::open().expect("open gsmtc");
        match media.now_playing().expect("poll") {
            Some(np) => println!("shuffle={:?} repeat={:?} can_seek={}", np.shuffle, np.repeat, np.can_seek),
            None => println!("no media session"),
        }
    }

    /// Prints the current media session (read-only). Run with `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn gsmtc_now_playing_smoke() {
        let media = GsmtcMedia::open().expect("open gsmtc");
        let mut np = media.now_playing().expect("poll");
        let art = np.as_mut().and_then(|n| n.art.take());
        println!("{np:#?}");
        if let Some(a) = art {
            let dec = png::Decoder::new(std::io::Cursor::new(&a[..]));
            let info = dec.read_info().expect("art is valid png");
            println!("art: {} bytes, {}x{}", a.len(), info.info().width, info.info().height);
        } else {
            println!("art: none");
        }
        // Second poll must hit the cache (no re-read).
        let again = media.now_playing().expect("poll 2");
        println!("second poll ok: {}", again.is_some());
    }
}
