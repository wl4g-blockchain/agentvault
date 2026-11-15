use super::*;

#[tokio::test]
async fn intent_queries_do_not_cross_scenario_ownership() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let intent = fixture.challenge().await;
    let service = IntentHandler::new(fixture.ports.payments);
    assert!(service
        .get_for_scenario("another-scenario", &intent.id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        service
            .get_for_scenario(&fixture.scenario.id, &intent.id)
            .await
            .unwrap(),
        Some(intent)
    );
}
