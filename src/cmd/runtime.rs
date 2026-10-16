use anyhow::Context as _;
use axum::Router;

use crate::{
    apm::{init_telemetry, AgentVaultMetrics, TelemetryConfig},
    config::AppConfig,
    controller::http::{HttpApiServer, PrincipalContext},
    controller::ManagementServer,
};

use super::{RuntimeComponent, RuntimeResource, ServerSupervisor};

/// Process runtime assembled from enterprise payment components.
///
/// The core owns lifecycle and observability only. HTTP handlers, SQL-backed
/// payment services, and messaging consumers are injected at the composition
/// root, which prevents the daemon from silently selecting another storage
/// implementation.
pub struct AgentVaultServer {
    config: AppConfig,
    api_router: Option<Router>,
    components: Vec<Box<dyn RuntimeComponent>>,
    resources: Vec<Box<dyn RuntimeResource>>,
}

impl AgentVaultServer {
    #[must_use]
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            api_router: None,
            components: Vec::new(),
            resources: Vec::new(),
        }
    }

    /// Builds the production payment runtime from validated operator
    /// configuration. No filesystem authority or implicit adapter fallback is
    /// selected when a requested backend or transport is unavailable.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, secrets, SQL, protocols,
    /// providers, signing, RPC, or transport construction fails.
    pub async fn assemble(config: AppConfig) -> anyhow::Result<Self> {
        config.validate_for_serve().context("validate service configuration")?;
        let mut server = Self::new(config.clone());
        if config.agentvault.enabled() {
            let payment = super::payment_runtime::assemble(&config).await?;
            server = server.with_api_router(payment.router);
            if let Some(grpc) = payment.grpc {
                server = server.with_component(grpc);
            }
            if let Some(local) = payment.local {
                server = server.with_component(local);
            }
            if let Some(mqtt) = payment.mqtt {
                server = server.with_component(mqtt);
            }
            server = server.with_component(payment.cache_health);
            server = server.with_component(payment.reconciliation);
            server = server.with_component(payment.approval_expiration);
            server = server.with_resource(payment.resources);
        } else if config.server.enabled
            || config.agentvault.transport.grpc.enabled
            || config.agentvault.transport.local.enabled
            || config.agentvault.transport.mqtt.enabled
        {
            anyhow::bail!("an AgentVault data-plane transport is enabled while payer and payee are disabled");
        }
        Ok(server)
    }

    /// Installs the payment API router assembled from the shared handlers.
    #[must_use]
    pub fn with_api_router(mut self, router: Router) -> Self {
        self.api_router = Some(router);
        self
    }

    /// Installs a transport or background component that shares the same
    /// handler/storage composition root.
    #[must_use]
    pub fn with_component<C>(mut self, component: C) -> Self
    where
        C: RuntimeComponent + 'static,
    {
        self.components.push(Box::new(component));
        self
    }

    /// Installs a provider registry whose resources close after every runtime
    /// component has stopped using them.
    #[must_use]
    pub fn with_resource<R>(mut self, resource: R) -> Self
    where
        R: RuntimeResource + 'static,
    {
        self.resources.push(Box::new(resource));
        self
    }

    /// Opens configured runtime components and serves until process shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error for configuration, telemetry, secret, listener, or
    /// component failures.
    pub async fn run(self) -> anyhow::Result<()> {
        self.config
            .validate_for_serve()
            .context("validate service configuration")?;
        let telemetry = init_telemetry(&TelemetryConfig::from_settings(
            &self.config.server.service_name,
            &self.config.logging,
            &self.config.mgmt.otel,
        ))
        .context("configure AgentVault telemetry")?;
        let metrics = AgentVaultMetrics::default();
        let mut supervisor = ServerSupervisor::new(self.config.server.shutdown_timeout);

        if self.config.server.enabled {
            let router = self
                .api_router
                .context("payment HTTP API is enabled but no router was installed")?;
            let principal_context = PrincipalContext::from_config(&self.config.server.authguard)?;
            supervisor.add(HttpApiServer::new(
                self.config.server.clone(),
                principal_context,
                router,
                metrics.clone(),
            ));
        }
        if self.config.mgmt.enabled {
            supervisor.add(ManagementServer::new(self.config.mgmt.clone(), metrics));
        }
        for component in self.components {
            supervisor.add_boxed(component);
        }

        tracing::info!(
            service.name = %self.config.server.service_name,
            api.enabled = self.config.server.enabled,
            management.enabled = self.config.mgmt.enabled,
            "AgentVault runtime started"
        );
        let runtime_result = supervisor.run().await;
        let mut resource_error = None;
        for resource in self.resources.into_iter().rev() {
            let name = resource.name();
            if let Err(error) = resource.close().await.with_context(|| format!("close {name}")) {
                tracing::error!(resource = name, %error, "runtime resource shutdown failed");
                if resource_error.is_none() {
                    resource_error = Some(error);
                }
            }
        }
        telemetry.shutdown();
        runtime_result?;
        resource_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/runtime_test.rs"]
mod tests;
