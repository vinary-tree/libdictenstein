fn main() {
    println!("cargo:rustc-check-cfg=cfg(miri)");

    #[cfg(feature = "protobuf")]
    {
        println!("cargo:rerun-if-changed=proto/libdictenstein.proto");
        println!("cargo:rerun-if-env-changed=PROTOC");

        let mut config = prost_build::Config::new();

        // `prost-build` locates `protoc` on the host and vendors none of its own,
        // so a bare `cargo build --features protobuf` succeeds or fails depending
        // on whether the machine happens to have protobuf-compiler installed.
        // Prefer an explicit `PROTOC` override when the caller sets one (needed on
        // any target `protoc-bin-vendored` does not ship a binary for), otherwise
        // use the vendored binary so the build is hermetic.
        match std::env::var_os("PROTOC") {
            Some(_) => {}
            None => match protoc_bin_vendored::protoc_bin_path() {
                Ok(path) => {
                    config.protoc_executable(path);
                }
                // Not fatal: fall through to prost-build's own PATH lookup, which
                // still works on hosts that do have protoc installed.
                Err(err) => {
                    println!(
                        "cargo:warning=protoc-bin-vendored provides no binary for this target \
                         ({err}); falling back to `protoc` on PATH. Set PROTOC to override."
                    );
                }
            },
        }

        config
            .compile_protos(&["proto/libdictenstein.proto"], &["proto/"])
            .expect("Failed to compile protobuf definitions");
    }
}
