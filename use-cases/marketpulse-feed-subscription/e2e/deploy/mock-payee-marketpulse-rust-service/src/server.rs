use std::{env, net::SocketAddr, time::Duration};

use agentvault_sdk::{
    AgentVault, AxumPayeeMiddleware, AxumPayeeRoute, GrpcConfig, GrpcVaultTransport, MqttConfig, MqttVaultTransport,
    PayeeConfig, TransportError, TransportRequest, TransportResponse, VaultTransport,
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
        let transport = payee_transport()?;
        let payee: AxumPayeeMiddleware<_> =
            AgentVault::new(transport)
                .payee(PayeeConfig::default())?
                .axum(vec![AxumPayeeRoute::mcp_tool(
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
        })
    }

    /// Starts the listener until process shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when the listener cannot bind or the HTTP server fails.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind(self.bind).await?;
        println!("MarketPulse Rust ready on {}", self.bind);
        axum::serve(listener, self.app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    }
}

fn parse_bind(value: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    Ok(value.parse()?)
}

#[derive(Clone)]
enum PayeeTransport {
    Mqtt(MqttVaultTransport),
    Grpc(GrpcVaultTransport),
}

impl VaultTransport for PayeeTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        match self {
            Self::Mqtt(transport) => transport.send(request).await,
            Self::Grpc(transport) => transport.send(request).await,
        }
    }
}

fn payee_transport() -> Result<PayeeTransport, Box<dyn std::error::Error>> {
    match env::var("AGENTVAULT_TRANSPORT")
        .unwrap_or_else(|_| "mqtt".to_owned())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "mqtt" => Ok(PayeeTransport::Mqtt(MqttVaultTransport::new(MqttConfig {
            host: required_env("AGENTVAULT_MQTT_HOST")?,
            port: required_env("AGENTVAULT_MQTT_PORT")?.parse()?,
            tls: boolean_env("AGENTVAULT_MQTT_TLS")?,
            username: required_env("AGENTVAULT_MQTT_USERNAME")?,
            password: required_env("AGENTVAULT_MQTT_PASSWORD")?,
            connection_prefix: "marketpulse-rust".to_owned(),
            vault_client_id: required_env("AGENTVAULT_MQTT_CLIENT_ID")?,
            request_timeout: Duration::from_secs(required_env("AGENTVAULT_MQTT_TIMEOUT_SECONDS")?.parse()?),
            max_response_bytes: 2 * 1024 * 1024,
        })?)),
        "grpc" => {
            let mut config = GrpcConfig::new(required_env("AGENTVAULT_GRPC_ENDPOINT")?)
                .with_tls(boolean_env("AGENTVAULT_GRPC_TLS")?);
            config.request_timeout = Duration::from_secs(required_env("AGENTVAULT_GRPC_TIMEOUT_SECONDS")?.parse()?);
            Ok(PayeeTransport::Grpc(GrpcVaultTransport::new(config)?))
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
