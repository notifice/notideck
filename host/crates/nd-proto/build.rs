// SPDX-License-Identifier: GPL-3.0-or-later
use std::path::PathBuf;

fn main() {
    let proto_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../proto");
    let file = proto_root.join("notideck/v1/control.proto");
    println!("cargo:rerun-if-changed={}", file.display());

    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    std::env::set_var("PROTOC", protoc);

    prost_build::Config::new().compile_protos(&[file], &[proto_root]).expect("compile control.proto");
}
