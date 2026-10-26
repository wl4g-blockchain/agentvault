use crate::storage::postgres::tests::storage;
use crate::storage::schema_tests::RepositoryFixture;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_URL pointing at a dedicated PostgreSQL database"]
async fn preserves_event_contract_through_composed_repository() {
    let storage = storage().await;
    RepositoryFixture::create(Arc::new(storage))
        .await
        .event_contract()
        .await;
}
