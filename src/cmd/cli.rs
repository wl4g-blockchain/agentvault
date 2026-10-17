use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use crate::{handler::provision_from_path, AgentVaultServer, AppConfig};

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
    /// Idempotently register payment identities, wallets, policy, and offers.
    Provision {
        /// Versioned JSON manifest containing public metadata and secret environment references.
        #[arg(long)]
        manifest: PathBuf,
    },
}

/// Parses the process command line and executes one `AgentVault` command.
///
/// # Errors
///
/// Returns configuration, provisioning, or runtime startup failures.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = AppConfig::load(&cli.config).with_context(|| format!("load {}", cli.config.display()))?;
    match cli.command {
        Command::Serve => AgentVaultServer::assemble(config).await?.run().await,
        Command::Provision { manifest } => {
            let report = provision_from_path(&config, &manifest)
                .await
                .with_context(|| format!("apply {}", manifest.display()))?;
            println!(
                "provisioning completed: {} created, {} reused",
                report.created, report.reused
            );
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/cli_test.rs"]
mod tests;
