use std::env;

use mock_payee_marketpulse_rust_service::server::MarketPulseServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt().json().with_target(false).try_init();
    run().await
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => MarketPulseServer::from_env()?.run().await,
        [argument] if argument == "--healthcheck" => MarketPulseServer::healthcheck().await,
        _ => Err("usage: mock-payee-marketpulse-rust-service [--healthcheck]".into()),
    }
}

#[cfg(test)]
#[path = "../tests/main_test.rs"]
mod tests;
