//! Administrative command line and interactive console.
//!
//! Catalog writes reuse the canonical resource importer or domain storage
//! ports. Financial history is read-only. Wallet custody has a deliberately
//! narrower surface: create and public-metadata reads only. No command can
//! load, rotate, update, delete, or export private key material.

use std::{
    io::{self, BufRead as _, Write as _},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{anyhow, Context as _, Result};
use clap::{Args, CommandFactory as _, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::model::Amount;
use crate::model::ApprovalDecisionKind;
use crate::model::SubmitApprovalDecision;
use crate::payment::ResourceImporter;
use crate::storage::IStorage;
use crate::storage::LedgerQuery;
use crate::storage::PageRequest;
use crate::storage::PaymentQuery;
use crate::storage::StorageFactory;
use crate::storage::StoragePorts;
use crate::AgentVaultServer;
use crate::AppConfig;

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
    /// Open the administrative console or execute one console command.
    Console {
        #[command(subcommand)]
        operation: Option<ConsoleCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum ConsoleCommand {
    /// Idempotently create canonical resources from YAML/JSON files.
    Import {
        /// Resource files or directories, traversed in stable order.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// List public metadata or immutable operational records.
    List(ListArgs),
    /// Get one public metadata or immutable operational record.
    Get(GetArgs),
    /// Enable or disable a mutable catalog resource.
    SetEnabled(SetEnabledArgs),
    /// Create wallets or read wallet public metadata.
    Wallet {
        #[command(subcommand)]
        operation: WalletCommand,
    },
    /// Apply an approval decision through the existing domain state machine.
    Approval {
        #[command(subcommand)]
        operation: ApprovalCommand,
    },
    /// Start the interactive management console.
    Shell,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ConsoleResource {
    Client,
    Scenario,
    Policy,
    #[value(alias = "offer")]
    Price,
    Approver,
    Payment,
    Approval,
    Ledger,
    Event,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MutableResource {
    Client,
    Scenario,
    Policy,
    #[value(alias = "offer")]
    Price,
    Approver,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(value_enum)]
    resource: ConsoleResource,
    #[arg(long)]
    client_id: Option<String>,
    #[arg(long)]
    scenario_id: Option<String>,
    #[arg(long)]
    wallet_id: Option<String>,
    #[arg(long)]
    intent_id: Option<String>,
    #[arg(long, default_value_t = 0)]
    offset: u64,
    #[arg(long, default_value_t = 50)]
    limit: u32,
}

#[derive(Debug, Args)]
struct GetArgs {
    #[arg(value_enum)]
    resource: ConsoleResource,
    id: String,
    /// Required for an approver, whose identity is `(scenario, principal)`.
    #[arg(long)]
    scenario_id: Option<String>,
}

#[derive(Debug, Args)]
struct SetEnabledArgs {
    #[arg(value_enum)]
    resource: MutableResource,
    id: String,
    #[arg(long, required = true, action = clap::ArgAction::Set)]
    enabled: bool,
    /// Required for an approver, whose `id` argument is the principal.
    #[arg(long)]
    scenario_id: Option<String>,
}

#[derive(Debug, Subcommand)]
enum WalletCommand {
    /// Create wallets from canonical Wallet resources only.
    Create {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// List wallet public metadata. Custody keys are never loaded.
    List {
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Get wallet public metadata. Custody keys are never loaded.
    Get { id: String },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Decision {
    Approve,
    Reject,
}

impl From<Decision> for ApprovalDecisionKind {
    fn from(value: Decision) -> Self {
        match value {
            Decision::Approve => Self::Approve,
            Decision::Reject => Self::Reject,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ApprovalCommand {
    /// Decide one pending approval as an explicitly authorized principal.
    Decide {
        approval_id: String,
        intent_id: String,
        #[arg(long)]
        principal: String,
        #[arg(long, value_enum)]
        decision: Decision,
        #[arg(long)]
        approved_amount: Option<Amount>,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Debug, Parser)]
#[command(name = "agentvault-console", no_binary_name = false)]
struct ConsoleLine {
    #[command(subcommand)]
    command: ConsoleCommand,
}

struct ConsoleStore {
    storage: Arc<dyn IStorage>,
    ports: StoragePorts,
}

impl ConsoleStore {
    async fn open(config: &AppConfig) -> Result<Self> {
        let storage = StorageFactory::new()
            .create(&config.storage)
            .await
            .context("open authoritative console storage")?;
        let ports = Arc::clone(&storage).ports();
        Ok(Self { storage, ports })
    }

    async fn close(self) -> Result<()> {
        self.storage
            .close()
            .await
            .context("close authoritative console storage")
    }

    fn page(offset: u64, limit: u32) -> Result<PageRequest> {
        if !(1..=500).contains(&limit) {
            return Err(anyhow!("console page limit must be between 1 and 500"));
        }
        Ok(PageRequest { offset, limit })
    }

    fn required<'a>(value: Option<&'a str>, flag: &str, resource: ConsoleResource) -> Result<&'a str> {
        value
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("--{flag} is required when listing {resource:?}"))
    }

    async fn list(&self, args: &ListArgs) -> Result<Value> {
        let page = Self::page(args.offset, args.limit)?;
        let value = match args.resource {
            ConsoleResource::Client => serde_json::to_value(self.ports.clients.list(page).await?)?,
            ConsoleResource::Scenario => {
                let client_id = Self::required(args.client_id.as_deref(), "client-id", args.resource)?;
                serde_json::to_value(self.ports.scenarios.list_for_client(client_id, page).await?)?
            }
            ConsoleResource::Policy => {
                let scenario_id = Self::required(args.scenario_id.as_deref(), "scenario-id", args.resource)?;
                serde_json::to_value(self.ports.policies.list_for_scenario(scenario_id, page).await?)?
            }
            ConsoleResource::Price => {
                let scenario_id = Self::required(args.scenario_id.as_deref(), "scenario-id", args.resource)?;
                serde_json::to_value(self.ports.offers.list_for_scenario(scenario_id, page).await?)?
            }
            ConsoleResource::Approver => {
                let scenario_id = Self::required(args.scenario_id.as_deref(), "scenario-id", args.resource)?;
                serde_json::to_value(self.ports.approvers.list_for_scenario(scenario_id, page).await?)?
            }
            ConsoleResource::Payment => serde_json::to_value(
                self.ports
                    .payments
                    .list(&PaymentQuery {
                        scenario_id: args.scenario_id.clone(),
                        wallet_id: args.wallet_id.clone(),
                        page,
                        ..PaymentQuery::default()
                    })
                    .await?,
            )?,
            ConsoleResource::Approval => {
                let scenario_id = Self::required(args.scenario_id.as_deref(), "scenario-id", args.resource)?;
                serde_json::to_value(
                    self.ports
                        .approvals
                        .list_pending(scenario_id, Self::now()?, page)
                        .await?,
                )?
            }
            ConsoleResource::Ledger => serde_json::to_value(
                self.ports
                    .ledger
                    .list(&LedgerQuery {
                        scenario_id: args.scenario_id.clone(),
                        wallet_id: args.wallet_id.clone(),
                        intent_id: args.intent_id.clone(),
                        page,
                        ..LedgerQuery::default()
                    })
                    .await?,
            )?,
            ConsoleResource::Event => {
                let intent_id = Self::required(args.intent_id.as_deref(), "intent-id", args.resource)?;
                serde_json::to_value(self.ports.events.list_for_intent(intent_id, page).await?)?
            }
        };
        Ok(value)
    }

    async fn get(&self, args: &GetArgs) -> Result<Value> {
        let value = match args.resource {
            ConsoleResource::Client => serde_json::to_value(
                self.ports
                    .clients
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("client not found"))?,
            )?,
            ConsoleResource::Scenario => serde_json::to_value(
                self.ports
                    .scenarios
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("scenario not found"))?,
            )?,
            ConsoleResource::Policy => serde_json::to_value(
                self.ports
                    .policies
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("policy not found"))?,
            )?,
            ConsoleResource::Price => serde_json::to_value(
                self.ports
                    .offers
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("offer not found"))?,
            )?,
            ConsoleResource::Approver => {
                let scenario_id = Self::required(args.scenario_id.as_deref(), "scenario-id", args.resource)?;
                serde_json::to_value(
                    self.ports
                        .approvers
                        .get(scenario_id, &args.id)
                        .await?
                        .ok_or_else(|| anyhow!("approver not found"))?,
                )?
            }
            ConsoleResource::Payment => serde_json::to_value(
                self.ports
                    .payments
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("payment not found"))?,
            )?,
            ConsoleResource::Approval => serde_json::to_value(
                self.ports
                    .approvals
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("approval not found"))?,
            )?,
            ConsoleResource::Ledger => serde_json::to_value(
                self.ports
                    .ledger
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("ledger entry not found"))?,
            )?,
            ConsoleResource::Event => serde_json::to_value(
                self.ports
                    .events
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("event not found"))?,
            )?,
        };
        Ok(value)
    }

    async fn set_enabled(&self, args: &SetEnabledArgs) -> Result<Value> {
        let now = Self::now()?;
        let value = match args.resource {
            MutableResource::Client => {
                serde_json::to_value(self.ports.clients.set_enabled(&args.id, args.enabled, now).await?)?
            }
            MutableResource::Scenario => {
                serde_json::to_value(self.ports.scenarios.set_enabled(&args.id, args.enabled, now).await?)?
            }
            MutableResource::Policy => {
                let mut policy = self
                    .ports
                    .policies
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("policy not found"))?;
                let expected_updated_at = policy.updated_at;
                policy.enabled = args.enabled;
                policy.updated_at = Self::next_timestamp(expected_updated_at, now)?;
                serde_json::to_value(self.ports.policies.update(&policy, expected_updated_at).await?)?
            }
            MutableResource::Price => {
                let mut offer = self
                    .ports
                    .offers
                    .get(&args.id)
                    .await?
                    .ok_or_else(|| anyhow!("offer not found"))?;
                let expected_updated_at = offer.updated_at;
                offer.enabled = args.enabled;
                offer.updated_at = Self::next_timestamp(expected_updated_at, now)?;
                serde_json::to_value(self.ports.offers.update(&offer, expected_updated_at).await?)?
            }
            MutableResource::Approver => {
                let scenario_id = args
                    .scenario_id
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| anyhow!("--scenario-id is required when updating Approver"))?;
                serde_json::to_value(
                    self.ports
                        .approvers
                        .set_enabled(scenario_id, &args.id, args.enabled, now)
                        .await?,
                )?
            }
        };
        Ok(value)
    }

    async fn list_wallets(&self, offset: u64, limit: u32) -> Result<Value> {
        Ok(serde_json::to_value(
            self.ports.wallets.list(Self::page(offset, limit)?).await?,
        )?)
    }

    async fn get_wallet(&self, id: &str) -> Result<Value> {
        Ok(serde_json::to_value(
            self.ports
                .wallets
                .get(id)
                .await?
                .ok_or_else(|| anyhow!("wallet not found"))?,
        )?)
    }

    fn next_timestamp(previous: i64, now: i64) -> Result<i64> {
        if now > previous {
            Ok(now)
        } else {
            match previous.checked_add(1) {
                Some(value) => Ok(value),
                None => Err(anyhow::Error::msg("resource timestamp overflowed")),
            }
        }
    }

    fn now() -> Result<i64> {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if now < 0 {
            Err(anyhow!("console requires a non-negative UTC timestamp"))
        } else {
            Ok(now)
        }
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/console_test.rs"]
mod tests;

/// Administrative command execution and console lifecycle.
pub struct ConsoleRunner;
impl ConsoleRunner {
    /// Parses the process command line and executes one `AgentVault` command.
    ///
    /// # Errors
    ///
    /// Returns configuration, console, storage, or runtime startup failures.
    pub async fn run() -> Result<()> {
        let cli = Cli::parse();
        let config = AppConfig::load(&cli.config).with_context(|| format!("load {}", cli.config.display()))?;
        match cli.command {
            Command::Serve => AgentVaultServer::assemble(config).await?.run().await,
            Command::Console {
                operation: None | Some(ConsoleCommand::Shell),
            } => ConsoleRunner::run_shell(&config).await,
            Command::Console {
                operation: Some(operation),
            } => ConsoleRunner::execute(&config, operation).await,
        }
    }

    async fn execute(config: &AppConfig, command: ConsoleCommand) -> Result<()> {
        match command {
            ConsoleCommand::Import { paths } => {
                let report = ResourceImporter::import_from_paths(config, &paths)
                    .await
                    .context("import AgentVault console resources")?;
                println!(
                    "resource import completed: {} created, {} reused",
                    report.created, report.reused
                );
                Ok(())
            }
            ConsoleCommand::Wallet {
                operation: WalletCommand::Create { paths },
            } => {
                let report = ResourceImporter::create_wallets_from_paths(config, &paths)
                    .await
                    .context("create AgentVault wallets")?;
                println!(
                    "wallet creation completed: {} created, {} reused",
                    report.created, report.reused
                );
                Ok(())
            }
            ConsoleCommand::Wallet {
                operation: WalletCommand::List { offset, limit },
            } => {
                let store = ConsoleStore::open(config).await?;
                let result = store.list_wallets(offset, limit).await;
                ConsoleRunner::finish_read(store, result).await
            }
            ConsoleCommand::Wallet {
                operation: WalletCommand::Get { id },
            } => {
                let store = ConsoleStore::open(config).await?;
                let result = store.get_wallet(&id).await;
                ConsoleRunner::finish_read(store, result).await
            }
            ConsoleCommand::List(args) => {
                let store = ConsoleStore::open(config).await?;
                let result = store.list(&args).await;
                ConsoleRunner::finish_read(store, result).await
            }
            ConsoleCommand::Get(args) => {
                let store = ConsoleStore::open(config).await?;
                let result = store.get(&args).await;
                ConsoleRunner::finish_read(store, result).await
            }
            ConsoleCommand::SetEnabled(args) => {
                let store = ConsoleStore::open(config).await?;
                let result = store.set_enabled(&args).await;
                ConsoleRunner::finish_read(store, result).await
            }
            ConsoleCommand::Approval {
                operation:
                    ApprovalCommand::Decide {
                        approval_id,
                        intent_id,
                        principal,
                        decision,
                        approved_amount,
                        reason,
                    },
            } => {
                let store = ConsoleStore::open(config).await?;
                let handler =
                    super::payment_runtime::PaymentRuntimeFactory::console_approval_handler(config, &store.ports)?;
                let result = handler
                    .submit_decision(
                        &principal,
                        &SubmitApprovalDecision {
                            approval_id,
                            intent_id,
                            decision: decision.into(),
                            approved_amount,
                            reason,
                        },
                        ConsoleStore::now()?,
                    )
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| serde_json::to_value(result.approval).map_err(anyhow::Error::from));
                ConsoleRunner::finish_read(store, result).await
            }
            ConsoleCommand::Shell => Err(anyhow!("shell cannot be nested")),
        }
    }

    async fn finish_read(store: ConsoleStore, result: Result<Value>) -> Result<()> {
        let close = store.close().await;
        let value = result?;
        close?;
        ConsoleRunner::print_json(&value)
    }

    fn print_json(value: &impl Serialize) -> Result<()> {
        println!("{}", serde_json::to_string_pretty(value)?);
        Ok(())
    }

    async fn run_shell(config: &AppConfig) -> Result<()> {
        println!("AgentVault Management Console");
        println!("Wallet custody is write-only: create and metadata reads are the only wallet commands.");
        println!("Type 'help' for commands or 'exit' to quit.");
        let stdin = io::stdin();
        let mut lines = stdin.lock().lines();
        loop {
            print!("agentvault> ");
            io::stdout().flush().context("flush console prompt")?;
            let Some(line) = lines.next() else {
                break;
            };
            let line = line.context("read console command")?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if matches!(line, "exit" | "quit") {
                break;
            }
            if line == "help" {
                ConsoleLine::command().print_help()?;
                println!();
                continue;
            }
            let args = std::iter::once("agentvault-console").chain(line.split_ascii_whitespace());
            match ConsoleLine::try_parse_from(args) {
                Ok(ConsoleLine {
                    command: ConsoleCommand::Shell,
                }) => eprintln!("nested shell sessions are not supported"),
                Ok(parsed) => {
                    if let Err(error) = ConsoleRunner::execute(config, parsed.command).await {
                        eprintln!("error: {error:#}");
                    }
                }
                Err(error) => eprintln!("{error}"),
            }
        }
        Ok(())
    }
}
