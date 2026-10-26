// SPDX-License-Identifier: GPL-3.0-or-later
//! Logging for a DLL that lives inside the Frame Server: no console. Appends to
//! `%ProgramData%\Notideck\vcam.log` (capped) and `OutputDebugStringW`. Never fails.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::core::HSTRING;
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;

const CAP_BYTES: u64 = 1024 * 1024;

struct Sink {
    file: Option<File>,
    written: u64,
}

fn sink() -> &'static Mutex<Sink> {
    static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(open_sink()))
}

fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("ProgramData")?;
    Some(PathBuf::from(base).join("Notideck").join("vcam.log"))
}

fn open_sink() -> Sink {
    let Some(path) = log_path() else { return Sink { file: None, written: 0 } };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut opts = OpenOptions::new();
    opts.create(true);
    if size >= CAP_BYTES {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    let written = if size >= CAP_BYTES { 0 } else { size };
    Sink { file: opts.open(&path).ok(), written }
}

pub fn log(msg: &str) {
    let ms = nd_vcam::shm::now_ms();
    let line = format!("{ms} pid={} {msg}\n", std::process::id());
    unsafe { OutputDebugStringW(&HSTRING::from(format!("notideck_vcam: {msg}"))) };
    let Ok(mut s) = sink().lock() else { return };
    if s.written >= CAP_BYTES {
        // Restart the file once the cap is hit.
        *s = open_sink_truncated();
    }
    let n = line.len() as u64;
    if let Some(f) = s.file.as_mut() {
        if f.write_all(line.as_bytes()).is_ok() {
            s.written += n;
        }
    }
}

fn open_sink_truncated() -> Sink {
    let Some(path) = log_path() else { return Sink { file: None, written: 0 } };
    Sink { file: OpenOptions::new().create(true).write(true).truncate(true).open(path).ok(), written: 0 }
}

#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => { $crate::log::log(&format!($($arg)*)) };
}
