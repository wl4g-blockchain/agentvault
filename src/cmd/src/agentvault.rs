use crate::{apm::AgentVaultMetrics, apm::ReadinessProbe, AppConfig};
use agentvault_common::{util::runtime::RuntimeComponent, util::runtime::RuntimeResource};
use anyhow::Context as _;
use axum::Router;
use std::sync::Arc;
pub struct AgentVaultServer {
    config: AppConfig,
    api_router: Option<Router>,
    components: Vec<Box<dyn RuntimeComponent>>,
    resources: Vec<Box<dyn RuntimeResource>>,
    readiness_probes: Vec<Arc<dyn ReadinessProbe>>,
    metrics: AgentVaultMetrics,
}
impl AgentVaultServer {
    #[must_use]
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            api_router: None,
            components: vec![],
            resources: vec![],
            readiness_probes: vec![],
            metrics: AgentVaultMetrics::default(),
        }
    }
    ///
    /// # Errors
    /// Returns a configuration, secret, storage or payment-component initialization failure.
    pub async fn assemble(config: AppConfig) -> anyhow::Result<Self> {
        config.validate_for_serve().context("validate service configuration")?;
        let mut server = Self::new(config.clone());
        if config.agentvault.enabled() {
            let payment =
                super::payment_runtime::PaymentRuntimeFactory::assemble(&config, server.metrics.clone()).await?;
            server = server.with_readiness_probe(payment.storage_readiness);
            server = server.with_api_router(payment.router);
            server = server.with_component(payment.cache_health);
            if let Some(reconciliation) = payment.reconciliation {
                server = server.with_component(reconciliation);
            }
            if let Some(wallet_balance_sync) = payment.wallet_balance_sync {
                server = server.with_component(wallet_balance_sync);
            }
            if let Some(approval) = payment.approval_expiration {
                server = server.with_component(approval);
            }
            server = server.with_resource(payment.resources);
        } else if config.server.enabled {
            anyhow::bail!("an AgentVault data-plane transport is enabled while payer and payee are disabled");
        }
        Ok(server)
    }

    #[must_use]
    pub fn with_api_router(mut self, router: Router) -> Self {
        self.api_router = Some(router);
        self
    }
    #[must_use]
    pub fn with_component<C: RuntimeComponent + 'static>(mut self, component: C) -> Self {
        self.components.push(Box::new(component));
        self
    }
    #[must_use]
    pub fn with_resource<C: RuntimeResource + 'static>(mut self, resource: C) -> Self {
        self.resources.push(Box::new(resource));
        self
    }
    fn with_readiness_probe(mut self, probe: Arc<dyn ReadinessProbe>) -> Self {
        self.readiness_probes.push(probe);
        self
    }
    ///
    /// # Errors
    /// Returns a configuration, listener, component or graceful-shutdown failure.
    pub async fn run(self) -> anyhow::Result<()> {
        self.config.validate_for_serve()?;
        let routes = self.api_router.unwrap_or_default();
        if self.config.agentvault.payer.enabled {
            agentvault_payer::server::PayerServer::run(
                self.config,
                routes,
                self.metrics,
                self.readiness_probes,
                self.components,
                self.resources,
            )
            .await
        } else {
            agentvault_payee::server::PayeeServer::run(
                self.config,
                routes,
                self.metrics,
                self.readiness_probes,
                self.components,
                self.resources,
            )
            .await
        }
    }
}

#[cfg(test)]
#[path = "../tests/agentvault_test.rs"]
mod tests;
