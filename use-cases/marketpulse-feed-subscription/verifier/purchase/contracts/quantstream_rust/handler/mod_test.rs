#[test]
fn handler_module_exports_ingestion_health() {
    let _ = super::ingestion::IngestionHandler::health;
}
