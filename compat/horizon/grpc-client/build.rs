fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .compile_protos(&["proto/horizon/store/v1/store_context.proto"], &["proto"])?;
    Ok(())
}
