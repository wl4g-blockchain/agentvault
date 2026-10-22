use std::{env, net::SocketAddr, time::Duration};

use agentvault_sdk::{
    AgentVault, AxumPayeeMiddleware, AxumPayeeRoute, ConfiguredVaultTransport, GrpcConfig, MqttConfig, PayeeConfig,
    VaultTransportConfig,
};
use axum::{routing::post, Router};

use crate::{
    handler::feed::{mcp, FeedHandler},
    required_env,
    route::feed::FeedRouter,
    storage::feed::FeedSQLiteStorage,
};

/// Owns the Axum listener, MCP endpoint, SDK middleware and component lifecycle.
pub struct MarketPulseServer {
    bind: SocketAddr,
    app: Router,
    vault: AgentVault<ConfiguredVaultTransport>,
}

impl MarketPulseServer {
    /// Builds the complete `MarketPulse` MCP server from environment configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when required configuration is missing, the listener
    /// address is invalid, or the durable `SQLite` store cannot be opened.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let bind = parse_bind(&required_env("MARKETPULSE_BIND")?)?;
        let vault = AgentVault::from_config(payee_transport_config()?)?;
        let payee: AxumPayeeMiddleware<_> = vault.payee(PayeeConfig::default())?.axum(vec![AxumPayeeRoute::mcp_tool(
            "/mcp",
            "marketpulse.realtime_news",
            required_env("MARKETPULSE_SCENARIO_ID")?,
            required_env("MARKETPULSE_OFFER_ID")?,
            required_env("MARKETPULSE_RESOURCE_URL")?,
            required_env("MARKETPULSE_ORIGIN")?,
        )]);
        let storage = FeedSQLiteStorage::open(&required_env("MARKETPULSE_DATABASE")?)?;
        let allow_failure = env::var("MARKETPULSE_E2E_ALLOW_FAILURE_INJECTION").is_ok_and(|value| value == "true");
        let handler = FeedHandler::new(storage, allow_failure);
        let mcp = Router::new()
            .route("/mcp", post(mcp))
            .with_state(handler.clone())
            .layer(payee);
        Ok(Self {
            bind,
            app: FeedRouter::new(handler).router().merge(mcp),
            vault,
        })
    }

    /// Checks the local HTTP health endpoint without constructing payment state.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured listener is invalid, unreachable,
    /// or does not report success.
    pub async fn healthcheck() -> Result<(), Box<dyn std::error::Error>> {
        let port = parse_bind(&required_env("MARKETPULSE_BIND")?)?.port();
        let status = reqwest::get(format!("http://127.0.0.1:{port}/healthz")).await?.status();
        if !status.is_success() {
            return Err(format!("MarketPulse health endpoint returned {status}").into());
        }
        Ok(())
    }

    /// Starts the listener until process shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener cannot bind or the HTTP server fails.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind(self.bind).await?;
        println!("MarketPulse Rust ready on {}", self.bind);
        let server_result = axum::serve(listener, self.app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await;
        let close_result = self.vault.close().await;
        server_result?;
        close_result?;
        Ok(())
    }
}

fn parse_bind(value: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    Ok(value.parse()?)
}

fn payee_transport_config() -> Result<VaultTransportConfig, Box<dyn std::error::Error>> {
    match env::var("AGENTVAULT_TRANSPORT")
        .unwrap_or_else(|_| "mqtt".to_owned())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "mqtt" => Ok(VaultTransportConfig::Mqtt(MqttConfig {
            host: required_env("AGENTVAULT_MQTT_HOST")?,
            port: required_env("AGENTVAULT_MQTT_PORT")?.parse()?,
            tls: boolean_env("AGENTVAULT_MQTT_TLS")?,
            username: required_env("AGENTVAULT_MQTT_USERNAME")?,
            password: required_env("AGENTVAULT_MQTT_PASSWORD")?,
            connection_prefix: "marketpulse-rust".to_owned(),
            vault_client_id: required_env("AGENTVAULT_MQTT_CLIENT_ID")?,
            request_timeout: Duration::from_secs(required_env("AGENTVAULT_MQTT_TIMEOUT_SECONDS")?.parse()?),
            max_response_bytes: 2 * 1024 * 1024,
        })),
        "grpc" => {
            let mut config = GrpcConfig::new(required_env("AGENTVAULT_GRPC_ENDPOINT")?)
                .with_tls(boolean_env("AGENTVAULT_GRPC_TLS")?);
            config.request_timeout = Duration::from_secs(required_env("AGENTVAULT_GRPC_TIMEOUT_SECONDS")?.parse()?);
            Ok(VaultTransportConfig::Grpc(config))
        }
        _ => Err("AGENTVAULT_TRANSPORT must be mqtt or grpc".into()),
    }
}

fn boolean_env(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    match env::var(name).unwrap_or_else(|_| "false".to_owned()).trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("{name} must be true or false").into()),
    }
}

#[cfg(test)]
#[path = "../tests/server_test.rs"]
mod tests;
