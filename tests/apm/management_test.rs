use super::*;

struct FixedReadinessProbe {
    available: bool,
}

struct SlowReadinessProbe;

#[async_trait]
impl ReadinessProbe for FixedReadinessProbe {
    fn name(&self) -> &'static str {
        "test dependency"
    }

    async fn check(&self) -> anyhow::Result<()> {
        if self.available {
            Ok(())
        } else {
            anyhow::bail!("unavailable")
        }
    }
}

#[async_trait]
impl ReadinessProbe for SlowReadinessProbe {
    fn name(&self) -> &'static str {
        "slow dependency"
    }

    async fn check(&self) -> anyhow::Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(())
    }
}

#[tokio::test]
async fn management_health_contract_is_stable() {
    assert_eq!(ManagementEndpoints::liveness(), "ok");
    let ready = ManagementState {
        metrics: AgentVaultMetrics::default(),
        readiness_probes: vec![std::sync::Arc::new(FixedReadinessProbe { available: true })],
        probe_timeout: std::time::Duration::from_secs(1),
    };
    assert_eq!(
        ManagementEndpoints::readiness(State(ready)).await.status(),
        StatusCode::OK
    );

    let unavailable = ManagementState {
        metrics: AgentVaultMetrics::default(),
        readiness_probes: vec![std::sync::Arc::new(FixedReadinessProbe { available: false })],
        probe_timeout: std::time::Duration::from_secs(1),
    };
    assert_eq!(
        ManagementEndpoints::readiness(State(unavailable)).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let timed_out = ManagementState {
        metrics: AgentVaultMetrics::default(),
        readiness_probes: vec![std::sync::Arc::new(SlowReadinessProbe)],
        probe_timeout: std::time::Duration::from_millis(1),
    };
    assert_eq!(
        ManagementEndpoints::readiness(State(timed_out)).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn management_router_exposes_health_without_business_routes() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt as _;

    let router = crate::controller::ApiController::management_router(
        &ManagementConfig::default(),
        AgentVaultMetrics::default(),
        vec![],
    );
    for (method, path, expected) in [
        ("GET", "/healthz", StatusCode::OK),
        ("POST", "/admin/prices", StatusCode::NOT_FOUND),
        ("GET", "/payments/intent-1", StatusCode::NOT_FOUND),
    ] {
        let request = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected, "{method} {path}");
    }
}
