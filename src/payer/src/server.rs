use agentvault_common::{
    apm::AgentVaultMetrics, apm::ApmComponent, apm::ReadinessProbe, config::AppConfig, util::http::HttpApiServer,
    util::principal_context::PrincipalContext, util::runtime::RuntimeComponent, util::runtime::RuntimeResource,
    util::runtime::ServerSupervisor,
};
use axum::Router;
use std::sync::Arc;
/// HTTP service entry point for the payer domain.
pub struct PayerServer;
impl PayerServer {
    ///
    /// # Errors
    /// Returns a configuration, listener, component or graceful-shutdown failure.
    pub async fn run(
        config: AppConfig,
        routes: Router,
        metrics: AgentVaultMetrics,
        probes: Vec<Arc<dyn ReadinessProbe>>,
        components: Vec<Box<dyn RuntimeComponent>>,
        resources: Vec<Box<dyn RuntimeResource>>,
    ) -> anyhow::Result<()> {
        let mut runtime = ServerSupervisor::new(config.server.shutdown_timeout);
        let telemetry = ApmComponent::start(&config, metrics.clone(), probes, &mut runtime)?;
        if config.server.enabled {
            runtime.add(HttpApiServer::new(
                config.server.clone(),
                PrincipalContext::from_config(&config.server.authguard)?,
                routes,
                metrics,
            ));
        }
        for component in components {
            runtime.add_boxed(component);
        }
        let result = runtime.run_with_resources(resources).await;
        telemetry.shutdown();
        result
    }
}
