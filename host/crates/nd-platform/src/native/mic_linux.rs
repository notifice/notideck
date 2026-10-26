// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux phone-mic output (round 25, 27b): a "Notideck Microphone" virtual source.
//!
//! Two backends, chosen per `open`:
//! * PipeWire stream (round 27b): a `pw-cat --playback` child with `media.class=Audio/Source` is
//!   itself a source other apps can record from; the stream writes raw s16le mono into its stdin
//!   (non-blocking, 4 KiB pipe). About 0-5 ms added latency.
//! * `pactl load-module module-pipe-source` (PulseAudio and pipewire-pulse) creates a source that
//!   reads raw s16le mono from a FIFO; the stream writes into that FIFO without blocking.
//!   PipeWire's pipe-tunnel adds a fixed ~170 ms there, so this is only the fallback.
//!
//! Either way the source stays up between opens (apps keep their selected input) until the rate
//! changes or [`MicSink::shutdown`].

use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::Read;
use std::io::Write;
#[cfg(test)]
use std::os::fd::FromRawFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{MicOutputDevice, MicSink, MicStream, PlatformError, Result};

const SOURCE_NAME: &str = "notideck_mic";
const DESCRIPTION: &str = "Notideck Microphone";
const PIPE_SIZE: libc::c_int = 8192;
/// pw-cat stdin pipe: the smallest the kernel gives (one page).
const PW_PIPE_SIZE: libc::c_int = 4096;
/// What the agent keeps queued in the pw-cat pipe (the jitter buffer target).
const PW_TARGET_MS: u32 = 30;
/// The feeder keeps at least this much silence queued while no stream writes.
const FEED_MIN_MS: u32 = 10;
/// A stream counts as writing when it wrote within this long.
const FEED_IDLE: Duration = Duration::from_millis(50);
const FEED_TICK: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    PipeWire,
    PipeSource,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::PipeWire => "pipewire-stream",
            Backend::PipeSource => "pulse-pipe-source",
        }
    }
}

struct Loaded {
    index: u32,
    rate: u32,
}

#[derive(Default)]
struct State {
    loaded: Option<Loaded>,
    cleaned: bool,
    /// Backend of the last `open` (or the last successful detection before any open).
    backend: Option<Backend>,
    pw: Option<PwChild>,
}

pub struct PipeSourceSink {
    state: Mutex<State>,
}

impl PipeSourceSink {
    pub fn new() -> Self {
        PipeSourceSink { state: Mutex::new(State::default()) }
    }
}

impl Default for PipeSourceSink {
    fn default() -> Self {
        Self::new()
    }
}

/// `$XDG_RUNTIME_DIR/notideck/mic.fifo`, else `/tmp/notideck-<uid>/mic.fifo`.
fn fifo_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return Path::new(&dir).join("notideck").join("mic.fifo");
    }
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/notideck-{uid}")).join("mic.fifo")
}

/// Argument vector for `pactl` (without the program name) that loads the pipe source.
fn load_module_args(fifo: &Path, rate: u32) -> Vec<String> {
    vec![
        "load-module".into(),
        "module-pipe-source".into(),
        format!("source_name={SOURCE_NAME}"),
        format!("file={}", fifo.display()),
        "format=s16le".into(),
        format!("rate={rate}"),
        "channels=1".into(),
        format!("source_properties=device.description='{DESCRIPTION}'"),
    ]
}

/// Module indexes of stale notideck pipe sources in `pactl list short modules` output
/// (`<index>\t<name>\t<args>\t...`).
fn parse_stale_modules(list: &str) -> Vec<u32> {
    list.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let idx = it.next()?.trim().parse::<u32>().ok()?;
            let name = it.next()?;
            let args = it.next().unwrap_or("");
            (name == "module-pipe-source" && args.split_whitespace().any(|a| a == format!("source_name={SOURCE_NAME}")))
                .then_some(idx)
        })
        .collect()
}

fn parse_module_index(out: &str) -> Option<u32> {
    out.lines().find_map(|l| l.trim().parse::<u32>().ok())
}

fn pactl(args: &[String]) -> Result<String> {
    let out = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| PlatformError::Other(format!("pactl: {e}")))?;
    if !out.status.success() {
        return Err(PlatformError::Other(format!(
            "pactl {} failed: {}",
            args.first().map(String::as_str).unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn pactl_ok() -> bool {
    pactl(&["info".to_string()]).is_ok()
}

fn unload(index: u32) {
    let _ = pactl(&["unload-module".into(), index.to_string()]);
}

/// True when `pactl info` output names a PipeWire server (`Server Name: PulseAudio (on PipeWire 1.6.9)`).
fn parse_server_is_pipewire(info: &str) -> bool {
    info.lines().any(|l| l.trim().strip_prefix("Server Name:").is_some_and(|v| v.contains("PipeWire")))
}

fn in_path(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

/// `Ok(None)`-free detection: PipeWire stream when the server is PipeWire and `pw-cat` exists.
/// `None` when `pactl info` failed (nothing to cache).
fn detect_backend() -> Option<Backend> {
    let info = pactl(&["info".to_string()]).ok()?;
    Some(if parse_server_is_pipewire(&info) && in_path("pw-cat") { Backend::PipeWire } else { Backend::PipeSource })
}

/// Arguments of the `pw-cat` child (without the program name).
fn pw_cat_args(rate: u32) -> Vec<String> {
    let props = format!(
        "{{ media.class=Audio/Source node.name={SOURCE_NAME} node.description=\"{DESCRIPTION}\" \
         node.autoconnect=false priority.session=1 priority.driver=1 audio.position=[ MONO ] }}"
    );
    vec![
        "--playback".into(),
        "--raw".into(),
        "--format".into(),
        "s16".into(),
        "--rate".into(),
        rate.to_string(),
        "--channels".into(),
        "1".into(),
        "--latency".into(),
        "10ms".into(),
        "-P".into(),
        props,
        "-".into(),
    ]
}

fn fionread(fd: libc::c_int) -> usize {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int through the pointer.
    let r = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) };
    if r < 0 {
        0
    } else {
        n.max(0) as usize
    }
}

/// The write end of the pw-cat stdin pipe, shared by the streams and the feeder thread.
struct PwPipe {
    file: File,
    capacity: usize,
    lost: AtomicBool,
    /// Last time a stream wrote audio; also serialises writes so silence never lands mid-chunk.
    last_write: Mutex<Option<Instant>>,
}

impl PwPipe {
    fn queued_bytes(&self) -> usize {
        fionread(self.file.as_raw_fd())
    }

    /// Non-blocking write of `bytes`; `Ok(written)` (0 on a full pipe), `Err` on a closed reader.
    fn write_bytes(&self, bytes: &[u8]) -> std::io::Result<usize> {
        match (&self.file).write(bytes) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                self.lost.store(true, Ordering::Relaxed);
                Err(e)
            }
            r => r,
        }
    }
}

/// A running `pw-cat` plus the thread that keeps its pipe fed with silence between opens.
struct PwChild {
    child: Child,
    rate: u32,
    pipe: Arc<PwPipe>,
    stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
}

impl PwChild {
    fn spawn(rate: u32) -> Result<PwChild> {
        let mut cmd = Command::new("pw-cat");
        // No PR_SET_PDEATHSIG: it fires when the spawning thread exits, and `open` runs on tokio's
        // short-lived blocking threads. pw-cat exits on stdin EOF instead, which the kernel delivers
        // when the agent dies (even on SIGKILL), since only the agent holds the write end (CLOEXEC).
        cmd.args(pw_cat_args(rate)).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| PlatformError::Other(format!("pw-cat: {e}")))?;
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PlatformError::Other("pw-cat: no stdin".into()));
        };
        let file = File::from(OwnedFd::from(stdin));
        let fd = file.as_raw_fd();
        // SAFETY: valid fd owned by `file`.
        let capacity = unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETPIPE_SZ, PW_PIPE_SIZE);
            libc::fcntl(fd, libc::F_GETPIPE_SZ)
        };
        let capacity = if capacity > 0 { capacity as usize } else { PW_PIPE_SIZE as usize };
        let pipe = Arc::new(PwPipe { file, capacity, lost: AtomicBool::new(false), last_write: Mutex::new(None) });
        let stop = Arc::new(AtomicBool::new(false));
        let feeder = {
            let (pipe, stop) = (pipe.clone(), stop.clone());
            std::thread::Builder::new().name("nd-mic-feed".into()).spawn(move || feed_silence(&pipe, &stop, rate)).ok()
        };
        Ok(PwChild { child, rate, pipe, stop, feeder })
    }

    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(f) = self.feeder.take() {
            let _ = f.join();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Keeps `pipe` at >= [`FEED_MIN_MS`] of silence while no stream wrote for [`FEED_IDLE`], so pw-cat is
/// never starved between opens. Never blocks (the fd is non-blocking).
fn feed_silence(pipe: &PwPipe, stop: &AtomicBool, rate: u32) {
    let min_bytes = (rate as usize * FEED_MIN_MS as usize / 1000) * 2;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(FEED_TICK);
        let Ok(last) = pipe.last_write.lock() else { return };
        if last.is_some_and(|t| t.elapsed() < FEED_IDLE) {
            continue;
        }
        let queued = pipe.queued_bytes();
        if queued < min_bytes {
            // Holding the lock: a stream write waits for this tiny non-blocking write.
            if pipe.write_bytes(&vec![0u8; min_bytes - queued]).is_err() {
                return;
            }
        }
    }
}

impl PipeSourceSink {
    fn stale_cleanup(st: &mut State) {
        if !st.cleaned {
            if let Ok(list) = pactl(&["list".into(), "short".into(), "modules".into()]) {
                for idx in parse_stale_modules(&list) {
                    unload(idx);
                }
            }
            st.cleaned = true;
        }
    }

    fn open_pipewire(&self, sample_rate: u32) -> Result<Box<dyn MicStream>> {
        let mut st = self.state.lock().map_err(|_| PlatformError::Other("mic lock poisoned".into()))?;
        st.backend = Some(Backend::PipeWire);
        // A pipe-source from this run (backend switched) would hold the same node name.
        if let Some(l) = st.loaded.take() {
            unload(l.index);
        }
        if let Some(c) = st.pw.as_mut() {
            if c.rate != sample_rate || c.exited() {
                if let Some(c) = st.pw.take() {
                    c.stop();
                }
            }
        }
        if st.pw.is_none() {
            Self::stale_cleanup(&mut st);
            st.pw = Some(PwChild::spawn(sample_rate)?);
        }
        let pipe = st.pw.as_ref().expect("just spawned").pipe.clone();
        Ok(Box::new(PwStream { pipe }))
    }

    fn open_pipe_source(&self, sample_rate: u32) -> Result<Box<dyn MicStream>> {
        let fifo = fifo_path();
        let dir = fifo.parent().expect("fifo has a parent");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let mut st = self.state.lock().map_err(|_| PlatformError::Other("mic lock poisoned".into()))?;
        st.backend = Some(Backend::PipeSource);
        // A pw-cat child (backend switched) would hold the same node name.
        if let Some(c) = st.pw.take() {
            c.stop();
        }
        Self::stale_cleanup(&mut st);
        if st.loaded.as_ref().is_some_and(|l| l.rate != sample_rate) {
            if let Some(l) = st.loaded.take() {
                unload(l.index);
            }
        }
        if st.loaded.is_none() {
            let out = pactl(&load_module_args(&fifo, sample_rate))?;
            let index = parse_module_index(&out)
                .ok_or_else(|| PlatformError::Other(format!("unexpected pactl output: {}", out.trim())))?;
            st.loaded = Some(Loaded { index, rate: sample_rate });
        }
        drop(st);
        // The module creates the FIFO and holds the read end; wait for it to appear.
        let deadline = Instant::now() + Duration::from_secs(2);
        let file = loop {
            match OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&fifo) {
                Ok(f) => break f,
                Err(e) if Instant::now() < deadline => {
                    tracing::debug!("mic fifo not ready: {e}");
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(PlatformError::Other(format!("open {}: {e}", fifo.display()))),
            }
        };
        // SAFETY: valid fd owned by `file`.
        let cap = unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_SETPIPE_SZ, PIPE_SIZE);
            libc::fcntl(file.as_raw_fd(), libc::F_GETPIPE_SZ)
        };
        let capacity = if cap > 0 { cap as usize } else { PIPE_SIZE as usize };
        Ok(Box::new(PipeStream { file, capacity, lost: false }))
    }
}

impl MicSink for PipeSourceSink {
    fn backend_name(&self) -> &'static str {
        let Ok(mut st) = self.state.lock() else { return Backend::PipeSource.name() };
        if st.backend.is_none() {
            st.backend = detect_backend();
        }
        st.backend.unwrap_or(Backend::PipeSource).name()
    }

    fn devices(&self) -> Result<Vec<MicOutputDevice>> {
        if !pactl_ok() {
            return Ok(Vec::new());
        }
        Ok(vec![MicOutputDevice {
            id: SOURCE_NAME.into(),
            name: format!("{DESCRIPTION} (virtual source)"),
            virtual_cable: true,
            capture_name: DESCRIPTION.into(),
        }])
    }

    fn open(&self, device: &str, sample_rate: u32) -> Result<Box<dyn MicStream>> {
        if !device.is_empty() && device != SOURCE_NAME {
            return Err(PlatformError::Other(format!("unknown microphone output '{device}'")));
        }
        // Detect afresh each open: the audio server may have changed since the last one.
        match detect_backend().unwrap_or(Backend::PipeSource) {
            Backend::PipeWire => self.open_pipewire(sample_rate),
            Backend::PipeSource => self.open_pipe_source(sample_rate),
        }
    }

    fn shutdown(&self) {
        if let Ok(mut st) = self.state.lock() {
            if let Some(c) = st.pw.take() {
                c.stop();
            }
            if let Some(l) = st.loaded.take() {
                unload(l.index);
            }
            let _ = std::fs::remove_file(fifo_path());
        }
    }
}

struct PipeStream {
    file: File,
    capacity: usize,
    lost: bool,
}

impl PipeStream {
    fn queued_bytes(&self) -> usize {
        fionread(self.file.as_raw_fd())
    }
}

impl MicStream for PipeStream {
    fn write(&mut self, samples: &[i16]) -> Result<usize> {
        if self.lost {
            return Err(PlatformError::Other("microphone pipe closed".into()));
        }
        let free_samples = self.capacity.saturating_sub(self.queued_bytes()) / 2;
        if samples.is_empty() {
            return Ok(0);
        }
        // With no room we still try one sample: it fails with EPIPE when the reader is gone.
        let n = samples.len().min(free_samples.max(1));
        let bytes: Vec<u8> = samples[..n].iter().flat_map(|s| s.to_le_bytes()).collect();
        match self.file.write(&bytes) {
            Ok(w) => Ok(w / 2),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                self.lost = true;
                Err(PlatformError::Other("microphone pipe closed".into()))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn queued(&self) -> Result<usize> {
        Ok(self.queued_bytes() / 2)
    }

    fn lost(&self) -> bool {
        self.lost
    }
}

/// Stream into the pw-cat stdin pipe.
struct PwStream {
    pipe: Arc<PwPipe>,
}

impl MicStream for PwStream {
    fn write(&mut self, samples: &[i16]) -> Result<usize> {
        if self.pipe.lost.load(Ordering::Relaxed) {
            return Err(PlatformError::Other("microphone pipe closed".into()));
        }
        if samples.is_empty() {
            return Ok(0);
        }
        let mut last = self.pipe.last_write.lock().map_err(|_| PlatformError::Other("mic pipe poisoned".into()))?;
        let free_samples = self.pipe.capacity.saturating_sub(self.pipe.queued_bytes()) / 2;
        // With no room we still try one sample: it fails with EPIPE when the reader is gone.
        let n = samples.len().min(free_samples.max(1));
        let bytes: Vec<u8> = samples[..n].iter().flat_map(|s| s.to_le_bytes()).collect();
        match self.pipe.write_bytes(&bytes) {
            Ok(w) => {
                if w > 0 {
                    *last = Some(Instant::now());
                }
                Ok(w / 2)
            }
            Err(_) => Err(PlatformError::Other("microphone pipe closed".into())),
        }
    }

    fn queued(&self) -> Result<usize> {
        Ok(self.pipe.queued_bytes() / 2)
    }

    fn target_ms(&self) -> u32 {
        PW_TARGET_MS
    }

    fn lost(&self) -> bool {
        self.pipe.lost.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let a = load_module_args(Path::new("/run/user/1000/notideck/mic.fifo"), 48000);
        assert_eq!(a[0], "load-module");
        assert_eq!(a[1], "module-pipe-source");
        assert!(a.contains(&"source_name=notideck_mic".to_string()));
        assert!(a.contains(&"file=/run/user/1000/notideck/mic.fifo".to_string()));
        assert!(a.contains(&"rate=48000".to_string()));
        assert!(a.contains(&"channels=1".to_string()));
        assert!(a.contains(&"format=s16le".to_string()));
        assert!(a.iter().any(|x| x == "source_properties=device.description='Notideck Microphone'"));
    }

    #[test]
    fn pw_args() {
        let a = pw_cat_args(48000);
        assert_eq!(a.first().map(String::as_str), Some("--playback"));
        assert_eq!(a.last().map(String::as_str), Some("-"));
        let i = a.iter().position(|x| x == "--rate").unwrap();
        assert_eq!(a[i + 1], "48000");
        let i = a.iter().position(|x| x == "-P").unwrap();
        let p = &a[i + 1];
        for want in [
            "media.class=Audio/Source",
            "node.name=notideck_mic",
            "node.description=\"Notideck Microphone\"",
            "node.autoconnect=false",
            "priority.session=1",
            "priority.driver=1",
            "audio.position=[ MONO ]",
        ] {
            assert!(p.contains(want), "{want} missing in {p}");
        }
        assert!(p.starts_with("{ ") && p.ends_with(" }"), "{p}");
        assert_eq!(a.iter().filter(|x| *x == "-P").count(), 1);
    }

    #[test]
    fn server_name() {
        let pw = "Server String: /run/user/1000/pulse/native\nServer Name: PulseAudio (on PipeWire 1.6.9)\nServer Version: 15.0.0\n";
        let pa = "Server String: /run/user/1000/pulse/native\nServer Name: pulseaudio\nServer Version: 17.0\n";
        assert!(parse_server_is_pipewire(pw));
        assert!(!parse_server_is_pipewire(pa));
        assert!(!parse_server_is_pipewire(""));
    }

    #[test]
    fn stale() {
        let list = "1\tmodule-device-restore\t\n\
                    7\tmodule-pipe-source\tsource_name=notideck_mic file=/x format=s16le rate=48000 channels=1\n\
                    8\tmodule-pipe-source\tsource_name=other file=/y\n\
                    9\tmodule-null-sink\tsource_name=notideck_mic\n";
        assert_eq!(parse_stale_modules(list), vec![7]);
        assert!(parse_stale_modules("").is_empty());
    }

    #[test]
    fn module_index() {
        assert_eq!(parse_module_index("42\n"), Some(42));
        assert_eq!(parse_module_index("oops"), None);
    }

    #[test]
    fn stream_over_fifo() {
        // A plain FIFO stands in for the pipe-source module (no PulseAudio needed).
        let dir = std::env::temp_dir().join(format!("nd-mic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.fifo");
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let reader = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).unwrap();
        let file = OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&path).unwrap();
        // SAFETY: valid fd.
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETPIPE_SZ, PIPE_SIZE) };
        let mut s = PipeStream { file, capacity: PIPE_SIZE as usize, lost: false };
        let data = vec![1000i16; 1000];
        assert_eq!(s.write(&data).unwrap(), 1000);
        assert_eq!(s.queued().unwrap(), 1000);
        // Only ~4096 samples fit; the rest is dropped, not blocked.
        let big = vec![5i16; 10_000];
        let n = s.write(&big).unwrap();
        assert!(n > 0 && n <= 3096, "{n}");
        assert_eq!(s.write(&big).unwrap(), 0);
        drop(reader);
        assert!(s.write(&data).is_err() || s.lost());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn anon_pipe() -> (File, File) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: fds has room for two ints.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: both fds are fresh and owned here.
        let (r, w) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        // SAFETY: valid fd.
        unsafe {
            let fl = libc::fcntl(w.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(w.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK);
            libc::fcntl(w.as_raw_fd(), libc::F_SETPIPE_SZ, PW_PIPE_SIZE);
        }
        (r, w)
    }

    fn pw_pipe(w: File) -> Arc<PwPipe> {
        let capacity = unsafe { libc::fcntl(w.as_raw_fd(), libc::F_GETPIPE_SZ) } as usize;
        Arc::new(PwPipe { file: w, capacity, lost: AtomicBool::new(false), last_write: Mutex::new(None) })
    }

    #[test]
    fn stream_over_pipe() {
        let (reader, w) = anon_pipe();
        let mut s = PwStream { pipe: pw_pipe(w) };
        assert_eq!(s.target_ms(), 30);
        let data = vec![1000i16; 480];
        assert_eq!(s.write(&data).unwrap(), 480);
        assert_eq!(s.queued().unwrap(), 480);
        // 4096 bytes = 2048 samples fit; the rest is dropped, not blocked.
        let big = vec![5i16; 10_000];
        let n = s.write(&big).unwrap();
        assert!(n > 0 && n <= 2048 - 480, "{n}");
        assert_eq!(s.write(&big).unwrap(), 0);
        assert!(!s.lost());
        drop(reader);
        assert!(s.write(&data).is_err());
        assert!(s.lost());
    }

    #[test]
    fn feeder_fills_silence_and_stops() {
        let (reader, w) = anon_pipe();
        let pipe = pw_pipe(w);
        let stop = Arc::new(AtomicBool::new(false));
        let h = {
            let (p, st) = (pipe.clone(), stop.clone());
            std::thread::spawn(move || feed_silence(&p, &st, 48000))
        };
        std::thread::sleep(Duration::from_millis(60));
        // 10 ms at 48 kHz mono s16 = 960 bytes, topped up but not beyond.
        assert_eq!(pipe.queued_bytes(), 960);
        // A recent stream write holds the feeder off: drain, then it must stay empty.
        let mut buf = [0u8; 4096];
        let _ = (&reader).read(&mut buf);
        *pipe.last_write.lock().unwrap() = Some(Instant::now() + Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(pipe.queued_bytes(), 0);
        stop.store(true, Ordering::Relaxed);
        h.join().unwrap();
    }
}
