use serde::{de::Error as _, Deserialize, Deserializer};
use serde_yaml::Value;

use super::{
    domain::PaymentClient, domain::PaymentPolicy, domain::PaymentPrice, domain::PaymentScenario,
    domain::PaymentScenarioApprover, domain::Wallet, wallet::EVM_EOA_ALGORITHM, Amount, AssetIdentity, BudgetUnit,
    PaymentPolicyLimits,
};

#[derive(Default)]
pub struct ImportBatch {
    pub clients: Vec<ClientSpec>,
    pub wallets: Vec<WalletSpec>,
    pub scenarios: Vec<ScenarioSpec>,
    pub policies: Vec<PolicySpec>,
    pub prices: Vec<PriceSpec>,
    pub approvers: Vec<ApproverSpec>,
}

impl ImportBatch {
    #[must_use]
    pub fn contains_only_wallets(&self) -> bool {
        !self.wallets.is_empty()
            && self.clients.is_empty()
            && self.scenarios.is_empty()
            && self.policies.is_empty()
            && self.prices.is_empty()
            && self.approvers.is_empty()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceEnvelope {
    #[serde(rename = "consoleVersion")]
    pub console_version: String,
    pub kind: ResourceKind,
    pub metadata: ResourceMetadata,
    pub data: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceMetadata {
    pub name: String,
    #[serde(default)]
    pub status: ResourceStatus,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStatus {
    #[default]
    Active,
    Disabled,
}

impl ResourceStatus {
    #[must_use]
    pub const fn enabled(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Clone, Copy, Deserialize)]
pub enum ResourceKind {
    Client,
    Wallet,
    Scenario,
    Policy,
    Price,
    Approver,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSpec {
    pub id: String,
    pub name: String,
    pub auth_principal: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ClientSpec {
    #[must_use]
    pub fn into_domain(self, imported_at: i64) -> PaymentClient {
        PaymentClient {
            id: self.id,
            name: self.name,
            auth_principal: self.auth_principal,
            enabled: self.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    pub id: String,
    pub client_id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ScenarioSpec {
    #[must_use]
    pub fn into_domain(self, imported_at: i64) -> PaymentScenario {
        PaymentScenario {
            id: self.id,
            client_id: self.client_id,
            name: self.name,
            enabled: self.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        }
    }
}

pub struct WalletSpec {
    pub id: String,
    pub enabled: bool,
    pub budget_unit: Option<BudgetUnit>,
    pub max_per_payment: Option<Amount>,
    pub max_per_day: Option<Amount>,
    pub source: WalletSource,
}

pub enum WalletSource {
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

impl WalletSpec {
    #[must_use]
    pub fn domain_wallet(&self, address: String, public_key: String, created_at: i64) -> Wallet {
        let algorithm = match &self.source {
            WalletSource::Local { algorithm, .. } | WalletSource::External { algorithm, .. } => algorithm.clone(),
        };
        Wallet {
            id: self.id.clone(),
            algorithm,
            address,
            public_key,
            created_at,
        }
    }
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
pub struct PolicySpec {
    pub id: String,
    pub scenario_id: String,
    pub wallet_ids: Vec<String>,
    pub protocols: Vec<String>,
    pub networks: Vec<String>,
    pub assets: Vec<AssetIdentity>,
    pub budget_unit: BudgetUnit,
    #[serde(default)]
    pub allowed_recipients: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub inbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default)]
    pub outbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl PolicySpec {
    #[must_use]
    pub fn into_domain(self, imported_at: i64) -> PaymentPolicy {
        PaymentPolicy {
            id: self.id,
            scenario_id: self.scenario_id,
            wallet_ids: self.wallet_ids,
            protocols: self.protocols,
            networks: self.networks,
            assets: self.assets,
            budget_unit: self.budget_unit,
            allowed_recipients: self.allowed_recipients,
            allowed_origins: self.allowed_origins,
            inbound_limits: self.inbound_limits,
            outbound_limits: self.outbound_limits,
            enabled: self.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceSpec {
    pub id: String,
    pub scenario_id: String,
    pub policy_id: String,
    pub resource: String,
    pub protocols: Vec<String>,
    pub asset: AssetIdentity,
    pub amount: Amount,
    pub wallet_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl PriceSpec {
    #[must_use]
    pub fn into_domain(self, imported_at: i64) -> PaymentPrice {
        PaymentPrice {
            id: self.id,
            scenario_id: self.scenario_id,
            policy_id: self.policy_id,
            resource: self.resource,
            protocols: self.protocols,
            asset: self.asset,
            amount: self.amount,
            wallet_id: self.wallet_id,
            enabled: self.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproverSpec {
    pub scenario_id: String,
    pub auth_principal: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ApproverSpec {
    #[must_use]
    pub fn into_domain(self, imported_at: i64) -> PaymentScenarioApprover {
        PaymentScenarioApprover {
            scenario_id: self.scenario_id,
            auth_principal: self.auth_principal,
            enabled: self.enabled,
            created_at: imported_at,
            updated_at: imported_at,
        }
    }
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
    pub fn created(&mut self) {
        self.created += 1;
    }

    pub fn reused(&mut self) {
        self.reused += 1;
    }
}

#[cfg(test)]
#[path = "../../tests/model/resource_test.rs"]
mod tests;
