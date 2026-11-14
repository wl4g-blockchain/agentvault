fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    println!("cargo:rerun-if-changed=src/sdk/proto/transport.proto");
    println!("cargo:rerun-if-changed=src/sdk/proto/authguard_access.proto");
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&["src/sdk/proto/transport.proto"], &["src/sdk/proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["src/sdk/proto/authguard_access.proto"], &["src/sdk/proto"])?;
    Ok(())
}
