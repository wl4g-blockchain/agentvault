use super::*;

#[tokio::test]
async fn management_health_contract_is_stable() {
    assert_eq!(liveness().await, "ok");
    assert_eq!(readiness().await.status(), StatusCode::OK);
}
