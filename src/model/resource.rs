use serde::{de::Error as _, Deserialize, Deserializer};
use serde_yaml::Value;

use super::{
    domain::{PaymentClient, PaymentOffer, PaymentPolicy, PaymentScenario, PaymentScenarioApprover, Wallet},
    wallet::EVM_EOA_ALGORITHM,
    Amount, AssetIdentity, BudgetUnit, PaymentPolicyLimits,
};

#[derive(Default)]
pub(crate) struct ImportBatch {
    pub(crate) clients: Vec<ClientSpec>,
    pub(crate) wallets: Vec<WalletSpec>,
    pub(crate) scenarios: Vec<ScenarioSpec>,
    pub(crate) policies: Vec<PolicySpec>,
    pub(crate) offers: Vec<OfferSpec>,
    pub(crate) approvers: Vec<ApproverSpec>,
}

impl ImportBatch {
    pub(crate) fn contains_only_wallets(&self) -> bool {
        !self.wallets.is_empty()
            && self.clients.is_empty()
            && self.scenarios.is_empty()
            && self.policies.is_empty()
            && self.offers.is_empty()
            && self.approvers.is_empty()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceEnvelope {
    #[serde(rename = "consoleVersion")]
    pub(crate) console_version: String,
    pub(crate) kind: ResourceKind,
    pub(crate) metadata: ResourceMetadata,
    pub(crate) data: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceMetadata {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) status: ResourceStatus,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceStatus {
    #[default]
    Active,
    Disabled,
}

impl ResourceStatus {
    pub(crate) const fn enabled(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Clone, Copy, Deserialize)]
pub(crate) enum ResourceKind {
    Client,
    Wallet,
    Scenario,
    Policy,
    Offer,
    Approver,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientSpec {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) auth_principal: String,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

impl ClientSpec {
    pub(crate) fn into_domain(self, imported_at: i64) -> PaymentClient {
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
pub(crate) struct ScenarioSpec {
    pub(crate) id: String,
    pub(crate) client_id: String,
    pub(crate) name: String,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

impl ScenarioSpec {
    pub(crate) fn into_domain(self, imported_at: i64) -> PaymentScenario {
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

pub(crate) struct WalletSpec {
    pub(crate) id: String,
    pub(crate) enabled: bool,
    pub(crate) budget_unit: Option<BudgetUnit>,
    pub(crate) max_per_payment: Option<Amount>,
    pub(crate) max_per_day: Option<Amount>,
    pub(crate) source: WalletSource,
}

pub(crate) enum WalletSource {
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
    pub(crate) fn domain_wallet(&self, address: String, public_key: String, created_at: i64) -> Wallet {
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
pub(crate) struct PolicySpec {
    pub(crate) id: String,
    pub(crate) scenario_id: String,
    pub(crate) wallet_ids: Vec<String>,
    pub(crate) protocols: Vec<String>,
    pub(crate) networks: Vec<String>,
    pub(crate) assets: Vec<AssetIdentity>,
    pub(crate) budget_unit: BudgetUnit,
    #[serde(default)]
    pub(crate) allowed_recipients: Vec<String>,
    #[serde(default)]
    pub(crate) allowed_origins: Vec<String>,
    #[serde(default)]
    pub(crate) inbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default)]
    pub(crate) outbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

impl PolicySpec {
    pub(crate) fn into_domain(self, imported_at: i64) -> PaymentPolicy {
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
pub(crate) struct OfferSpec {
    pub(crate) id: String,
    pub(crate) scenario_id: String,
    pub(crate) policy_id: String,
    pub(crate) resource: String,
    pub(crate) protocols: Vec<String>,
    pub(crate) asset: AssetIdentity,
    pub(crate) amount: Amount,
    pub(crate) wallet_id: String,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

impl OfferSpec {
    pub(crate) fn into_domain(self, imported_at: i64) -> PaymentOffer {
        PaymentOffer {
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
pub(crate) struct ApproverSpec {
    pub(crate) scenario_id: String,
    pub(crate) auth_principal: String,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

impl ApproverSpec {
    pub(crate) fn into_domain(self, imported_at: i64) -> PaymentScenarioApprover {
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
    pub(crate) fn created(&mut self) {
        self.created += 1;
    }

    pub(crate) fn reused(&mut self) {
        self.reused += 1;
    }
}

#[cfg(test)]
#[path = "../../tests/model/resource_test.rs"]
mod tests;
