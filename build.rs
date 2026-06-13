fn main() {
    println!("cargo:rustc-check-cfg=cfg(miri)");

    #[cfg(feature = "protobuf")]
    {
        println!("cargo:rerun-if-changed=proto/libdictenstein.proto");
        prost_build::Config::new()
            .compile_protos(&["proto/libdictenstein.proto"], &["proto/"])
            .expect("Failed to compile protobuf definitions");
    }
}
