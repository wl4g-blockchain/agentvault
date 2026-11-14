use anyhow::Result;
#[tokio::main]
async fn main() -> Result<()> {
    agentvault_cmd::ConsoleRunner::run().await
}
