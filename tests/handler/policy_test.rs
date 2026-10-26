use super::*;

#[tokio::test]
async fn stale_policy_revision_cannot_override_new_configuration() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let service = PolicyHandler::new(fixture.ports.policies);
    let mut policy = fixture.policy;
    policy.enabled = false;
    let revision = policy.updated_at;
    policy.updated_at += 1;
    service.update(&policy, revision).await.unwrap();
    assert!(service.update(&policy, revision).await.is_err());
    assert_eq!(service.get(&policy.id).await.unwrap(), Some(policy));
}
