//! Idempotent console resource import for payment identities and policy.
//!
//! Resource files contain only public metadata and environment-variable names.
//! Local private keys are resolved at execution time, zeroized in memory, and
//! persisted only through the versioned SQL envelope-encryption repositories.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Read as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{de::Error as _, Deserialize, Deserializer};
use serde_yaml::{Mapping, Value};
use zeroize::Zeroizing;

use crate::{
    config::AppConfig,
    model::{
        wallet::{Wallet, EVM_EOA_ALGORITHM},
        Amount, AssetIdentity, BudgetUnit, PaymentClient, PaymentOffer, PaymentPolicy, PaymentPolicyLimits,
        PaymentScenario, PaymentScenarioApprover,
    },
    storage::{
        ApproverStorage, ClientStorage, OfferStorage, PolicyStorage, ScenarioStorage, StorageFactory, StoragePorts,
        WalletCreationConfig, WalletCustody, WalletKeyManager, WalletStorage,
    },
    utils::{crypto, secret::read_secret_env},
    Error, Result,
};

const CONSOLE_VERSION: &str = "core.agentvault.io/v1";
const MAX_RESOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RESOURCE_FILE_BODY_BYTES: usize = 1024 * 1024;
const MAX_RESOURCE_FILES: usize = 1_000;
const MAX_IMPORT_DEPTH: usize = 8;

#[derive(Default)]
struct ImportBatch {
    clients: Vec<ClientSpec>,
    wallets: Vec<WalletSpec>,
    scenarios: Vec<ScenarioSpec>,
    policies: Vec<PolicySpec>,
    offers: Vec<OfferSpec>,
    approvers: Vec<ApproverSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceEnvelope {
    #[serde(rename = "consoleVersion")]
    console_version: String,
    kind: ResourceKind,
    metadata: ResourceMetadata,
    data: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceMetadata {
    name: String,
    #[serde(default)]
    status: ResourceStatus,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResourceStatus {
    #[default]
    Active,
    Disabled,
}

impl ResourceStatus {
    const fn enabled(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Clone, Copy, Deserialize)]
enum ResourceKind {
    Client,
    Wallet,
    Scenario,
    Policy,
    Offer,
    Approver,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientSpec {
    id: String,
    name: String,
    auth_principal: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioSpec {
    id: String,
    client_id: String,
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

struct WalletSpec {
    id: String,
    enabled: bool,
    budget_unit: Option<BudgetUnit>,
    max_per_payment: Option<Amount>,
    max_per_day: Option<Amount>,
    source: WalletSource,
}

enum WalletSource {
    Local {
        algorithm: String,
        private_key_env: String,
    },
    External {
        algorithm: String,
        address: String,
        public_key: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestWalletCustody {
    Local,
    External,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletSpecWire {
    id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    budget_unit: Option<BudgetUnit>,
    #[serde(default)]
    max_per_payment: Option<Amount>,
    #[serde(default)]
    max_per_day: Option<Amount>,
    custody: ManifestWalletCustody,
    #[serde(default)]
    private_key_env: Option<String>,
    #[serde(default)]
    algorithm: Option<String>,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    public_key: Option<String>,
}

impl<'de> Deserialize<'de> for WalletSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WalletSpecWire::deserialize(deserializer)?;
        let source = match wire.custody {
            ManifestWalletCustody::Local => {
                if wire.address.is_some() || wire.public_key.is_some() {
                    return Err(D::Error::custom(
                        "local wallet accepts only algorithm and private_key_env custody material",
                    ));
                }
                WalletSource::Local {
                    algorithm: wire.algorithm.unwrap_or_else(|| EVM_EOA_ALGORITHM.to_owned()),
                    private_key_env: wire
                        .private_key_env
                        .ok_or_else(|| D::Error::custom("local wallet requires private_key_env"))?,
                }
            }
            ManifestWalletCustody::External => {
                if wire.private_key_env.is_some() {
                    return Err(D::Error::custom("external wallet cannot contain private_key_env"));
                }
                WalletSource::External {
                    algorithm: wire
                        .algorithm
                        .ok_or_else(|| D::Error::custom("external wallet requires algorithm"))?,
                    address: wire
                        .address
                        .ok_or_else(|| D::Error::custom("external wallet requires address"))?,
                    public_key: wire.public_key.unwrap_or_default(),
                }
            }
        };
        Ok(Self {
            id: wire.id,
            enabled: wire.enabled,
            budget_unit: wire.budget_unit,
            max_per_payment: wire.max_per_payment,
            max_per_day: wire.max_per_day,
            source,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicySpec {
    id: String,
    scenario_id: String,
    wallet_ids: Vec<String>,
    protocols: Vec<String>,
    networks: Vec<String>,
    assets: Vec<AssetIdentity>,
    budget_unit: BudgetUnit,
    #[serde(default)]
    allowed_recipients: Vec<String>,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default)]
    inbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default)]
    outbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferSpec {
    id: String,
    scenario_id: String,
    policy_id: String,
    resource: String,
    protocols: Vec<String>,
    asset: AssetIdentity,
    amount: Amount,
    wallet_id: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproverSpec {
    scenario_id: String,
    auth_principal: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

const fn default_true() -> bool {
    true
}

/// Counts produced by one idempotent console import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub created: usize,
    pub reused: usize,
}

impl ImportReport {
    fn created(&mut self) {
        self.created += 1;
    }

    fn reused(&mut self) {
        self.reused += 1;
    }
}

struct ImportPorts {
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    policies: Arc<dyn PolicyStorage>,
    offers: Arc<dyn OfferStorage>,
    approvers: Arc<dyn ApproverStorage>,
    wallets: Arc<dyn WalletStorage>,
    local_keys: Arc<dyn WalletKeyManager>,
}

/// Imports canonical `AgentVault` resource files into the configured SQL
/// authority.
///
/// Existing records are reused only when their immutable and effective
/// configuration exactly matches the resource. Drift fails closed instead of
/// silently mutating payment authority.
///
/// # Errors
///
/// Fails for unsafe input paths, invalid or duplicate resources, unavailable
/// environment secrets, configuration drift, or any SQL transaction failure.
pub async fn import_from_paths(config: &AppConfig, paths: &[PathBuf]) -> Result<ImportReport> {
    let mut batch = read_resources(paths)?;
    normalize_batch(&mut batch);
    validate_batch(&batch)?;
    let factory = StorageFactory::new();
    let storage = factory.create(&config.storage).await?;
    let result = async {
        let local_keys = Arc::clone(&storage).wallet_key_manager(&config.agentvault.key_management)?;
        let ports = import_ports(Arc::clone(&storage).ports(), local_keys);
        apply_batch(&ports, batch).await
    }
    .await;
    let close = storage.close().await;
    match result {
        Err(error) => Err(error),
        Ok(report) => {
            close?;
            Ok(report)
        }
    }
}

fn read_resources(paths: &[PathBuf]) -> Result<ImportBatch> {
    if paths.is_empty() {
        return Err(Error::InvalidRequest(
            "console import requires at least one path".to_owned(),
        ));
    }
    let mut files = Vec::new();
    for path in paths {
        collect_resource_files(path, 0, &mut files)?;
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err(Error::InvalidRequest(
            "console import found no YAML or JSON resources".to_owned(),
        ));
    }
    let mut batch = ImportBatch::default();
    for path in files {
        read_resource_file(&path, &mut batch)?;
    }
    Ok(batch)
}

fn collect_resource_files(path: &Path, depth: usize, files: &mut Vec<PathBuf>) -> Result<()> {
    if depth > MAX_IMPORT_DEPTH || files.len() >= MAX_RESOURCE_FILES {
        return Err(Error::Config("console import path limits were exceeded".to_owned()));
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(Error::Config(format!(
            "console import path must not be a symbolic link: {}",
            path.display()
        )));
    }
    if metadata.is_file() {
        if is_resource_file(path) {
            files.push(path.to_path_buf());
        } else {
            return Err(Error::Config(format!(
                "console import file must end in .yaml, .yml, or .json: {}",
                path.display()
            )));
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(Error::Config(format!(
            "console import path is not a regular file or directory: {}",
            path.display()
        )));
    }
    let mut entries = fs::read_dir(path)
        .map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let child = entry.path();
        let child_metadata = fs::symlink_metadata(&child).map_err(|source| Error::ReadFile {
            path: child.clone(),
            source,
        })?;
        if child_metadata.is_dir() || is_resource_file(&child) || child_metadata.file_type().is_symlink() {
            collect_resource_files(&child, depth + 1, files)?;
        }
    }
    Ok(())
}

fn is_resource_file(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yaml" | "yml" | "json"))
}

fn read_resource_file(path: &Path, batch: &mut ImportBatch) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() > MAX_RESOURCE_FILE_BYTES {
        return Err(Error::Config(
            "console resource must be a regular file no larger than 1 MiB".to_owned(),
        ));
    }
    let mut body = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(MAX_RESOURCE_FILE_BODY_BYTES));
    file.take(MAX_RESOURCE_FILE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    if body.len() > MAX_RESOURCE_FILE_BODY_BYTES {
        return Err(Error::Config(
            "console resource changed while reading or exceeds 1 MiB".to_owned(),
        ));
    }
    let mut documents = 0_usize;
    for document in serde_yaml::Deserializer::from_slice(&body) {
        let envelope = ResourceEnvelope::deserialize(document)
            .map_err(|error| Error::Config(format!("invalid console resource {}: {error}", path.display())))?;
        append_resource(batch, envelope, path)?;
        documents += 1;
    }
    if documents == 0 {
        return Err(Error::Config(format!(
            "console resource file is empty: {}",
            path.display()
        )));
    }
    Ok(())
}

fn append_resource(batch: &mut ImportBatch, envelope: ResourceEnvelope, path: &Path) -> Result<()> {
    if envelope.console_version != CONSOLE_VERSION {
        return Err(Error::Config(format!(
            "unsupported consoleVersion in {}: {}",
            path.display(),
            envelope.console_version
        )));
    }
    if envelope.metadata.name.trim().is_empty() {
        return Err(Error::InvalidRequest(format!(
            "console resource in {} has an empty metadata.name",
            path.display()
        )));
    }
    let enabled = envelope.metadata.status.enabled();
    match envelope.kind {
        ResourceKind::Client => batch.clients.push(decode_named_resource(
            envelope.metadata.name,
            enabled,
            envelope.data,
            path,
        )?),
        ResourceKind::Wallet => batch.wallets.push(decode_named_resource(
            envelope.metadata.name,
            enabled,
            envelope.data,
            path,
        )?),
        ResourceKind::Scenario => batch.scenarios.push(decode_named_resource(
            envelope.metadata.name,
            enabled,
            envelope.data,
            path,
        )?),
        ResourceKind::Policy => batch.policies.push(decode_named_resource(
            envelope.metadata.name,
            enabled,
            envelope.data,
            path,
        )?),
        ResourceKind::Offer => batch.offers.push(decode_named_resource(
            envelope.metadata.name,
            enabled,
            envelope.data,
            path,
        )?),
        ResourceKind::Approver => {
            let mut value = resource_mapping(envelope.data, path)?;
            insert_field(&mut value, "enabled", Value::Bool(enabled), path)?;
            batch
                .approvers
                .push(serde_yaml::from_value(Value::Mapping(value)).map_err(|error| {
                    Error::Config(format!("invalid Approver resource {}: {error}", path.display()))
                })?);
        }
    }
    Ok(())
}

fn decode_named_resource<T: for<'de> Deserialize<'de>>(
    name: String,
    enabled: bool,
    data: Value,
    path: &Path,
) -> Result<T> {
    let mut value = resource_mapping(data, path)?;
    insert_field(&mut value, "id", Value::String(name), path)?;
    insert_field(&mut value, "enabled", Value::Bool(enabled), path)?;
    serde_yaml::from_value(Value::Mapping(value))
        .map_err(|error| Error::Config(format!("invalid console resource {}: {error}", path.display())))
}

fn resource_mapping(data: Value, path: &Path) -> Result<Mapping> {
    match data {
        Value::Mapping(value) => Ok(value),
        _ => Err(Error::Config(format!(
            "console resource data must be a mapping: {}",
            path.display()
        ))),
    }
}

fn insert_field(mapping: &mut Mapping, field: &str, value: Value, path: &Path) -> Result<()> {
    if mapping.insert(Value::String(field.to_owned()), value).is_some() {
        return Err(Error::Config(format!(
            "console resource {} duplicates metadata-owned field {field}",
            path.display()
        )));
    }
    Ok(())
}

/// Converts aggregate test fixtures into the canonical one-resource-per-
/// document import format. Production import intentionally has no aggregate
/// compatibility path.
#[cfg(test)]
pub(crate) fn encode_test_resource_documents(aggregate: &serde_json::Value) -> String {
    let kinds = [
        ("clients", "Client"),
        ("wallets", "Wallet"),
        ("scenarios", "Scenario"),
        ("policies", "Policy"),
        ("offers", "Offer"),
        ("approvers", "Approver"),
    ];
    let mut documents = Vec::new();
    for (collection, kind) in kinds {
        let Some(resources) = aggregate.get(collection).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for resource in resources {
            let mut data = resource
                .as_object()
                .unwrap_or_else(|| panic!("{collection} test fixture entry must be an object"))
                .clone();
            let name = if kind == "Approver" {
                format!(
                    "{}--{}",
                    data.get("scenario_id")
                        .and_then(serde_json::Value::as_str)
                        .expect("approver scenario_id"),
                    data.get("auth_principal")
                        .and_then(serde_json::Value::as_str)
                        .expect("approver auth_principal")
                )
            } else {
                data.remove("id")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| panic!("{kind} test fixture requires id"))
            };
            let enabled = data.remove("enabled").and_then(|value| value.as_bool()).unwrap_or(true);
            let document = serde_json::json!({
                "consoleVersion": CONSOLE_VERSION,
                "kind": kind,
                "metadata": {
                    "name": name,
                    "status": if enabled { "active" } else { "disabled" }
                },
                "data": data
            });
            documents.push(serde_yaml::to_string(&document).expect("encode canonical test resource"));
        }
    }
    documents.join("---\n")
}

fn validate_batch(batch: &ImportBatch) -> Result<()> {
    unique(&batch.clients, |item| item.id.as_str(), "client id")?;
    unique(&batch.clients, |item| item.auth_principal.as_str(), "client principal")?;
    unique(&batch.wallets, |item| item.id.as_str(), "wallet id")?;
    unique(&batch.scenarios, |item| item.id.as_str(), "scenario id")?;
    unique(&batch.policies, |item| item.id.as_str(), "policy id")?;
    unique(&batch.offers, |item| item.id.as_str(), "offer id")?;
    let mut approvers = BTreeSet::new();
    for approver in &batch.approvers {
        if !approvers.insert((&approver.scenario_id, &approver.auth_principal)) {
            return Err(Error::InvalidRequest(
                "duplicate scenario approver in console import".to_owned(),
            ));
        }
    }
    for wallet in &batch.wallets {
        validate_wallet_spec(wallet)?;
    }
    for policy in &batch.policies {
        unique_sorted_strings(&policy.wallet_ids, "policy wallet id")?;
        unique_sorted_strings(&policy.protocols, "policy protocol")?;
        unique_sorted_strings(&policy.networks, "policy network")?;
        unique_sorted_strings(&policy.allowed_recipients, "policy recipient")?;
        unique_sorted_strings(&policy.allowed_origins, "policy origin")?;
        if policy.assets.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::InvalidRequest(
                "duplicate policy asset in console import".to_owned(),
            ));
        }
    }
    for offer in &batch.offers {
        unique_sorted_strings(&offer.protocols, "offer protocol")?;
    }
    Ok(())
}

fn normalize_batch(batch: &mut ImportBatch) {
    batch.clients.sort_by(|left, right| left.id.cmp(&right.id));
    batch.wallets.sort_by(|left, right| left.id.cmp(&right.id));
    batch.scenarios.sort_by(|left, right| left.id.cmp(&right.id));
    batch.policies.sort_by(|left, right| left.id.cmp(&right.id));
    batch.offers.sort_by(|left, right| left.id.cmp(&right.id));
    batch.approvers.sort_by(|left, right| {
        (&left.scenario_id, &left.auth_principal).cmp(&(&right.scenario_id, &right.auth_principal))
    });
    for policy in &mut batch.policies {
        policy.wallet_ids.sort();
        policy.protocols.sort();
        policy.networks.sort();
        policy.allowed_recipients.sort();
        policy.allowed_origins.sort();
        policy.assets.sort_by(|left, right| {
            (&left.network, &left.asset, &left.token_contract, left.decimals).cmp(&(
                &right.network,
                &right.asset,
                &right.token_contract,
                right.decimals,
            ))
        });
    }
    for offer in &mut batch.offers {
        offer.protocols.sort();
    }
}

fn unique_sorted_strings(values: &[String], description: &str) -> Result<()> {
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::InvalidRequest(format!(
            "duplicate {description} in console import"
        )));
    }
    Ok(())
}

fn unique<'a, T>(items: &'a [T], key: impl Fn(&'a T) -> &'a str, description: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    if items.iter().any(|item| !seen.insert(key(item))) {
        return Err(Error::InvalidRequest(format!(
            "duplicate {description} in console import"
        )));
    }
    Ok(())
}

fn validate_wallet_spec(wallet: &WalletSpec) -> Result<()> {
    if wallet.id.trim().is_empty() {
        return Err(Error::InvalidRequest("imported wallet id is empty".to_owned()));
    }
    match &wallet.source {
        WalletSource::Local {
            algorithm,
            private_key_env,
        } if private_key_env.trim().is_empty()
            || (!crate::model::wallet::is_evm_wallet_algorithm(algorithm)
                && !crate::model::wallet::is_solana_wallet_algorithm(algorithm)) =>
        {
            return Err(Error::InvalidRequest(
                "local wallet algorithm or private_key_env is invalid".to_owned(),
            ));
        }
        WalletSource::External { algorithm, address, .. }
            if algorithm.trim().is_empty() || address.trim().is_empty() =>
        {
            return Err(Error::InvalidRequest(
                "external wallet algorithm or address is empty".to_owned(),
            ));
        }
        _ => {}
    }
    if let Some(unit) = &wallet.budget_unit {
        unit.validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid wallet budget unit {field}")))?;
    } else if wallet.max_per_payment.is_some() || wallet.max_per_day.is_some() {
        return Err(Error::InvalidRequest("wallet limits require a budget_unit".to_owned()));
    }
    if wallet.max_per_payment.is_some_and(Amount::is_zero)
        || wallet.max_per_day.is_some_and(Amount::is_zero)
        || wallet
            .max_per_payment
            .zip(wallet.max_per_day)
            .is_some_and(|(payment, day)| payment > day)
    {
        return Err(Error::InvalidRequest("invalid wallet limits".to_owned()));
    }
    Ok(())
}

fn import_ports(storage: StoragePorts, local_keys: Arc<dyn WalletKeyManager>) -> ImportPorts {
    ImportPorts {
        clients: storage.clients,
        scenarios: storage.scenarios,
        policies: storage.policies,
        offers: storage.offers,
        approvers: storage.approvers,
        wallets: storage.wallets,
        local_keys,
    }
}

async fn apply_batch(ports: &ImportPorts, batch: ImportBatch) -> Result<ImportReport> {
    let imported_at = time::OffsetDateTime::now_utc().unix_timestamp();
    if imported_at < 0 {
        return Err(Error::InvalidTransition(
            "console import requires a non-negative UTC timestamp".to_owned(),
        ));
    }
    let mut report = ImportReport::default();

    import_clients(ports, batch.clients, imported_at, &mut report).await?;
    import_wallets(ports, batch.wallets, imported_at, &mut report).await?;
    import_scenarios(ports, batch.scenarios, imported_at, &mut report).await?;
    import_policies(ports, batch.policies, imported_at, &mut report).await?;
    import_offers(ports, batch.offers, imported_at, &mut report).await?;
    import_approvers(ports, batch.approvers, imported_at, &mut report).await?;
    Ok(report)
}

async fn import_clients(
    ports: &ImportPorts,
    specs: Vec<ClientSpec>,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentClient {
            id: spec.id,
            name: spec.name,
            auth_principal: spec.auth_principal,
            enabled: spec.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        };
        match ports.clients.get(&desired.id).await? {
            Some(existing) if same_client(&existing, &desired) => report.reused(),
            Some(_) => return Err(drift("client", &desired.id)),
            None => {
                ports.clients.insert(&desired).await?;
                report.created();
            }
        }
    }
    Ok(())
}

async fn import_wallets(
    ports: &ImportPorts,
    specs: Vec<WalletSpec>,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    for spec in specs {
        import_wallet(ports, spec, imported_at, report).await?;
    }
    Ok(())
}

async fn import_scenarios(
    ports: &ImportPorts,
    specs: Vec<ScenarioSpec>,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentScenario {
            id: spec.id,
            client_id: spec.client_id,
            name: spec.name,
            enabled: spec.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        };
        match ports.scenarios.get(&desired.id).await? {
            Some(existing) if same_scenario(&existing, &desired) => report.reused(),
            Some(_) => return Err(drift("scenario", &desired.id)),
            None => {
                ports.scenarios.register(&desired).await?;
                report.created();
            }
        }
    }
    Ok(())
}

async fn import_policies(
    ports: &ImportPorts,
    specs: Vec<PolicySpec>,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentPolicy {
            id: spec.id,
            scenario_id: spec.scenario_id,
            wallet_ids: spec.wallet_ids,
            protocols: spec.protocols,
            networks: spec.networks,
            assets: spec.assets,
            budget_unit: spec.budget_unit,
            allowed_recipients: spec.allowed_recipients,
            allowed_origins: spec.allowed_origins,
            inbound_limits: spec.inbound_limits,
            outbound_limits: spec.outbound_limits,
            enabled: spec.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        };
        desired
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid imported policy {field}")))?;
        match ports.policies.get(&desired.id).await? {
            Some(existing) if same_policy(&existing, &desired) => report.reused(),
            Some(_) => return Err(drift("policy", &desired.id)),
            None => {
                ports.policies.insert(&desired).await?;
                report.created();
            }
        }
    }
    Ok(())
}

async fn import_offers(
    ports: &ImportPorts,
    specs: Vec<OfferSpec>,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentOffer {
            id: spec.id,
            scenario_id: spec.scenario_id,
            policy_id: spec.policy_id,
            resource: spec.resource,
            protocols: spec.protocols,
            asset: spec.asset,
            amount: spec.amount,
            wallet_id: spec.wallet_id,
            enabled: spec.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        };
        desired
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid imported offer {field}")))?;
        match ports.offers.get(&desired.id).await? {
            Some(existing) if same_offer(&existing, &desired) => report.reused(),
            Some(_) => return Err(drift("offer", &desired.id)),
            None => {
                ports.offers.insert(&desired).await?;
                report.created();
            }
        }
    }
    Ok(())
}

async fn import_approvers(
    ports: &ImportPorts,
    specs: Vec<ApproverSpec>,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentScenarioApprover {
            scenario_id: spec.scenario_id,
            auth_principal: spec.auth_principal,
            enabled: spec.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        };
        match ports
            .approvers
            .get(&desired.scenario_id, &desired.auth_principal)
            .await?
        {
            Some(existing) if existing.enabled == desired.enabled => report.reused(),
            Some(_) => {
                return Err(drift(
                    "scenario approver",
                    &format!("{}/{}", desired.scenario_id, desired.auth_principal),
                ));
            }
            None => {
                ports.approvers.upsert(&desired).await?;
                report.created();
            }
        }
    }
    Ok(())
}

async fn import_wallet(
    ports: &ImportPorts,
    spec: WalletSpec,
    imported_at: i64,
    report: &mut ImportReport,
) -> Result<()> {
    let (expected, private_key) = wallet_material(&spec, imported_at)?;
    if let Some(existing) = ports.wallets.get_payment_wallet(&spec.id).await? {
        if !same_wallet(&existing.wallet, &expected)
            || existing.custody != wallet_custody(&spec.source)
            || existing.enabled != spec.enabled
            || existing.budget_unit != spec.budget_unit
            || existing.max_per_payment != spec.max_per_payment
            || existing.max_per_day != spec.max_per_day
            || (matches!(&spec.source, WalletSource::Local { .. }) && existing.active_key_version.is_none())
        {
            return Err(drift("wallet", &spec.id));
        }
        report.reused();
        return Ok(());
    }

    let config = WalletCreationConfig {
        enabled: spec.enabled,
        budget_unit: spec.budget_unit.clone(),
        max_per_payment: spec.max_per_payment,
        max_per_day: spec.max_per_day,
    };
    match &spec.source {
        WalletSource::Local { algorithm, .. } => {
            let private_key = private_key
                .ok_or_else(|| Error::InvalidTransition("local wallet import key is unavailable".to_owned()))?;
            ports
                .local_keys
                .create_local(&spec.id, algorithm, &private_key, &config, imported_at)
                .await?;
        }
        WalletSource::External { .. } => {
            ports.wallets.create_external_configured(&expected, &config).await?;
        }
    }
    report.created();
    Ok(())
}

fn wallet_material(spec: &WalletSpec, created_at: i64) -> Result<(Wallet, Option<Zeroizing<Vec<u8>>>)> {
    match &spec.source {
        WalletSource::Local {
            algorithm,
            private_key_env,
        } => {
            let private_key = private_key_from_env(private_key_env, algorithm)?;
            let (address, public_key) = crypto::payment_key_metadata(algorithm, &private_key)?;
            let wallet = Wallet {
                id: spec.id.clone(),
                algorithm: algorithm.clone(),
                address,
                public_key,
                created_at,
            };
            Ok((wallet, Some(private_key)))
        }
        WalletSource::External {
            algorithm,
            address,
            public_key,
        } => {
            let wallet = Wallet {
                id: spec.id.clone(),
                algorithm: algorithm.clone(),
                address: address.clone(),
                public_key: public_key.clone(),
                created_at,
            }
            .canonicalized()
            .map_err(|field| Error::InvalidRequest(format!("invalid external wallet {field}")))?;
            Ok((wallet, None))
        }
    }
}

fn private_key_from_env(variable: &str, algorithm: &str) -> Result<Zeroizing<Vec<u8>>> {
    let encoded = read_secret_env(variable.trim(), "imported wallet private key", 1024)?;
    let encoded = encoded.strip_prefix("0x").unwrap_or(encoded.as_str());
    let decoded = Zeroizing::new(
        hex::decode(encoded).map_err(|_| Error::InvalidKey(format!("private key in {variable} is not hexadecimal")))?,
    );
    crypto::validate_private_key_for_algorithm(algorithm, &decoded)?;
    Ok(decoded)
}

const fn wallet_custody(source: &WalletSource) -> WalletCustody {
    match source {
        WalletSource::Local { .. } => WalletCustody::Local,
        WalletSource::External { .. } => WalletCustody::External,
    }
}

fn same_client(left: &PaymentClient, right: &PaymentClient) -> bool {
    left.id == right.id
        && left.name == right.name
        && left.auth_principal == right.auth_principal
        && left.enabled == right.enabled
}

fn same_scenario(left: &PaymentScenario, right: &PaymentScenario) -> bool {
    left.id == right.id && left.client_id == right.client_id && left.name == right.name && left.enabled == right.enabled
}

fn same_wallet(left: &Wallet, right: &Wallet) -> bool {
    left.id == right.id
        && left.algorithm == right.algorithm
        && left.address == right.address
        && left.public_key == right.public_key
}

fn same_policy(left: &PaymentPolicy, right: &PaymentPolicy) -> bool {
    let mut normalized = right.clone();
    normalized.created_at = left.created_at;
    normalized.updated_at = left.updated_at;
    left == &normalized
}

fn same_offer(left: &PaymentOffer, right: &PaymentOffer) -> bool {
    let mut normalized = right.clone();
    normalized.created_at = left.created_at;
    normalized.updated_at = left.updated_at;
    left == &normalized
}

fn drift(kind: &str, id: &str) -> Error {
    Error::PaymentConflict(format!(
        "imported {kind} {id} exists with different authoritative content"
    ))
}

#[cfg(test)]
#[path = "../../tests/handler/resource_import_test.rs"]
mod tests;
