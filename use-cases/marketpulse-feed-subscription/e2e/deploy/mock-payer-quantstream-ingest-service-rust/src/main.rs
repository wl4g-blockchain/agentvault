use std::{env, sync::Arc};

use quantstream_ingestor_rust::{
    agent::mcp::DynError,
    handler::ingestion::IngestionHandler,
    job::ingestion_job::{unix_time, IngestionJob},
    route::ingestion::IngestionRouter,
};

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if run_once_requested(&arguments)? {
        return IngestionJob::from_env()
            .await?
            .run_once(&format!("rust-{}", unix_time()))
            .await;
    }
    let job = Arc::new(IngestionJob::from_env().await?);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    let server = axum::serve(listener, IngestionRouter::new(IngestionHandler).router());
    tokio::select! {
        result = Arc::clone(&job).run_scheduler() => result,
        result = server => result.map_err(Into::into),
    }
}

fn run_once_requested(arguments: &[String]) -> Result<bool, DynError> {
    if arguments.len() > 1 || arguments.first().is_some_and(|argument| argument != "--once") {
        return Err("usage: quantstream-ingestor-rust [--once]".into());
    }
    Ok(!arguments.is_empty())
}

#[cfg(test)]
#[path = "../tests/main_test.rs"]
mod tests;
