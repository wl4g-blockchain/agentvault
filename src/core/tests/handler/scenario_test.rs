use super::*;

#[tokio::test]
async fn scenario_access_rechecks_client_lifecycle() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let service = ScenarioHandler::new(fixture.ports.clients.clone(), fixture.ports.scenarios);
    let access = service
        .authorize(&fixture.client.auth_principal, &fixture.scenario.id)
        .await
        .unwrap();
    assert_eq!(access.scenario.id, fixture.scenario.id);
    fixture
        .ports
        .clients
        .set_enabled(&fixture.client.id, false, 11)
        .await
        .unwrap();
    assert!(matches!(
        service
            .authorize(&fixture.client.auth_principal, &fixture.scenario.id)
            .await,
        Err(Error::Forbidden)
    ));
}
