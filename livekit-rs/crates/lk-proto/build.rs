//! Generate `lk-proto` from the vendored `livekit/protocol` sources.
//!
//! All 43 `.proto` files, not the 10 that `livekit-protocol` 0.7.13 ships.
//!
//! `protox` compiles them, not `protoc`: it is a pure-Rust compiler, so no CI
//! job, contributor machine or release image needs a protobuf toolchain
//! installed, and the compiler version cannot drift between them. The
//! descriptor set it produces is what `prost-build`, `pbjson-build` and the
//! service generators all consume, so all four see exactly the same input.
//!
//! JSON goes through `pbjson`, never `prost`'s own JSON support: the signal
//! protocol's JSON frames must follow protojson semantics, including
//! discarding unknown fields, and that is what `pbjson` implements. A
//! `cargo deny` ban keeps the alternative out of the tree.

// A build script is not a library; the workspace lints that police one do not
// apply.
#![allow(unreachable_pub, missing_docs)]

#[path = "build/descriptor.rs"]
mod descriptor;
#[path = "build/services.rs"]
mod services;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let protobufs = manifest_dir.join("protobufs");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);

    let files = collect_protos(&protobufs)?;
    anyhow::ensure!(!files.is_empty(), "no .proto files under {protobufs:?}");

    // Rebuild whenever any vendored proto changes, and whenever the pin does.
    println!("cargo:rerun-if-changed={}", protobufs.display());
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
    }

    let mut compiler =
        protox::Compiler::new([&protobufs]).context("starting the protox compiler")?;
    compiler
        .include_source_info(true)
        .include_imports(true)
        .open_files(&files)
        .context("compiling the vendored protos with protox")?;

    let descriptor_set = compiler.file_descriptor_set();
    // Not `descriptor_set.encode_to_vec()`. `prost_types::MethodOptions` has no
    // storage for extensions, so re-encoding the prost view of the descriptor
    // silently drops the `psrpc.options` annotation every rpc method carries,
    // and the psrpc generator would see a service with no topics on it.
    // `encode_file_descriptor_set` encodes protox's own reflective view, which
    // keeps them.
    let descriptor_bytes = compiler.encode_file_descriptor_set();

    // The descriptor set is written out so that `lk-service` can serve
    // reflection and the interop harness can diff it against the Go server's.
    let descriptor_path = out_dir.join("livekit_descriptor.bin");
    std::fs::write(&descriptor_path, &descriptor_bytes)?;

    let mut config = prost_build::Config::new();
    config
        // pbjson-build emits `HashMap`-shaped deserialisers, so the map type
        // here is not a free choice.
        .bytes([
            ".livekit.DataPacket",
            ".livekit.UserPacket",
            ".livekit.DataStream",
        ])
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types")
        .compile_fds(descriptor_set)
        .context("prost-build codegen")?;

    pbjson_build::Builder::new()
        .register_descriptors(&descriptor_bytes)?
        // protojson's DiscardUnknown. Without this a newer Go node sending a
        // field this build has never heard of would fail the whole frame, and
        // the signal protocol is explicitly forward-compatible. This is why
        // JSON goes through pbjson at all.
        .ignore_unknown_fields()
        // Likewise for enum values: an unknown variant decodes to the proto3
        // default rather than failing, which is what protojson does and what
        // lets a Go node add a codec or a disconnect reason without breaking
        // every Rust node in the cluster.
        .ignore_unknown_enum_variants()
        .build(&[".livekit", ".rpc", ".psrpc", ".logger"])
        .context("pbjson-build codegen")?;

    let lite = descriptor::FileDescriptorSetLite::decode_set(&descriptor_bytes)
        .context("decoding the descriptor set for service generation")?;
    std::fs::write(out_dir.join("twirp.rs"), services::generate_twirp(&lite))?;
    std::fs::write(
        out_dir.join("psrpc_services.rs"),
        services::generate_psrpc(&lite),
    )?;

    Ok(())
}

/// Every `.proto` under `root`, as paths relative to it, sorted.
///
/// Sorted because the descriptor set's file order ends up in the generated
/// code, and a build that depends on directory iteration order is not
/// reproducible.
fn collect_protos(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "proto") {
                found.push(path.strip_prefix(root)?.to_path_buf());
            }
        }
    }
    found.sort();
    Ok(found)
}
