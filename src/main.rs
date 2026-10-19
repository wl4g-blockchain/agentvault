use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    agentvault::cmd::run_cli().await
}
