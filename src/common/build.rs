fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    println!("cargo:rerun-if-changed=../sdk/proto/authguard_access.proto");
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["../sdk/proto/authguard_access.proto"], &["../sdk/proto"])?;
    Ok(())
}
