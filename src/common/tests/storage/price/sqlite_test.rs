use crate::storage::base_sqlite::tests::storage;
use crate::storage::schema_tests::RepositoryFixture;
use std::sync::Arc;

#[tokio::test]
async fn preserves_price_contract_through_composed_repository() {
    let (_directory, storage) = storage().await;
    RepositoryFixture::create(Arc::new(storage))
        .await
        .price_contract()
        .await;
}

#[tokio::test]

async fn deletion_is_revision_guarded_and_preserves_payment_history() {
    let (_directory, storage) = storage().await;
    RepositoryFixture::create(Arc::new(storage))
        .await
        .price_delete_contract()
        .await;
}
