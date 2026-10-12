use std::sync::Arc;

use anyhow::Context as _;

use crate::{
    api::HttpApiServer,
    config::AppConfig,
    history::SignHistoryStore,
    management::ManagementServer,
    observability::{init_telemetry, AgentVaultMetrics, TelemetryConfig},
    secret::read_private_text,
    service::{PaymentSignService, WalletService},
    transport::{LocalSignServer, MqttSignServer},
    EncryptedFileStore,
};

use super::ServerSupervisor;

const MAX_API_TOKEN_FILE_BYTES: usize = 16 * 1024;

pub struct AgentVaultServer {
    config: AppConfig,
    store: Arc<EncryptedFileStore>,
}

impl AgentVaultServer {
    #[must_use]
    pub fn new(config: AppConfig, store: Arc<EncryptedFileStore>) -> Self {
        Self { config, store }
    }

    /// Opens runtime components and serves until process shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error for telemetry, history, secret, listener, or component failures.
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
        let history = Arc::new(
            SignHistoryStore::open(self.config.history_path(), self.config.security.max_history_entries)
                .context("open signing history")?,
        );
        let sign = Arc::new(PaymentSignService::new(
            Arc::clone(&self.store),
            history,
            metrics.clone(),
            self.config.security.clone(),
        ));
        let wallet = Arc::new(WalletService::new(Arc::clone(&self.store), metrics.clone()));
        let mut supervisor = ServerSupervisor::new(self.config.server.shutdown_timeout);

        if self.config.server.enabled {
            let token = load_api_token(&self.config.server.api_token_file)?;
            supervisor.add(HttpApiServer::new(
                self.config.server.clone(),
                token,
                wallet,
                Arc::clone(&sign),
                metrics.clone(),
            ));
        }
        if self.config.mgmt.enabled {
            supervisor.add(ManagementServer::new(
                self.config.mgmt.clone(),
                metrics.clone(),
                Arc::clone(&sign),
            ));
        }
        if self.config.signers.local.enabled {
            supervisor.add(LocalSignServer::new(
                self.config.signers.local.clone(),
                Arc::clone(&sign),
            ));
        }
        if self.config.signers.mqtt.enabled {
            supervisor.add(MqttSignServer::new(self.config.signers.mqtt.clone(), sign, metrics));
        }

        tracing::info!(
            service.name = %self.config.server.service_name,
            api.enabled = self.config.server.enabled,
            management.enabled = self.config.mgmt.enabled,
            signer.local.enabled = self.config.signers.local.enabled,
            signer.mqtt.enabled = self.config.signers.mqtt.enabled,
            "AgentVault runtime started"
        );
        let result = supervisor.run().await;
        telemetry.shutdown();
        result
    }
}

fn load_api_token(path: &str) -> anyhow::Result<Option<String>> {
    if path.trim().is_empty() {
        return Ok(None);
    }
    let token = read_private_text(path, "API token file", MAX_API_TOKEN_FILE_BYTES).context("read API token")?;
    let token = token.trim_end_matches(['\r', '\n']);
    anyhow::ensure!(!token.is_empty(), "API token file must not be empty");
    Ok(Some(token.to_owned()))
}
