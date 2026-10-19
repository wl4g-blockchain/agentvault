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
    assert_eq!(liveness().await, "ok");
    let ready = ManagementState {
        metrics: AgentVaultMetrics::default(),
        readiness_probes: vec![std::sync::Arc::new(FixedReadinessProbe { available: true })],
        probe_timeout: std::time::Duration::from_secs(1),
    };
    assert_eq!(readiness(State(ready)).await.status(), StatusCode::OK);

    let unavailable = ManagementState {
        metrics: AgentVaultMetrics::default(),
        readiness_probes: vec![std::sync::Arc::new(FixedReadinessProbe { available: false })],
        probe_timeout: std::time::Duration::from_secs(1),
    };
    assert_eq!(
        readiness(State(unavailable)).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let timed_out = ManagementState {
        metrics: AgentVaultMetrics::default(),
        readiness_probes: vec![std::sync::Arc::new(SlowReadinessProbe)],
        probe_timeout: std::time::Duration::from_millis(1),
    };
    assert_eq!(
        readiness(State(timed_out)).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
