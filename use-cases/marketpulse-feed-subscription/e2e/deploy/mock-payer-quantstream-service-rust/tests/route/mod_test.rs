#[test]
fn route_module_exports_the_ingest_router() {
    assert!(std::any::type_name::<super::ingestion::IngestionRouter>().ends_with("IngestionRouter"));
}
