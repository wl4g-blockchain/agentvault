//! External E2E payment provider: signed Tempo charges and native Solana exact.

mod fund;
mod provider;
mod solana;

use axum::{extract::DefaultBodyLimit, routing::get, Router};
use provider::TempoFacilitator;

pub type ServiceError = Box<dyn std::error::Error>;

struct ProviderServer;

impl ProviderServer {
    async fn run() -> Result<(), ServiceError> {
        let _ = tracing_subscriber::fmt().json().with_target(false).try_init();
        let mut app = Router::new().route("/healthz", get(|| async { "ok" }));
        if let Some(funding) = fund::FundingService::from_env()? {
            app = app.merge(funding.router());
        }
        if let Some(tempo) = TempoFacilitator::from_env().await? {
            app = app.merge(tempo.router());
        }
        if let Some(solana) = solana::SolanaFacilitator::from_env().await? {
            app = app.merge(solana.router());
        }
        let bind = std::env::var("MANUAL_PAYMENT_PROVIDER_BIND").unwrap_or_else(|_| "0.0.0.0:8087".to_owned());
        let listener = tokio::net::TcpListener::bind(&bind).await?;
        tracing::info!(event = "manual.payment_provider.listening", address = %bind);
        axum::serve(listener, app.layer(DefaultBodyLimit::max(256 * 1024)))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), ServiceError> {
    ProviderServer::run().await
}
