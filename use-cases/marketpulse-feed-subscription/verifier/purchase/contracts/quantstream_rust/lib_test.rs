#[test]
fn library_exports_the_four_business_layers() {
    let _ = std::mem::size_of::<crate::route::ingestion::IngestionRouter>();
    let _ = crate::handler::ingestion::IngestionHandler::health;
    let _ = crate::job::ingestion_job::unix_time();
    let _ = std::mem::size_of::<crate::agent::mcp::ToolResult>();
}
