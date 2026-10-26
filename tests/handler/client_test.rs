use super::*;

#[tokio::test]
async fn disabled_client_cannot_authenticate() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let service = PayClientHandler::new(fixture.ports.clients);
    assert_eq!(
        service.authenticate(&fixture.client.auth_principal).await.unwrap(),
        fixture.client
    );
    service.set_enabled(&fixture.client.id, false, 11).await.unwrap();
    assert!(matches!(
        service.authenticate(&fixture.client.auth_principal).await,
        Err(Error::Forbidden)
    ));
}
