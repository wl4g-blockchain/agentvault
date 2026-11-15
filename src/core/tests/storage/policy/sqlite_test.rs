use crate::storage::schema_tests::RepositoryFixture;
use crate::storage::sqlite::tests::storage;
use std::sync::Arc;

#[tokio::test]
async fn preserves_policy_contract_through_composed_repository() {
    let (_directory, storage) = storage().await;
    RepositoryFixture::create(Arc::new(storage))
        .await
        .policy_contract()
        .await;
}
