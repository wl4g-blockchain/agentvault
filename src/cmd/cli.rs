use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use crate::{handler::import_from_paths, AgentVaultServer, AppConfig};

#[derive(Debug, Parser)]
#[command(name = "agentvault", version, about = "AgentVault payment infrastructure")]
struct Cli {
    #[arg(long, default_value = "etc/agentvault.yaml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the configured payment and management components.
    Serve,
    /// Operate on declarative `AgentVault` resources.
    Console {
        #[command(subcommand)]
        operation: ConsoleCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConsoleCommand {
    /// Idempotently import resource YAML/JSON files or directories.
    Import {
        /// Resource files or directories. Directories are traversed in stable order.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
}

/// Parses the process command line and executes one `AgentVault` command.
///
/// # Errors
///
/// Returns configuration, resource-import, or runtime startup failures.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = AppConfig::load(&cli.config).with_context(|| format!("load {}", cli.config.display()))?;
    match cli.command {
        Command::Serve => AgentVaultServer::assemble(config).await?.run().await,
        Command::Console {
            operation: ConsoleCommand::Import { paths },
        } => {
            let report = import_from_paths(&config, &paths)
                .await
                .context("import AgentVault console resources")?;
            println!(
                "resource import completed: {} created, {} reused",
                report.created, report.reused
            );
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/cli_test.rs"]
mod tests;
