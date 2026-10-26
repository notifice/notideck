// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux virtual camera sink: v4l2loopback (SPEC §7.3, PLAN round 26).
//!
//! The agent decodes into [`MemShm`] exactly as on Windows; [`V4l2Sink`] plays the DLL's part. It
//! opens the loopback device as the writer, sets a fixed YUYV output format, writes one frame per
//! 1/fps (the newest shm frame while the producer is fresh, else the placeholder, so the device
//! stays listed) and reports readers through [`crate::shm::Shm::set_consumer`].
//!
//! # Kernel ABI used here
//! The V4L2 ioctl numbers are computed from the struct sizes below with the generic `_IOC`
//! encoding; the sizes/offsets were checked against `/usr/include/linux/videodev2.h` on x86_64
//! (a C program printed `VIDIOC_QUERYCAP=0x80685600`, `VIDIOC_S_FMT=0xc0d05605`,
//! `VIDIOC_S_PARM=0xc0cc5616`, `VIDIOC_SUBSCRIBE_EVENT=0x4020565a`, `VIDIOC_DQEVENT=0x80885659`;
//! the unit tests pin those values on 64-bit targets).
//!
//! The v4l2loopback private ABI (`v4l2loopback.h`, not installed on this system; reproduced from
//! upstream v4l2loopback ≥ 0.13, the installed module is 0.15.4):
//! ```c
//! #define V4L2LOOPBACK_CTL_ADD    0x4C80   /* arg: struct v4l2_loopback_config *; returns device nr */
//! #define V4L2LOOPBACK_CTL_REMOVE 0x4C81   /* arg: int device nr (by value) */
//! #define V4L2LOOPBACK_CTL_QUERY  0x4C82
//! struct v4l2_loopback_config {
//!     __s32 output_nr;   /* -1 = first free */
//!     __s32 unused;      /* was capture_nr; -1 */
//!     char card_label[32];
//!     __u32 min_width, max_width, min_height, max_height;  /* 0 = module default */
//!     __s32 max_buffers, max_openers;                     /* <= 0 = module default */
//!     __s32 debug;
//!     __u8 announce_all_caps;                             /* 0 = exclusive_caps=1 */
//! };
//! /* private event, raised whenever the number of capturing clients changes */
//! #define V4L2_EVENT_PRI_CLIENT_USAGE (V4L2_EVENT_PRIVATE_START + 0x08E00000 + 1)
//! struct v4l2_event_client_usage { __u32 count; };
//! ```
//! Confidence: the control ioctl numbers and the client-usage event are well known (used by
//! `v4l2loopback-ctl` and PipeWire/OBS integrations); the exact config field list is from memory,
//! so the config is passed inside a zero-padded 256-byte buffer (unknown trailing fields read as 0)
//! and `v4l2loopback-ctl` is preferred whenever it is installed.

use std::path::{Path, PathBuf};

use crate::mem::MemShm;
use crate::shm::{nv12_len, ReadResult, Shm, FRIENDLY_NAME, MAX_HEIGHT, MAX_WIDTH};

/// Where the module stands for the running kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopbackState {
    /// No `v4l2loopback.ko*` under `/lib/modules/<release>`.
    ModuleMissing,
    /// Module on disk, not loaded.
    NotLoaded,
    /// Loaded, but no device labelled [`crate::shm::FRIENDLY_NAME`].
    NoDevice,
    /// Our device, e.g. `/dev/video10`.
    Ready(PathBuf),
}

/// Fixed output format (YUYV). Even sizes, at most `MAX_WIDTH`×`MAX_HEIGHT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for OutputFormat {
    fn default() -> Self {
        OutputFormat { width: 1280, height: 720, fps: 30 }
    }
}

impl OutputFormat {
    /// YUYV frame size in bytes.
    pub fn yuyv_len(&self) -> usize {
        self.width as usize * self.height as usize * 2
    }

    fn validate(&self) -> anyhow::Result<()> {
        let ok = self.width >= 2
            && self.height >= 2
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && self.width <= MAX_WIDTH
            && self.height <= MAX_HEIGHT
            && (1..=120).contains(&self.fps);
        anyhow::ensure!(ok, "unsupported output format {}x{}@{}", self.width, self.height, self.fps);
        Ok(())
    }
}

const SYS_ROOT: &str = "/sys";
const MODULES_ROOT: &str = "/lib/modules";
/// Persistent module options (read by `modprobe`).
pub const MODPROBE_CONF: &str = "etc/modprobe.d/notideck-v4l2loopback.conf";
/// Load the module at boot (systemd-modules-load).
pub const MODULES_LOAD_CONF: &str = "etc/modules-load.d/notideck-v4l2loopback.conf";
const CTL_DEVICE: &str = "/dev/v4l2loopback";
const CTL_PROGRAM: &str = "v4l2loopback-ctl";

pub fn loopback_state() -> LoopbackState {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    state_at(Path::new(SYS_ROOT), Path::new(MODULES_ROOT), release.trim())
}

/// [`loopback_state`] with injectable roots (unit tests).
fn state_at(sys: &Path, modules: &Path, release: &str) -> LoopbackState {
    if !sys.join("module/v4l2loopback").exists() {
        let present = !release.is_empty() && has_module_file(&modules.join(release), 0);
        return if present { LoopbackState::NotLoaded } else { LoopbackState::ModuleMissing };
    }
    match find_device(sys) {
        Some(dev) => LoopbackState::Ready(dev),
        None => LoopbackState::NoDevice,
    }
}

/// Recursive search for `v4l2loopback.ko*` (`.ko`, `.ko.zst`, `.ko.xz`, `.ko.gz`).
fn has_module_file(dir: &Path, depth: u32) -> bool {
    if depth > 12 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if has_module_file(&e.path(), depth + 1) {
                return true;
            }
        } else if e.file_name().to_string_lossy().starts_with("v4l2loopback.ko") {
            return true;
        }
    }
    false
}

/// `/dev/videoN` whose sysfs `name` is [`FRIENDLY_NAME`] (lowest N if several).
fn find_device(sys: &Path) -> Option<PathBuf> {
    let mut found: Vec<(u32, String)> = video_nodes(sys)
        .into_iter()
        .filter(|(_, name, label, _)| label == FRIENDLY_NAME && name.starts_with("video"))
        .map(|(n, name, _, _)| (n, name))
        .collect();
    found.sort();
    found.first().map(|(_, name)| Path::new("/dev").join(name))
}

/// `(number, node name, label, is_virtual)` for every `/sys/class/video4linux/video*`.
/// v4l2loopback devices have no parent device, so their sysfs path is under `devices/virtual/`.
fn video_nodes(sys: &Path) -> Vec<(u32, String, String, bool)> {
    let Ok(entries) = std::fs::read_dir(sys.join("class/video4linux")) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(n) = name.strip_prefix("video").and_then(|s| s.parse::<u32>().ok()) else { continue };
        let label = std::fs::read_to_string(e.path().join("name")).unwrap_or_default().trim().to_string();
        let virt = std::fs::read_link(e.path()).map(|t| t.to_string_lossy().contains("/virtual/")).unwrap_or(false);
        out.push((n, name, label, virt));
    }
    out
}

/// NV12 (`nv12_len(w, h)` bytes) to packed YUYV (`w * h * 2` bytes). `w`, `h` even.
pub fn nv12_to_yuyv(nv12: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let (w, h) = (width as usize, height as usize);
    assert!(w.is_multiple_of(2) && h.is_multiple_of(2), "nv12_to_yuyv: odd size {w}x{h}");
    assert_eq!(nv12.len(), nv12_len(width, height), "nv12_to_yuyv: input size");
    assert_eq!(out.len(), w * h * 2, "nv12_to_yuyv: output size");
    if w == 0 || h == 0 {
        return;
    }
    let (y_plane, uv_plane) = nv12.split_at(w * h);
    for (row, out_row) in out.chunks_exact_mut(w * 2).enumerate() {
        let y_row = &y_plane[row * w..(row + 1) * w];
        let uv_row = &uv_plane[(row / 2) * w..(row / 2 + 1) * w];
        for ((o, y), uv) in
            out_row.as_chunks_mut::<4>().0.iter_mut().zip(y_row.as_chunks::<2>().0).zip(uv_row.as_chunks::<2>().0)
        {
            o[0] = y[0];
            o[1] = uv[0];
            o[2] = y[1];
            o[3] = uv[1];
        }
    }
}

/// Which frame to write, mirroring `nd-vcam-dll`'s `FrameProvider`: the newest shm frame of the
/// output size while the producer is fresh, the last converted frame again when nothing new (or
/// a torn read), else the placeholder.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Source {
    Fresh,
    Repeated,
    Placeholder,
}

struct FrameSelector {
    width: u32,
    height: u32,
    last_seq: u64,
    has_last: bool,
    nv12: Vec<u8>,
    yuyv: Vec<u8>,
    placeholder: Vec<u8>,
}

impl FrameSelector {
    fn new(width: u32, height: u32) -> Self {
        let mut placeholder = vec![0u8; width as usize * height as usize * 2];
        nv12_to_yuyv(&crate::placeholder::cached(width, height), width, height, &mut placeholder);
        FrameSelector {
            width,
            height,
            last_seq: 0,
            has_last: false,
            nv12: vec![0u8; nv12_len(width, height)],
            yuyv: vec![0u8; placeholder.len()],
            placeholder,
        }
    }

    fn next(&mut self, shm: &Shm) -> (Source, &[u8]) {
        match shm.read_frame(self.last_seq, self.width, self.height, &mut self.nv12) {
            ReadResult::Frame(seq) => {
                self.last_seq = seq;
                nv12_to_yuyv(&self.nv12, self.width, self.height, &mut self.yuyv);
                self.has_last = true;
                (Source::Fresh, &self.yuyv)
            }
            // Only `Frame` converts into `yuyv`, so a torn copy in `nv12` never leaks out.
            ReadResult::Unchanged | ReadResult::Torn if self.has_last => (Source::Repeated, &self.yuyv),
            ReadResult::NoProducer => {
                self.has_last = false;
                self.last_seq = 0;
                (Source::Placeholder, &self.placeholder)
            }
            _ => (Source::Placeholder, &self.placeholder),
        }
    }
}

/// Count the processes (other than `exclude_pid`) holding `dev` open, by scanning
/// `<proc>/<pid>/fd/*` symlinks. Fallback only: it counts openers, so a process that merely
/// probes/enumerates the device while we look is counted too, and processes of other users are
/// invisible without privileges.
fn count_openers(proc_root: &Path, dev: &Path, exclude_pid: u32) -> u32 {
    let Ok(entries) = std::fs::read_dir(proc_root) else { return 0 };
    let mut count = 0;
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == exclude_pid {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else { continue };
        if fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == dev)) {
            count += 1;
        }
    }
    count
}

/// The writer side of the loopback device. Drop stops the writer thread and closes the device.
pub struct V4l2Sink {
    fmt: OutputFormat,
    inner: sys::Running,
}

impl V4l2Sink {
    pub fn start(dev: &Path, shm: MemShm, fmt: OutputFormat) -> anyhow::Result<V4l2Sink> {
        fmt.validate()?;
        let inner = sys::Running::start(dev, shm, fmt)?;
        Ok(V4l2Sink { fmt, inner })
    }

    pub fn format(&self) -> OutputFormat {
        self.fmt
    }

    /// Processes currently capturing from the device.
    pub fn readers(&self) -> u32 {
        self.inner.readers()
    }

    /// Last open/format/write error, if the writer is failing.
    pub fn error(&self) -> Option<String> {
        self.inner.error()
    }
}

/// Run as root (`pkexec <exe> --install-vcam`): persist the module options + autoload and create
/// our device now. Returns a human-readable result line.
pub fn install() -> anyhow::Result<String> {
    sys::require_root()?;
    let state = loopback_state();
    if state == LoopbackState::ModuleMissing {
        anyhow::bail!(
            "the v4l2loopback kernel module is not installed for this kernel; install v4l2loopback-dkms \
             (or your distribution's v4l2loopback module package) and try again"
        );
    }
    write_config_files(Path::new("/"))?;
    let ctl = find_program(CTL_PROGRAM, std::env::var_os("PATH").as_deref());
    match install_plan(&state) {
        Some(Step::Modprobe) => {
            sys::run(Path::new("modprobe"), &modprobe_args())?;
            match loopback_state() {
                LoopbackState::Ready(dev) => Ok(format!("{FRIENDLY_NAME} ready at {}", dev.display())),
                // Loaded with someone else's options (no device of ours): add one.
                _ => add_device(ctl.as_deref()),
            }
        }
        Some(Step::Add) => add_device(ctl.as_deref()),
        None => match state {
            LoopbackState::Ready(dev) => Ok(format!("{FRIENDLY_NAME} already installed at {}", dev.display())),
            other => anyhow::bail!("unexpected v4l2loopback state {other:?}"),
        },
    }
}

fn add_device(ctl: Option<&Path>) -> anyhow::Result<String> {
    let mut errors = Vec::new();
    if let Some(ctl) = ctl {
        match sys::run(ctl, &ctl_add_args()) {
            Ok(()) => {
                if let LoopbackState::Ready(dev) = loopback_state() {
                    return Ok(format!("{FRIENDLY_NAME} added at {}", dev.display()));
                }
                errors.push(format!("{CTL_PROGRAM} add: no device labelled {FRIENDLY_NAME} afterwards"));
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    match sys::ctl_add() {
        Ok(nr) => return Ok(format!("{FRIENDLY_NAME} added at /dev/video{nr}")),
        Err(e) => errors.push(format!("{e:#}")),
    }
    anyhow::bail!(
        "could not add a {FRIENDLY_NAME} device to the loaded v4l2loopback module ({}); close apps using \
         other loopback cameras, run `sudo modprobe -r v4l2loopback` and try again",
        errors.join("; ")
    )
}

/// Run as root: remove our config files and our device only.
pub fn uninstall() -> anyhow::Result<String> {
    sys::require_root()?;
    remove_config_files(Path::new("/"))?;
    let LoopbackState::Ready(dev) = loopback_state() else {
        return Ok(format!("{FRIENDLY_NAME} config removed (no device present)"));
    };
    let nr = device_number(&dev).ok_or_else(|| anyhow::anyhow!("unexpected device path {}", dev.display()))?;
    let ctl = find_program(CTL_PROGRAM, std::env::var_os("PATH").as_deref());
    let mut errors = Vec::new();
    if let Some(ctl) = &ctl {
        match sys::run(ctl, &ctl_delete_args(&dev)) {
            Ok(()) => return Ok(format!("{FRIENDLY_NAME} removed ({})", dev.display())),
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    match sys::ctl_remove(nr) {
        Ok(()) => return Ok(format!("{FRIENDLY_NAME} removed ({})", dev.display())),
        Err(e) => errors.push(format!("{e:#}")),
    }
    let others = other_loopback_devices(Path::new(SYS_ROOT), nr);
    if may_unload_module(others) {
        sys::run(Path::new("modprobe"), &["-r", "v4l2loopback"])
            .map_err(|e| anyhow::anyhow!("{e:#} (earlier: {})", errors.join("; ")))?;
        return Ok("v4l2loopback unloaded".into());
    }
    anyhow::bail!(
        "could not remove {} ({}); other loopback devices exist, so the module was left loaded \
         (it is removed at the next reboot)",
        dev.display(),
        errors.join("; ")
    )
}

// ---- install/uninstall planning (pure) ----

pub fn modprobe_conf_contents() -> String {
    format!("# Written by Notideck.\noptions v4l2loopback exclusive_caps=1 card_label=\"{FRIENDLY_NAME}\"\n")
}

pub fn modules_load_conf_contents() -> &'static str {
    "# Written by Notideck.\nv4l2loopback\n"
}

fn write_config_files(root: &Path) -> anyhow::Result<()> {
    for (rel, contents) in
        [(MODPROBE_CONF, modprobe_conf_contents()), (MODULES_LOAD_CONF, modules_load_conf_contents().into())]
    {
        let path = root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, contents).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn remove_config_files(root: &Path) -> anyhow::Result<()> {
    for rel in [MODPROBE_CONF, MODULES_LOAD_CONF] {
        let path = root.join(rel);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => anyhow::bail!("remove {}: {e}", path.display()),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// `modprobe v4l2loopback` (options come from our modprobe.d file, written first).
    Modprobe,
    /// Add our device to the loaded module (`v4l2loopback-ctl add`, else `CTL_ADD`).
    Add,
}

/// What `install` does for a state (after writing the config files). `Modprobe` falls through to
/// `Add` when the module came up without our device (e.g. other options in another modprobe.d file).
fn install_plan(state: &LoopbackState) -> Option<Step> {
    match state {
        LoopbackState::ModuleMissing | LoopbackState::Ready(_) => None,
        LoopbackState::NotLoaded => Some(Step::Modprobe),
        LoopbackState::NoDevice => Some(Step::Add),
    }
}

/// `modprobe` arguments. The options are deliberately not repeated on the command line: our
/// modprobe.d file is written first and carries them, which avoids quoting the space in the label
/// through modprobe's option joining.
fn modprobe_args() -> Vec<&'static str> {
    vec!["v4l2loopback"]
}

fn ctl_add_args() -> Vec<&'static str> {
    vec!["add", "-x", "1", "-n", FRIENDLY_NAME]
}

fn ctl_delete_args(dev: &Path) -> Vec<&str> {
    vec!["delete", dev.to_str().unwrap_or("")]
}

/// Unloading the whole module is only acceptable when ours is the only loopback device.
fn may_unload_module(other_loopback_devices: usize) -> bool {
    other_loopback_devices == 0
}

fn other_loopback_devices(sys: &Path, ours: u32) -> usize {
    video_nodes(sys).into_iter().filter(|(n, _, _, virt)| *virt && *n != ours).count()
}

fn device_number(dev: &Path) -> Option<u32> {
    dev.file_name()?.to_str()?.strip_prefix("video")?.parse().ok()
}

/// `name` in `PATH` or the usual system directories (pkexec runs with a minimal `PATH`).
fn find_program(name: &str, path_env: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = path_env.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    for d in ["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"] {
        dirs.push(PathBuf::from(d));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

// ---- kernel interface ----

mod sys {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{count_openers, FrameSelector, OutputFormat};
    use crate::mem::MemShm;

    // _IOC encoding (asm-generic; x86, arm, aarch64, riscv).
    const IOC_WRITE: u32 = 1;
    const IOC_READ: u32 = 2;
    const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
        (dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr
    }

    pub(super) const VIDIOC_QUERYCAP: u32 = ioc(IOC_READ, 0, size_of::<Capability>());
    pub(super) const VIDIOC_S_FMT: u32 = ioc(IOC_READ | IOC_WRITE, 5, size_of::<Format>());
    pub(super) const VIDIOC_S_PARM: u32 = ioc(IOC_READ | IOC_WRITE, 22, size_of::<StreamParm>());
    pub(super) const VIDIOC_DQEVENT: u32 = ioc(IOC_READ, 89, size_of::<Event>());
    pub(super) const VIDIOC_SUBSCRIBE_EVENT: u32 = ioc(IOC_WRITE, 90, size_of::<EventSubscription>());

    const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
    /// `v4l2_fourcc('Y','U','Y','V')`.
    const V4L2_PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
    const V4L2_FIELD_NONE: u32 = 1;
    const V4L2_COLORSPACE_SRGB: u32 = 8;
    const V4L2_CAP_TIMEPERFRAME: u32 = 0x1000;
    const V4L2_EVENT_PRIVATE_START: u32 = 0x0800_0000;
    pub(super) const V4L2_EVENT_PRI_CLIENT_USAGE: u32 = V4L2_EVENT_PRIVATE_START + 0x08E0_0000 + 1;
    const V4L2_EVENT_SUB_FL_SEND_INITIAL: u32 = 1;

    const V4L2LOOPBACK_CTL_ADD: u32 = 0x4C80;
    const V4L2LOOPBACK_CTL_REMOVE: u32 = 0x4C81;

    /// `struct v4l2_capability` (104 bytes).
    #[repr(C)]
    pub(super) struct Capability {
        driver: [u8; 16],
        card: [u8; 32],
        bus_info: [u8; 32],
        version: u32,
        capabilities: u32,
        device_caps: u32,
        reserved: [u32; 3],
    }

    /// `struct v4l2_pix_format` (48 bytes).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PixFormat {
        width: u32,
        height: u32,
        pixelformat: u32,
        field: u32,
        bytesperline: u32,
        sizeimage: u32,
        colorspace: u32,
        priv_: u32,
        flags: u32,
        ycbcr_enc: u32,
        quantization: u32,
        xfer_func: u32,
    }

    /// `struct v4l2_format` (208 bytes on 64-bit: the `fmt` union holds pointers, so it is
    /// 8-aligned at offset 8).
    #[repr(C, align(8))]
    pub(super) struct Format {
        type_: u32,
        _pad: u32,
        pix: PixFormat,
        _raw: [u8; 200 - 48],
    }

    /// `struct v4l2_outputparm` (40 bytes).
    #[repr(C)]
    struct OutputParm {
        capability: u32,
        outputmode: u32,
        timeperframe_num: u32,
        timeperframe_den: u32,
        extendedmode: u32,
        writebuffers: u32,
        reserved: [u32; 4],
    }

    /// `struct v4l2_streamparm` (204 bytes, 4-aligned).
    #[repr(C)]
    pub(super) struct StreamParm {
        type_: u32,
        output: OutputParm,
        _raw: [u8; 200 - 40],
    }

    /// `struct v4l2_event_subscription` (32 bytes).
    #[repr(C)]
    pub(super) struct EventSubscription {
        type_: u32,
        id: u32,
        flags: u32,
        reserved: [u32; 5],
    }

    /// `struct v4l2_event` (136 bytes on 64-bit: `u` is 8-aligned at offset 8, `timestamp` is
    /// a 64-bit `struct timespec`).
    #[repr(C, align(8))]
    pub(super) struct Event {
        type_: u32,
        _pad: u32,
        u: [u8; 64],
        pending: u32,
        sequence: u32,
        timestamp: [i64; 2],
        id: u32,
        reserved: [u32; 8],
        _pad2: u32,
    }

    /// `struct v4l2_loopback_config` (see the module docs), inside a zeroed 256-byte buffer.
    #[repr(C)]
    pub(super) struct LoopbackConfig {
        output_nr: i32,
        unused: i32,
        card_label: [u8; 32],
        min_width: u32,
        max_width: u32,
        min_height: u32,
        max_height: u32,
        max_buffers: i32,
        max_openers: i32,
        debug: i32,
        announce_all_caps: u8,
        _tail: [u8; 256 - 69],
    }

    fn zeroed<T>() -> T {
        // SAFETY: only used for the plain-old-data ioctl structs above (all-zero is valid).
        unsafe { std::mem::zeroed() }
    }

    fn ioctl<T>(fd: &OwnedFd, req: u32, arg: *mut T) -> std::io::Result<i32> {
        // SAFETY: `arg` points at a live struct of the size encoded in `req` (or is ignored).
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), req as libc::Ioctl, arg) };
        if r < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    fn open(path: &Path, flags: i32) -> std::io::Result<OwnedFd> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))?;
        // SAFETY: valid C string; the fd is owned by the returned OwnedFd.
        let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fresh fd from open().
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn cstr(bytes: &[u8]) -> String {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    pub(super) fn require_root() -> anyhow::Result<()> {
        // SAFETY: trivial libc call.
        anyhow::ensure!(unsafe { libc::geteuid() } == 0, "must run as root (pkexec)");
        Ok(())
    }

    pub(super) fn run(program: &Path, args: &[&str]) -> anyhow::Result<()> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| anyhow::anyhow!("{} {}: {e}", program.display(), args.join(" ")))?;
        anyhow::ensure!(
            out.status.success(),
            "{} {} failed ({}): {}",
            program.display(),
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(())
    }

    pub(super) fn ctl_add() -> anyhow::Result<u32> {
        let fd = open(Path::new(super::CTL_DEVICE), libc::O_RDWR)
            .map_err(|e| anyhow::anyhow!("open {}: {e}", super::CTL_DEVICE))?;
        let mut cfg: LoopbackConfig = zeroed();
        cfg.output_nr = -1;
        cfg.unused = -1;
        let label = crate::shm::FRIENDLY_NAME.as_bytes();
        cfg.card_label[..label.len()].copy_from_slice(label);
        // Widths/buffers/openers 0 = module defaults; announce_all_caps 0 = exclusive_caps=1.
        cfg.announce_all_caps = 0;
        let nr =
            ioctl(&fd, V4L2LOOPBACK_CTL_ADD, &mut cfg).map_err(|e| anyhow::anyhow!("V4L2LOOPBACK_CTL_ADD: {e}"))?;
        Ok(nr as u32)
    }

    pub(super) fn ctl_remove(nr: u32) -> anyhow::Result<()> {
        let fd = open(Path::new(super::CTL_DEVICE), libc::O_RDWR)
            .map_err(|e| anyhow::anyhow!("open {}: {e}", super::CTL_DEVICE))?;
        // The device number is passed by value, not by pointer.
        // SAFETY: integer argument.
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), V4L2LOOPBACK_CTL_REMOVE as libc::Ioctl, nr as libc::c_ulong) };
        anyhow::ensure!(r >= 0, "V4L2LOOPBACK_CTL_REMOVE {nr}: {}", std::io::Error::last_os_error());
        Ok(())
    }

    /// Open + verify + set the output format.
    fn open_device(dev: &Path, fmt: OutputFormat) -> anyhow::Result<OwnedFd> {
        // O_NONBLOCK: VIDIOC_DQEVENT then returns ENOENT instead of blocking when no event is
        // queued (core v4l2 behaviour); v4l2loopback's write() does not block either way, an
        // EAGAIN would just skip one frame.
        let fd =
            open(dev, libc::O_RDWR | libc::O_NONBLOCK).map_err(|e| anyhow::anyhow!("open {}: {e}", dev.display()))?;
        let mut cap: Capability = zeroed();
        ioctl(&fd, VIDIOC_QUERYCAP, &mut cap).map_err(|e| anyhow::anyhow!("VIDIOC_QUERYCAP {}: {e}", dev.display()))?;
        let driver = cstr(&cap.driver);
        anyhow::ensure!(
            driver == "v4l2 loopback",
            "{} is not a v4l2loopback device (driver {driver:?})",
            dev.display()
        );

        let mut f: Format = zeroed();
        f.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        f.pix = PixFormat {
            width: fmt.width,
            height: fmt.height,
            pixelformat: V4L2_PIX_FMT_YUYV,
            field: V4L2_FIELD_NONE,
            bytesperline: fmt.width * 2,
            sizeimage: fmt.yuyv_len() as u32,
            colorspace: V4L2_COLORSPACE_SRGB,
            priv_: 0,
            flags: 0,
            ycbcr_enc: 0,
            quantization: 0,
            xfer_func: 0,
        };
        ioctl(&fd, VIDIOC_S_FMT, &mut f).map_err(|e| {
            anyhow::anyhow!(
                "VIDIOC_S_FMT {}x{} YUYV on {}: {e} (is another app writing to it?)",
                fmt.width,
                fmt.height,
                dev.display()
            )
        })?;
        let p = f.pix;
        anyhow::ensure!(
            p.width == fmt.width
                && p.height == fmt.height
                && p.pixelformat == V4L2_PIX_FMT_YUYV
                && p.sizeimage as usize == fmt.yuyv_len(),
            "device adjusted the format to {}x{} fourcc {:#x} sizeimage {} (wanted {}x{} YUYV)",
            p.width,
            p.height,
            p.pixelformat,
            p.sizeimage,
            fmt.width,
            fmt.height
        );

        // Frame rate: optional (advertised to readers); ignore failure.
        let mut parm: StreamParm = zeroed();
        parm.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        parm.output.capability = V4L2_CAP_TIMEPERFRAME;
        parm.output.timeperframe_num = 1;
        parm.output.timeperframe_den = fmt.fps;
        let _ = ioctl(&fd, VIDIOC_S_PARM, &mut parm);
        Ok(fd)
    }

    fn subscribe_client_usage(fd: &OwnedFd) -> bool {
        let mut sub: EventSubscription = zeroed();
        sub.type_ = V4L2_EVENT_PRI_CLIENT_USAGE;
        sub.flags = V4L2_EVENT_SUB_FL_SEND_INITIAL;
        ioctl(fd, VIDIOC_SUBSCRIBE_EVENT, &mut sub).is_ok()
    }

    /// Drain queued events; the latest client-usage count, if any arrived.
    fn drain_client_usage(fd: &OwnedFd) -> Option<u32> {
        let mut latest = None;
        for _ in 0..64 {
            let mut ev: Event = zeroed();
            if ioctl(fd, VIDIOC_DQEVENT, &mut ev).is_err() {
                break; // ENOENT: queue empty
            }
            if ev.type_ == V4L2_EVENT_PRI_CLIENT_USAGE {
                latest = Some(u32::from_ne_bytes([ev.u[0], ev.u[1], ev.u[2], ev.u[3]]));
            }
            if ev.pending == 0 {
                break;
            }
        }
        latest
    }

    enum Readers {
        Events,
        ProcScan { dev: PathBuf, next: Instant },
    }

    struct Shared {
        stop: AtomicBool,
        readers: AtomicU32,
        error: Mutex<Option<String>>,
    }

    impl Shared {
        fn set_error(&self, e: Option<String>) {
            *self.error.lock().unwrap_or_else(|p| p.into_inner()) = e;
        }
    }

    pub(super) struct Running {
        shared: Arc<Shared>,
        thread: Option<JoinHandle<()>>,
        shm: MemShm,
        fmt: OutputFormat,
    }

    impl Running {
        pub(super) fn start(dev: &Path, shm: MemShm, fmt: OutputFormat) -> anyhow::Result<Running> {
            let fd = open_device(dev, fmt)?;
            let readers = if subscribe_client_usage(&fd) {
                Readers::Events
            } else {
                let dev = std::fs::canonicalize(dev).unwrap_or_else(|_| dev.to_path_buf());
                Readers::ProcScan { dev, next: Instant::now() }
            };
            let shared =
                Arc::new(Shared { stop: AtomicBool::new(false), readers: AtomicU32::new(0), error: Mutex::new(None) });
            // A previous sink may have left the consumer marked active.
            shm.shm().set_consumer(false, fmt.width, fmt.height, fmt.fps);
            let thread = {
                let (shared, shm) = (shared.clone(), shm.clone());
                std::thread::Builder::new()
                    .name("nd-v4l2-sink".into())
                    .spawn(move || writer(fd, shm, fmt, readers, &shared))
                    .map_err(|e| anyhow::anyhow!("spawn writer: {e}"))?
            };
            Ok(Running { shared, thread: Some(thread), shm, fmt })
        }

        pub(super) fn readers(&self) -> u32 {
            self.shared.readers.load(Ordering::Relaxed)
        }

        pub(super) fn error(&self) -> Option<String> {
            self.shared.error.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.shared.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join(); // closes the fd (owned by the thread)
            }
            self.shm.shm().set_consumer(false, self.fmt.width, self.fmt.height, self.fmt.fps);
        }
    }

    fn writer(fd: OwnedFd, shm: MemShm, fmt: OutputFormat, mut readers: Readers, shared: &Shared) {
        let period = Duration::from_secs_f64(1.0 / f64::from(fmt.fps.max(1)));
        let mut sel = FrameSelector::new(fmt.width, fmt.height);
        let my_pid = std::process::id();
        let mut count = 0u32;
        let mut next = Instant::now();
        while !shared.stop.load(Ordering::Relaxed) {
            // Readers.
            let now = Instant::now();
            let new_count = match &mut readers {
                Readers::Events => drain_client_usage(&fd).unwrap_or(count),
                Readers::ProcScan { dev, next } => {
                    if now >= *next {
                        *next = now + Duration::from_secs(1);
                        count_openers(Path::new("/proc"), dev, my_pid)
                    } else {
                        count
                    }
                }
            };
            shared.readers.store(new_count, Ordering::Relaxed);
            if new_count > 0 {
                // Also refreshes consumer_heartbeat_ms every frame.
                shm.shm().set_consumer(true, fmt.width, fmt.height, fmt.fps);
            } else if count > 0 {
                shm.shm().set_consumer(false, fmt.width, fmt.height, fmt.fps);
            }
            count = new_count;

            // Frame.
            let (_src, frame) = sel.next(shm.shm());
            // SAFETY: valid fd and buffer.
            let n = unsafe { libc::write(fd.as_raw_fd(), frame.as_ptr().cast(), frame.len()) };
            if n == frame.len() as isize {
                shared.set_error(None);
            } else if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::WouldBlock && e.kind() != std::io::ErrorKind::Interrupted {
                    shared.set_error(Some(format!("write: {e}")));
                }
            } else {
                shared.set_error(Some(format!("short write: {n} of {} bytes", frame.len())));
            }

            next += period;
            let now = Instant::now();
            if next <= now {
                next = now; // fell behind (suspend, slow write): don't burst
            } else {
                // Sleep in short slices so Drop is not delayed by a long frame period.
                std::thread::sleep((next - now).min(Duration::from_millis(100)));
                while Instant::now() < next && !shared.stop.load(Ordering::Relaxed) {
                    std::thread::sleep((next - Instant::now()).min(Duration::from_millis(100)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn tmp(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "nd-vcam-v4l2-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn add_video(sys: &Path, devices: &Path, n: u32, label: &str, virt: bool) {
        let class = sys.join("class/video4linux");
        std::fs::create_dir_all(&class).unwrap();
        let real = devices.join(if virt { "virtual" } else { "pci0000:00" }).join(format!("video4linux/video{n}"));
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("name"), format!("{label}\n")).unwrap();
        std::os::unix::fs::symlink(&real, class.join(format!("video{n}"))).unwrap();
    }

    #[test]
    fn state_detection() {
        let root = tmp("state");
        let (sys, modules) = (root.join("sys"), root.join("lib/modules"));
        std::fs::create_dir_all(&sys).unwrap();
        assert_eq!(state_at(&sys, &modules, "6.1.0"), LoopbackState::ModuleMissing);
        let deep = modules.join("6.1.0/kernel/drivers/media/v4l2-core");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("videodev.ko.zst"), b"").unwrap();
        assert_eq!(state_at(&sys, &modules, "6.1.0"), LoopbackState::ModuleMissing);
        std::fs::write(deep.join("v4l2loopback.ko.zst"), b"").unwrap();
        assert_eq!(state_at(&sys, &modules, "6.1.0"), LoopbackState::NotLoaded);
        assert_eq!(state_at(&sys, &modules, "6.2.0"), LoopbackState::ModuleMissing);
        assert_eq!(state_at(&sys, &modules, ""), LoopbackState::ModuleMissing);

        std::fs::create_dir_all(sys.join("module/v4l2loopback")).unwrap();
        assert_eq!(state_at(&sys, &modules, "6.1.0"), LoopbackState::NoDevice);
        let devices = root.join("devices");
        add_video(&sys, &devices, 0, "Integrated Camera", false);
        add_video(&sys, &devices, 3, "OBS Virtual Camera", true);
        assert_eq!(state_at(&sys, &modules, "6.1.0"), LoopbackState::NoDevice);
        add_video(&sys, &devices, 12, FRIENDLY_NAME, true);
        add_video(&sys, &devices, 10, FRIENDLY_NAME, true);
        assert_eq!(state_at(&sys, &modules, "6.1.0"), LoopbackState::Ready(PathBuf::from("/dev/video10")));
        assert_eq!(other_loopback_devices(&sys, 10), 2); // video3 + video12, not the PCI camera
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nv12_to_yuyv_known_values() {
        // 4x2: Y = 0..8, UV row: (U0,V0) = (100,200), (U1,V1) = (110,210).
        let nv12: Vec<u8> = vec![0, 1, 2, 3, 4, 5, 6, 7, 100, 200, 110, 210];
        assert_eq!(nv12.len(), nv12_len(4, 2));
        let mut out = vec![0u8; 4 * 2 * 2];
        nv12_to_yuyv(&nv12, 4, 2, &mut out);
        assert_eq!(out, vec![0, 100, 1, 200, 2, 110, 3, 210, 4, 100, 5, 200, 6, 110, 7, 210]);
    }

    #[test]
    fn nv12_to_yuyv_two_chroma_rows() {
        // 2x4: chroma rows apply to luma row pairs.
        let nv12: Vec<u8> = vec![10, 11, 12, 13, 14, 15, 16, 17, 50, 60, 70, 80];
        let mut out = vec![0u8; 2 * 4 * 2];
        nv12_to_yuyv(&nv12, 2, 4, &mut out);
        assert_eq!(out, vec![10, 50, 11, 60, 12, 50, 13, 60, 14, 70, 15, 80, 16, 70, 17, 80]);
    }

    #[test]
    fn nv12_to_yuyv_full_size_flat() {
        let (w, h) = (1920u32, 1080u32);
        let mut nv12 = vec![0x10u8; nv12_len(w, h)];
        nv12[(w * h) as usize..].as_chunks_mut::<2>().0.iter_mut().for_each(|c| *c = [0x40, 0xC0]);
        let mut out = vec![0u8; (w * h * 2) as usize];
        nv12_to_yuyv(&nv12, w, h, &mut out);
        assert!(out.as_chunks::<4>().0.iter().all(|c| *c == [0x10, 0x40, 0x10, 0xC0]));
    }

    #[test]
    #[should_panic(expected = "output size")]
    fn nv12_to_yuyv_wrong_output_size() {
        let mut out = vec![0u8; 3];
        nv12_to_yuyv(&[0u8; 12], 4, 2, &mut out);
    }

    #[test]
    fn output_format_validation() {
        assert!(OutputFormat::default().validate().is_ok());
        assert_eq!(OutputFormat::default().yuyv_len(), 1280 * 720 * 2);
        assert!(OutputFormat { width: 1920, height: 1080, fps: 30 }.validate().is_ok());
        assert!(OutputFormat { width: 1921, height: 1080, fps: 30 }.validate().is_err());
        assert!(OutputFormat { width: 2560, height: 1440, fps: 30 }.validate().is_err());
        assert!(OutputFormat { width: 640, height: 360, fps: 0 }.validate().is_err());
    }

    #[test]
    fn frame_selection_mirrors_dll() {
        let mem = MemShm::new();
        let shm = mem.shm();
        let (w, h) = (64u32, 36u32);
        let mut sel = FrameSelector::new(w, h);
        let mut ph = vec![0u8; (w * h * 2) as usize];
        nv12_to_yuyv(&crate::placeholder::cached(w, h), w, h, &mut ph);

        let (src, out) = sel.next(shm);
        assert_eq!(src, Source::Placeholder);
        assert_eq!(out, &ph[..]);
        assert_eq!(out[0], crate::placeholder::BG_Y);

        shm.producer_heartbeat();
        assert_eq!(sel.next(shm).0, Source::Placeholder); // heartbeat but no frame yet

        let frame: Vec<u8> = (0..nv12_len(w, h)).map(|i| (i % 251) as u8).collect();
        assert!(shm.write_frame(w, h, 1, &frame));
        let mut want = vec![0u8; (w * h * 2) as usize];
        nv12_to_yuyv(&frame, w, h, &mut want);
        let (src, out) = sel.next(shm);
        assert_eq!(src, Source::Fresh);
        assert_eq!(out, &want[..]);
        let (src, out) = sel.next(shm);
        assert_eq!(src, Source::Repeated);
        assert_eq!(out, &want[..]);

        // Wrong-size frame from the producer: placeholder.
        assert!(shm.write_frame(1280, 720, 2, &vec![1u8; nv12_len(1280, 720)]));
        let (src, out) = sel.next(shm);
        assert_eq!(src, Source::Placeholder);
        assert_eq!(out, &ph[..]);
    }

    #[test]
    fn proc_scan_counts_other_openers() {
        let root = tmp("proc");
        let dev = Path::new("/dev/video10");
        let mk = |pid: &str, fd: &str, target: &Path| {
            let d = root.join(pid).join("fd");
            std::fs::create_dir_all(&d).unwrap();
            std::os::unix::fs::symlink(target, d.join(fd)).unwrap();
        };
        mk("100", "3", dev);
        mk("100", "4", dev); // same process twice: counted once
        mk("200", "7", dev);
        mk("300", "5", Path::new("/dev/video0"));
        mk("42", "9", dev); // ourselves
        mk("self", "9", dev); // not a pid
        assert_eq!(count_openers(&root, dev, 42), 2);
        assert_eq!(count_openers(&root, dev, 1), 3);
        assert_eq!(count_openers(&root.join("missing"), dev, 1), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_files() {
        assert_eq!(
            modprobe_conf_contents(),
            "# Written by Notideck.\noptions v4l2loopback exclusive_caps=1 card_label=\"Notideck Camera\"\n"
        );
        assert!(modules_load_conf_contents().lines().any(|l| l == "v4l2loopback"));
        let root = tmp("etc");
        remove_config_files(&root).unwrap(); // nothing there: fine
        write_config_files(&root).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(MODPROBE_CONF)).unwrap(), modprobe_conf_contents());
        assert_eq!(std::fs::read_to_string(root.join(MODULES_LOAD_CONF)).unwrap(), modules_load_conf_contents());
        write_config_files(&root).unwrap(); // idempotent
        remove_config_files(&root).unwrap();
        assert!(!root.join(MODPROBE_CONF).exists());
        assert!(!root.join(MODULES_LOAD_CONF).exists());
        assert!(root.join("etc/modprobe.d").is_dir()); // the directory itself stays
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_planning() {
        assert_eq!(install_plan(&LoopbackState::ModuleMissing), None);
        assert_eq!(install_plan(&LoopbackState::NotLoaded), Some(Step::Modprobe));
        assert_eq!(install_plan(&LoopbackState::NoDevice), Some(Step::Add));
        assert_eq!(install_plan(&LoopbackState::Ready("/dev/video10".into())), None);
        assert_eq!(modprobe_args(), vec!["v4l2loopback"]);
        assert_eq!(ctl_add_args(), vec!["add", "-x", "1", "-n", "Notideck Camera"]);
        assert_eq!(ctl_delete_args(Path::new("/dev/video10")), vec!["delete", "/dev/video10"]);
        assert!(may_unload_module(0));
        assert!(!may_unload_module(1));
        assert_eq!(device_number(Path::new("/dev/video10")), Some(10));
        assert_eq!(device_number(Path::new("/dev/media0")), None);
    }

    #[test]
    fn program_lookup() {
        let dir = tmp("path");
        std::fs::write(dir.join("v4l2loopback-ctl"), b"#!/bin/sh\n").unwrap();
        let path = std::env::join_paths([dir.clone()]).unwrap();
        assert_eq!(find_program("v4l2loopback-ctl", Some(&path)), Some(dir.join("v4l2loopback-ctl")));
        assert_eq!(find_program("nd-surely-not-a-program", Some(&path)), None);
        assert!(find_program("sh", None).is_some()); // system dirs searched without PATH
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn ioctl_abi_matches_videodev2_h() {
        use std::mem::size_of;
        assert_eq!(size_of::<sys::Capability>(), 104);
        assert_eq!(size_of::<sys::Format>(), 208);
        assert_eq!(size_of::<sys::StreamParm>(), 204);
        assert_eq!(size_of::<sys::EventSubscription>(), 32);
        assert_eq!(size_of::<sys::Event>(), 136);
        assert_eq!(size_of::<sys::LoopbackConfig>(), 256);
        assert_eq!(sys::VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(sys::VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(sys::VIDIOC_S_PARM, 0xc0cc_5616);
        assert_eq!(sys::VIDIOC_SUBSCRIBE_EVENT, 0x4020_565a);
        assert_eq!(sys::VIDIOC_DQEVENT, 0x8088_5659);
        assert_eq!(sys::V4L2_EVENT_PRI_CLIENT_USAGE, 0x10E0_0001);
    }

    /// Live test against a real device. Needs the module loaded with our device, e.g.
    /// `sudo modprobe v4l2loopback exclusive_caps=1 card_label="Notideck Camera"`, then
    /// `cargo test -p nd-vcam live_sink -- --ignored --nocapture` and, while it runs, open the
    /// camera in a reader (`ffplay /dev/videoN`, `mpv av://v4l2:/dev/videoN`, a browser) to see
    /// the placeholder and the reader count change.
    #[test]
    #[ignore = "needs a loaded v4l2loopback device labelled Notideck Camera"]
    fn live_sink() {
        let LoopbackState::Ready(dev) = loopback_state() else {
            panic!("no device: {:?}", loopback_state());
        };
        let mem = MemShm::new();
        let sink = V4l2Sink::start(&dev, mem.clone(), OutputFormat::default()).expect("start");
        for i in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            println!(
                "{i:2}: {} readers={} error={:?} wanted={:?}",
                dev.display(),
                sink.readers(),
                sink.error(),
                mem.shm().wanted()
            );
        }
        assert_eq!(sink.error(), None);
        drop(sink);
        assert_eq!(mem.shm().wanted(), None);
    }
}
