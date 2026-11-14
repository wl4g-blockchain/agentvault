use crate::storage::base_sqlite::tests::storage;
use crate::storage::schema_tests::RepositoryFixture;
use std::sync::Arc;

#[tokio::test]
async fn preserves_signing_contract_through_composed_repository() {
    let (_directory, storage) = storage().await;
    RepositoryFixture::create(Arc::new(storage))
        .await
        .signing_contract()
        .await;
}
