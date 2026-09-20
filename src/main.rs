use std::{path::PathBuf, sync::Arc};

use agent_wallet::{
    store::generate_master_key_file,
    transport::{local, mqtt},
    Config, EncryptedFileStore, WalletService,
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "walletd", version, about = "Process-isolated EOA signing service")]
struct Cli {
    #[arg(long, default_value = "config/wallet.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the configured MQTT and/or Unix-socket signing transports.
    Serve,
    /// Manage EOA keys while walletd is stopped.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// Manage the store master key while walletd is stopped.
    MasterKey {
        #[command(subcommand)]
        command: MasterKeyCommand,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
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
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();

    let cli = Cli::parse();
    if let Command::MasterKey {
        command: MasterKeyCommand::Generate { output },
    } = &cli.command
    {
        generate_master_key_file(output).with_context(|| format!("generate master key at {}", output.display()))?;
        println!("generated {}", output.display());
        return Ok(());
    }

    let config = Config::load(&cli.config).with_context(|| format!("load {}", cli.config.display()))?;
    if matches!(&cli.command, Command::Serve) {
        config.validate_for_serve().context("validate service configuration")?;
    }
    let store = Arc::new(
        EncryptedFileStore::open(&config.store.directory, &config.store.master_key_file)
            .context("open encrypted wallet store")?,
    );

    match cli.command {
        Command::Serve => serve(config, store).await,
        Command::Key { command } => manage_key(command, &store),
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

async fn serve(config: Config, store: Arc<EncryptedFileStore>) -> Result<()> {
    let service = Arc::new(WalletService::new(store, config.security.clone()));
    let local_enabled = config.transports.local.enabled;
    let mqtt_enabled = config.transports.mqtt.enabled;
    let local_config = config.transports.local;
    let mqtt_config = config.transports.mqtt;

    match (local_enabled, mqtt_enabled) {
        (true, true) => {
            tokio::select! {
                result = local::run(local_config.socket_path, Arc::clone(&service)) => result.context("local transport"),
                result = mqtt::run(mqtt_config, service) => result.context("MQTT transport"),
                result = tokio::signal::ctrl_c() => result.context("wait for shutdown signal"),
            }
        }
        (true, false) => {
            tokio::select! {
                result = local::run(local_config.socket_path, service) => result.context("local transport"),
                result = tokio::signal::ctrl_c() => result.context("wait for shutdown signal"),
            }
        }
        (false, true) => {
            tokio::select! {
                result = mqtt::run(mqtt_config, service) => result.context("MQTT transport"),
                result = tokio::signal::ctrl_c() => result.context("wait for shutdown signal"),
            }
        }
        (false, false) => unreachable!("configuration validation requires a transport"),
    }
}

fn manage_key(command: KeyCommand, store: &EncryptedFileStore) -> Result<()> {
    match command {
        KeyCommand::Generate { id } => print_key(&store.generate(&id).context("generate EOA key")?),
        KeyCommand::Import { id, private_key_file } => {
            print_key(
                &store
                    .import_file(&id, &private_key_file)
                    .with_context(|| format!("import EOA key from {}", private_key_file.display()))?,
            );
        }
        KeyCommand::Show { id } => print_key(&store.get(&id).context("read wallet key")?),
        KeyCommand::List => {
            for key in store.list().context("list wallet keys")? {
                print_key(&key);
            }
        }
        KeyCommand::Delete { id } => {
            store.delete(&id).context("delete wallet key")?;
            println!("deleted {id}");
        }
    }
    Ok(())
}

fn print_key(key: &agent_wallet::store::KeyInfo) {
    println!("{}\t{}\t{}", key.id, key.algorithm, key.address);
}
