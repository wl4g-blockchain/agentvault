use super::*;

#[tokio::test]
async fn stale_price_revision_cannot_restore_disabled_pricing() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let service = PriceHandler::new(fixture.ports.offers);
    let mut price = fixture.offer;
    price.enabled = false;
    let revision = price.updated_at;
    price.updated_at += 1;
    service.update(&price, revision).await.unwrap();
    assert!(service.update(&price, revision).await.is_err());
    assert_eq!(service.get(&price.id).await.unwrap(), Some(price));
}
