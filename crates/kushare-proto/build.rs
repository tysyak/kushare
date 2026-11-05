use std::env;
use std::path::PathBuf;

const PROTO_FILES: &[&str] = &[
    "proto/offline_wire_formats.proto",
    "proto/wire_format.proto",
    "proto/sharing_enums.proto",
    "proto/ukey.proto",
    "proto/securegcm.proto",
    "proto/securemessage.proto",
    "proto/device_to_device_messages.proto",
    // Protocolo propio del enlace persistente (no es de Quick Share).
    "proto/kushare_link.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for file in PROTO_FILES {
        println!("cargo:rerun-if-changed={file}");
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    // protox is a pure-Rust .proto parser: no dependency on a system `protoc`
    // binary (which is a C++ toolchain), keeping the build chain pure Rust.
    let file_descriptor_set = protox::compile(PROTO_FILES, ["proto/"])?;

    // `include_file` makes prost-build emit a single mod.rs that nests every
    // generated package as real Rust modules matching its dotted protobuf
    // package name (e.g. `location::nearby::connections`). This is required
    // for cross-package references (e.g. wire_format.proto referencing
    // sharing_enums.proto's `location.nearby.proto.sharing.OsType`) to
    // resolve their generated `super::super::...` paths correctly.
    prost_build::Config::new()
        .out_dir(&out_dir)
        .include_file("mod.rs")
        .compile_fds(file_descriptor_set)?;

    Ok(())
}
