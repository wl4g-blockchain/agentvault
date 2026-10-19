use mock_payee_marketpulse_rust_service::server::MarketPulseServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt().json().with_target(false).try_init();
    run().await
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    MarketPulseServer::from_env()?.run().await
}

#[cfg(test)]
#[path = "../tests/main_test.rs"]
mod tests;
