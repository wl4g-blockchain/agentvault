//! Idempotent provisioning for operator-owned payment identities and policy.
//!
//! The manifest contains only public metadata and environment-variable names.
//! Local private keys are resolved at execution time, zeroized in memory, and
//! persisted only through the versioned SQL envelope-encryption repositories.

use std::{
    collections::BTreeSet, fs::OpenOptions, io::Read as _, os::unix::fs::OpenOptionsExt as _, path::Path, sync::Arc,
};

use serde::{de::Error as _, Deserialize, Deserializer};
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
        WalletCustody, WalletKeyManager, WalletProvisioningConfig, WalletStorage,
    },
    utils::{crypto, secret::read_secret_env},
    Error, Result,
};

const MANIFEST_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BODY_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionManifest {
    version: u32,
    #[serde(default)]
    clients: Vec<ClientSpec>,
    #[serde(default)]
    wallets: Vec<WalletSpec>,
    #[serde(default)]
    scenarios: Vec<ScenarioSpec>,
    #[serde(default)]
    policies: Vec<PolicySpec>,
    #[serde(default)]
    offers: Vec<OfferSpec>,
    #[serde(default)]
    approvers: Vec<ApproverSpec>,
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

/// Counts produced by one idempotent provisioning run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProvisioningReport {
    pub created: usize,
    pub reused: usize,
}

impl ProvisioningReport {
    fn created(&mut self) {
        self.created += 1;
    }

    fn reused(&mut self) {
        self.reused += 1;
    }
}

struct ProvisioningPorts {
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    policies: Arc<dyn PolicyStorage>,
    offers: Arc<dyn OfferStorage>,
    approvers: Arc<dyn ApproverStorage>,
    wallets: Arc<dyn WalletStorage>,
    local_keys: Arc<dyn WalletKeyManager>,
}

/// Applies one versioned JSON manifest to the configured SQL authority.
///
/// Existing records are reused only when their immutable and effective
/// configuration exactly matches the manifest. Drift fails closed instead of
/// silently mutating payment authority.
///
/// # Errors
///
/// Fails for an unsafe manifest file, invalid/duplicate resources, unavailable
/// environment secrets, configuration drift, or any SQL transaction failure.
pub async fn provision_from_path(config: &AppConfig, manifest_path: impl AsRef<Path>) -> Result<ProvisioningReport> {
    let mut manifest = read_manifest(manifest_path.as_ref())?;
    normalize_manifest(&mut manifest);
    validate_manifest(&manifest)?;
    let factory = StorageFactory::new();
    let result = async {
        let storage = factory.create(&config.storage).await?;
        let local_keys = Arc::clone(&storage).wallet_key_manager(&config.agentvault.key_management)?;
        let ports = provisioning_ports(Arc::clone(&storage).ports(), local_keys);
        apply_manifest(&ports, manifest).await
    }
    .await;
    let close = factory.shutdown_all().await;
    match result {
        Err(error) => Err(error),
        Ok(report) => {
            close?;
            Ok(report)
        }
    }
}

fn read_manifest(path: &Path) -> Result<ProvisionManifest> {
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
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Config(
            "provisioning manifest must be a regular file no larger than 1 MiB".to_owned(),
        ));
    }
    let mut body = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(MAX_MANIFEST_BODY_BYTES));
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    if body.len() > MAX_MANIFEST_BODY_BYTES {
        return Err(Error::Config(
            "provisioning manifest changed while reading or exceeds 1 MiB".to_owned(),
        ));
    }
    serde_json::from_slice(&body).map_err(Error::from)
}

fn validate_manifest(manifest: &ProvisionManifest) -> Result<()> {
    if manifest.version != MANIFEST_VERSION {
        return Err(Error::Config(format!(
            "unsupported provisioning manifest version {}",
            manifest.version
        )));
    }
    unique(&manifest.clients, |item| item.id.as_str(), "client id")?;
    unique(
        &manifest.clients,
        |item| item.auth_principal.as_str(),
        "client principal",
    )?;
    unique(&manifest.wallets, |item| item.id.as_str(), "wallet id")?;
    unique(&manifest.scenarios, |item| item.id.as_str(), "scenario id")?;
    unique(&manifest.policies, |item| item.id.as_str(), "policy id")?;
    unique(&manifest.offers, |item| item.id.as_str(), "offer id")?;
    let mut approvers = BTreeSet::new();
    for approver in &manifest.approvers {
        if !approvers.insert((&approver.scenario_id, &approver.auth_principal)) {
            return Err(Error::InvalidRequest(
                "duplicate scenario approver in provisioning manifest".to_owned(),
            ));
        }
    }
    for wallet in &manifest.wallets {
        validate_wallet_spec(wallet)?;
    }
    for policy in &manifest.policies {
        unique_sorted_strings(&policy.wallet_ids, "policy wallet id")?;
        unique_sorted_strings(&policy.protocols, "policy protocol")?;
        unique_sorted_strings(&policy.networks, "policy network")?;
        unique_sorted_strings(&policy.allowed_recipients, "policy recipient")?;
        unique_sorted_strings(&policy.allowed_origins, "policy origin")?;
        if policy.assets.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::InvalidRequest(
                "duplicate policy asset in provisioning manifest".to_owned(),
            ));
        }
    }
    for offer in &manifest.offers {
        unique_sorted_strings(&offer.protocols, "offer protocol")?;
    }
    Ok(())
}

fn normalize_manifest(manifest: &mut ProvisionManifest) {
    manifest.clients.sort_by(|left, right| left.id.cmp(&right.id));
    manifest.wallets.sort_by(|left, right| left.id.cmp(&right.id));
    manifest.scenarios.sort_by(|left, right| left.id.cmp(&right.id));
    manifest.policies.sort_by(|left, right| left.id.cmp(&right.id));
    manifest.offers.sort_by(|left, right| left.id.cmp(&right.id));
    manifest.approvers.sort_by(|left, right| {
        (&left.scenario_id, &left.auth_principal).cmp(&(&right.scenario_id, &right.auth_principal))
    });
    for policy in &mut manifest.policies {
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
    for offer in &mut manifest.offers {
        offer.protocols.sort();
    }
}

fn unique_sorted_strings(values: &[String], description: &str) -> Result<()> {
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::InvalidRequest(format!(
            "duplicate {description} in provisioning manifest"
        )));
    }
    Ok(())
}

fn unique<'a, T>(items: &'a [T], key: impl Fn(&'a T) -> &'a str, description: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    if items.iter().any(|item| !seen.insert(key(item))) {
        return Err(Error::InvalidRequest(format!(
            "duplicate {description} in provisioning manifest"
        )));
    }
    Ok(())
}

fn validate_wallet_spec(wallet: &WalletSpec) -> Result<()> {
    if wallet.id.trim().is_empty() {
        return Err(Error::InvalidRequest("provisioning wallet id is empty".to_owned()));
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

fn provisioning_ports(storage: StoragePorts, local_keys: Arc<dyn WalletKeyManager>) -> ProvisioningPorts {
    ProvisioningPorts {
        clients: storage.clients,
        scenarios: storage.scenarios,
        policies: storage.policies,
        offers: storage.offers,
        approvers: storage.approvers,
        wallets: storage.wallets,
        local_keys,
    }
}

async fn apply_manifest(ports: &ProvisioningPorts, manifest: ProvisionManifest) -> Result<ProvisioningReport> {
    let provisioned_at = time::OffsetDateTime::now_utc().unix_timestamp();
    if provisioned_at < 0 {
        return Err(Error::InvalidTransition(
            "provisioning requires a non-negative UTC timestamp".to_owned(),
        ));
    }
    let mut report = ProvisioningReport::default();

    provision_clients(ports, manifest.clients, provisioned_at, &mut report).await?;
    provision_wallets(ports, manifest.wallets, provisioned_at, &mut report).await?;
    provision_scenarios(ports, manifest.scenarios, provisioned_at, &mut report).await?;
    provision_policies(ports, manifest.policies, provisioned_at, &mut report).await?;
    provision_offers(ports, manifest.offers, provisioned_at, &mut report).await?;
    provision_approvers(ports, manifest.approvers, provisioned_at, &mut report).await?;
    Ok(report)
}

async fn provision_clients(
    ports: &ProvisioningPorts,
    specs: Vec<ClientSpec>,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentClient {
            id: spec.id,
            name: spec.name,
            auth_principal: spec.auth_principal,
            enabled: spec.enabled,
            created_at: provisioned_at,
            updated_at: provisioned_at,
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

async fn provision_wallets(
    ports: &ProvisioningPorts,
    specs: Vec<WalletSpec>,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
) -> Result<()> {
    for spec in specs {
        provision_wallet(ports, spec, provisioned_at, report).await?;
    }
    Ok(())
}

async fn provision_scenarios(
    ports: &ProvisioningPorts,
    specs: Vec<ScenarioSpec>,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentScenario {
            id: spec.id,
            client_id: spec.client_id,
            name: spec.name,
            enabled: spec.enabled,
            created_at: provisioned_at,
            updated_at: provisioned_at,
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

async fn provision_policies(
    ports: &ProvisioningPorts,
    specs: Vec<PolicySpec>,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
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
            created_at: provisioned_at,
            updated_at: provisioned_at,
        };
        desired
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid provisioned policy {field}")))?;
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

async fn provision_offers(
    ports: &ProvisioningPorts,
    specs: Vec<OfferSpec>,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
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
            created_at: provisioned_at,
            updated_at: provisioned_at,
        };
        desired
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid provisioned offer {field}")))?;
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

async fn provision_approvers(
    ports: &ProvisioningPorts,
    specs: Vec<ApproverSpec>,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
) -> Result<()> {
    for spec in specs {
        let desired = PaymentScenarioApprover {
            scenario_id: spec.scenario_id,
            auth_principal: spec.auth_principal,
            enabled: spec.enabled,
            created_at: provisioned_at,
            updated_at: provisioned_at,
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

async fn provision_wallet(
    ports: &ProvisioningPorts,
    spec: WalletSpec,
    provisioned_at: i64,
    report: &mut ProvisioningReport,
) -> Result<()> {
    let (expected, private_key) = wallet_material(&spec, provisioned_at)?;
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

    let config = WalletProvisioningConfig {
        enabled: spec.enabled,
        budget_unit: spec.budget_unit.clone(),
        max_per_payment: spec.max_per_payment,
        max_per_day: spec.max_per_day,
    };
    match &spec.source {
        WalletSource::Local { algorithm, .. } => {
            let private_key = private_key
                .ok_or_else(|| Error::InvalidTransition("local wallet provisioning key is unavailable".to_owned()))?;
            ports
                .local_keys
                .create_local(&spec.id, algorithm, &private_key, &config, provisioned_at)
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
    let encoded = read_secret_env(variable.trim(), "provisioned wallet private key", 1024)?;
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
        "provisioned {kind} {id} exists with different authoritative content"
    ))
}

#[cfg(test)]
#[path = "../../tests/handler/provisioning_test.rs"]
mod tests;
