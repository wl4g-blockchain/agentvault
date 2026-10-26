use super::*;

#[tokio::test]
async fn wallet_queries_expose_metadata_without_private_key_material() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let service = WalletHandler::new(fixture.ports.wallets);
    let wallet = service.get(&fixture.wallet.id).await.unwrap().unwrap();
    assert_eq!(wallet, fixture.wallet);
    let value = serde_json::to_value(wallet).unwrap();
    assert!(value.get("private_key").is_none());
    assert!(value.get("key_ciphertext").is_none());
}
