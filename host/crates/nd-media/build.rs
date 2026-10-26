// SPDX-License-Identifier: GPL-3.0-or-later
// The OpenH264 C++ build (tests, optional `openh264` feature) links the shared libstdc++ on
// windows-gnu. So that `cargo test --target x86_64-pc-windows-gnu` executables can start (e.g. via
// WSL interop), copy the mingw runtime DLLs next to the test binaries in `target/<triple>/<profile>/deps`.
use std::path::{Path, PathBuf};
use std::process::Command;

fn find(name: &str) -> Option<PathBuf> {
    let out = Command::new("x86_64-w64-mingw32-g++").arg(format!("-print-file-name={name}")).output().ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    p.is_absolute().then_some(p).filter(|p| p.exists())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    // OUT_DIR = <target>/<triple>/<profile>/build/<pkg>-<hash>/out
    let Some(deps) = Path::new(&out_dir).ancestors().nth(3).map(|p| p.join("deps")) else {
        return;
    };
    for dll in ["libstdc++-6.dll", "libgcc_s_seh-1.dll"] {
        if let Some(src) = find(dll) {
            let _ = std::fs::copy(src, deps.join(dll));
        }
    }
}
