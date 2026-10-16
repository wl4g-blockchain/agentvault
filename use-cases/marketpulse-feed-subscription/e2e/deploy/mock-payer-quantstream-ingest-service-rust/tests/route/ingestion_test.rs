use super::IngestionRouter;

#[test]
fn route_layer_exports_the_standard_ingestion_router() {
    assert!(std::any::type_name::<IngestionRouter>().ends_with("IngestionRouter"));
}
