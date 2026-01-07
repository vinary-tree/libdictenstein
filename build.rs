fn main() {
    #[cfg(feature = "protobuf")]
    {
        prost_build::Config::new()
            .compile_protos(&["proto/libdictenstein.proto"], &["proto/"])
            .expect("Failed to compile protobuf definitions");
    }
}
