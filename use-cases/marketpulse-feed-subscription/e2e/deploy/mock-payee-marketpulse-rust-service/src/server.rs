use std::{env, net::SocketAddr};

use agentvault_sdk::{AgentVault, AxumPayeeMiddleware, AxumPayeeRoute, HttpVaultTransport, PayeeConfig};
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
        let vault_url = required_env("AGENTVAULT_URL")?;
        let transport = match env::var("MARKETPULSE_AUTHGUARD_BEARER") {
            Ok(value) if !value.trim().is_empty() => {
                HttpVaultTransport::new(&vault_url)?.with_authguard_bearer(value)?
            }
            _ => HttpVaultTransport::new(&vault_url)?,
        };
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

#[cfg(test)]
#[path = "../tests/server_test.rs"]
mod tests;
