use std::{path::PathBuf, sync::Arc};

use agent_wallet::{
    store::{generate_master_key_file, KeyInfo},
    AgentVaultServer, AppConfig, EncryptedFileStore,
};
use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "walletd", version, about = "Process-isolated payment signing service")]
struct Cli {
    #[arg(long, default_value = "etc/agentvault.yaml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run configured API, management, local, and MQTT components.
    Serve,
    /// Manage EOA wallets while walletd is stopped.
    #[command(alias = "key")]
    Wallet {
        #[command(subcommand)]
        command: WalletCommand,
    },
    /// Manage the store master key while walletd is stopped.
    MasterKey {
        #[command(subcommand)]
        command: MasterKeyCommand,
    },
}

#[derive(Debug, Subcommand)]
enum WalletCommand {
    Generate {
        id: String,
    },
    Import {
        id: String,
        #[arg(long)]
        private_key_file: PathBuf,
    },
    Show {
        id: String,
    },
    List,
    Rotate {
        id: String,
    },
    Delete {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum MasterKeyCommand {
    Generate {
        #[arg(long)]
        output: PathBuf,
    },
    Rotate {
        #[arg(long)]
        new_key_file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::MasterKey {
        command: MasterKeyCommand::Generate { output },
    } = &cli.command
    {
        generate_master_key_file(output).with_context(|| format!("generate master key at {}", output.display()))?;
        println!("generated {}", output.display());
        return Ok(());
    }

    let config = AppConfig::load(&cli.config).with_context(|| format!("load {}", cli.config.display()))?;
    let store = Arc::new(
        EncryptedFileStore::open(&config.store.directory, &config.store.master_key_file)
            .context("open encrypted wallet store")?,
    );

    match cli.command {
        Command::Serve => AgentVaultServer::new(config, store).run().await,
        Command::Wallet { command } => manage_wallet(command, &store),
        Command::MasterKey {
            command: MasterKeyCommand::Rotate { new_key_file },
        } => {
            store
                .rotate_master_key(&config.store.master_key_file, &new_key_file)
                .context("rotate master key")?;
            println!("rotated master key envelope; update store.master_key_file before restarting walletd");
            Ok(())
        }
        Command::MasterKey {
            command: MasterKeyCommand::Generate { .. },
        } => unreachable!(),
    }
}

fn manage_wallet(command: WalletCommand, store: &EncryptedFileStore) -> Result<()> {
    match command {
        WalletCommand::Generate { id } => print_wallet(&store.generate(&id).context("generate EOA wallet")?),
        WalletCommand::Import { id, private_key_file } => print_wallet(
            &store
                .import_file(&id, &private_key_file)
                .with_context(|| format!("import EOA wallet from {}", private_key_file.display()))?,
        ),
        WalletCommand::Show { id } => print_wallet(&store.get(&id).context("read wallet")?),
        WalletCommand::List => {
            for wallet in store.list().context("list wallets")? {
                print_wallet(&wallet);
            }
        }
        WalletCommand::Rotate { id } => print_wallet(&store.rotate(&id).context("rotate EOA wallet")?),
        WalletCommand::Delete { id } => {
            store.delete(&id).context("delete wallet")?;
            println!("deleted {id}");
        }
    }
    Ok(())
}

fn print_wallet(wallet: &KeyInfo) {
    println!("{}\t{}\t{}", wallet.id, wallet.algorithm, wallet.address);
}
