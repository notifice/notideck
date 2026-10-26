// SPDX-License-Identifier: GPL-3.0-or-later
//! Embeds the Windows icon, version info and manifest (Windows targets only).
//! Linux builds never touch windres.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/notideck.rc");
    println!("cargo:rerun-if-changed=res/notideck.manifest");
    println!("cargo:rerun-if-changed=res/notideck.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut nums: Vec<u32> = ver
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(4)
        .map(|s| s.parse().unwrap_or(0))
        .collect();
    nums.resize(4, 0);
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let header = format!(
        "#define ND_VER_COMMA {},{},{},{}\n#define ND_VER_STR \"{}\\0\"\n",
        nums[0], nums[1], nums[2], nums[3], ver
    );
    std::fs::write(out.join("version.h"), header).unwrap();
    // windres resolves `#include "version.h"` via OUT_DIR (embed-resource adds it as an include path).
    let rc = manifest_dir.join("res/notideck.rc");
    let macros: [&str; 0] = [];
    embed_resource::compile(&rc, macros).manifest_required().unwrap();
}
