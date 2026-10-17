fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    println!("cargo:rerun-if-changed=src/adapters/proto/transport.proto");
    println!("cargo:rerun-if-changed=src/adapters/proto/authguard_access.proto");
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&["src/adapters/proto/transport.proto"], &["src/adapters/proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["src/adapters/proto/authguard_access.proto"], &["src/adapters/proto"])?;
    Ok(())
}
