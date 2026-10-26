use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    agentvault::cmd::ConsoleRunner::run().await
}
