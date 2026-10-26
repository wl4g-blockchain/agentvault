use serde::{Deserialize, Serialize};

use crate::model::{payment_recipients_match, Amount, AssetIdentity, BudgetUnit, PaymentDirection};

/// One direction's authoritative scenario budget persisted with a policy.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentPolicyLimits {
    pub max_per_payment: Amount,
    pub max_per_day: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_threshold: Option<Amount>,
    #[serde(default = "PaymentPolicyLimits::default_true")]
    pub approval_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_overage: Option<Amount>,
}

impl PaymentPolicyLimits {
    const fn default_true() -> bool {
        true
    }
}

impl PaymentPolicyLimits {
    /// Validates scalar limit relations.
    ///
    /// # Errors
    ///
    /// Returns the invalid limit name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_per_payment.is_zero() {
            return Err("max_per_payment");
        }
        if self.max_per_day.is_zero() || self.max_per_payment > self.max_per_day {
            return Err("max_per_day");
        }
        if self.approval_threshold.is_some_and(Amount::is_zero) {
            return Err("approval_threshold");
        }
        if self.approval_overage.is_some_and(Amount::is_zero)
            || (!self.approval_enabled && self.approval_overage.is_some())
            || self.hard_daily_limit().is_none()
        {
            return Err("approval_overage");
        }
        Ok(())
    }

    #[must_use]
    pub fn hard_daily_limit(&self) -> Option<Amount> {
        self.max_per_day
            .checked_add(self.approval_overage.unwrap_or(Amount::ZERO))
    }

    #[must_use]
    pub fn requires_approval(&self, amount: Amount) -> bool {
        self.approval_enabled && self.approval_threshold.is_some_and(|threshold| amount >= threshold)
    }
}

crate::storage::repository_entity! { "pay_policy", ReadOnly;
    keys: ["id"], immutable: ["created_at"], revision: None;
    /// Scenario policy aggregate. Wallet membership is persisted by the
    /// `pay_policy_wallet` relation and represented here as a deterministic list.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentPolicy {
        pub id: String,
        pub scenario_id: String,
        pub wallet_ids: Vec<String>,
        pub protocols: Vec<String>,
        pub networks: Vec<String>,
        pub assets: Vec<AssetIdentity>,
        pub budget_unit: BudgetUnit,
        pub allowed_recipients: Vec<String>,
        pub allowed_origins: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub inbound_limits: Option<PaymentPolicyLimits>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub outbound_limits: Option<PaymentPolicyLimits>,
        pub enabled: bool,
        pub created_at: i64,
        pub updated_at: i64,
    }
}

impl PaymentPolicy {
    #[must_use]
    pub fn allows_wallet(&self, wallet_id: &str) -> bool {
        self.enabled && self.wallet_ids.iter().any(|allowed| allowed == wallet_id)
    }

    #[must_use]
    pub const fn limits_for(&self, direction: PaymentDirection) -> Option<&PaymentPolicyLimits> {
        match direction {
            PaymentDirection::Inbound => self.inbound_limits.as_ref(),
            PaymentDirection::Outbound => self.outbound_limits.as_ref(),
        }
    }

    #[must_use]
    pub fn allows_direction(&self, direction: PaymentDirection) -> bool {
        self.enabled && self.limits_for(direction).is_some()
    }

    #[must_use]
    pub fn allows_protocol(&self, protocol: &str) -> bool {
        self.enabled && self.protocols.iter().any(|allowed| allowed == protocol)
    }

    #[must_use]
    pub fn allows_asset(&self, asset: &AssetIdentity) -> bool {
        self.enabled
            && self.networks.iter().any(|network| network == &asset.network)
            && self.assets.iter().any(|allowed| allowed == asset)
    }

    #[must_use]
    pub fn allows_recipient(&self, recipient: &str) -> bool {
        self.enabled && self.allowed_recipients.iter().any(|allowed| allowed == recipient)
    }

    #[must_use]
    pub fn allows_payment_recipient(&self, network: &str, recipient: &str) -> bool {
        self.enabled
            && self
                .allowed_recipients
                .iter()
                .any(|allowed| payment_recipients_match(network, allowed, recipient))
    }

    #[must_use]
    pub fn allows_origin(&self, origin: &str) -> bool {
        self.enabled && self.allowed_origins.iter().any(|allowed| allowed == origin)
    }

    #[must_use]
    pub fn requires_approval(&self, direction: PaymentDirection, amount: Amount) -> bool {
        self.limits_for(direction)
            .is_some_and(|limits| limits.requires_approval(amount))
    }

    /// Validates policy invariants before persistence or authorization.
    ///
    /// # Errors
    ///
    /// Returns the invalid field or limit relation.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.scenario_id.trim().is_empty() {
            return Err("scenario_id");
        }
        if self.wallet_ids.is_empty()
            || self.wallet_ids.iter().any(|wallet_id| wallet_id.trim().is_empty())
            || self.wallet_ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("wallet_ids");
        }
        if self.inbound_limits.is_none() && self.outbound_limits.is_none() {
            return Err("direction_limits");
        }
        if self
            .inbound_limits
            .as_ref()
            .is_some_and(|limits| limits.validate().is_err())
        {
            return Err("inbound_limits");
        }
        if self
            .outbound_limits
            .as_ref()
            .is_some_and(|limits| limits.validate().is_err())
        {
            return Err("outbound_limits");
        }
        self.budget_unit.validate().map_err(|_| "budget_unit")?;
        if self.assets.iter().any(|asset| asset.validate().is_err())
            || self.assets.iter().any(|asset| !self.budget_unit.matches(asset))
        {
            return Err("assets");
        }
        if self.protocols.iter().any(|protocol| protocol.trim().is_empty()) {
            return Err("protocols");
        }
        if self.networks.iter().any(|network| network.trim().is_empty()) {
            return Err("networks");
        }
        if self
            .allowed_recipients
            .iter()
            .any(|recipient| recipient.trim().is_empty())
        {
            return Err("allowed_recipients");
        }
        if self.allowed_origins.iter().any(|origin| origin.trim().is_empty()) {
            return Err("allowed_origins");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/model/domain/policy_test.rs"]
mod tests;
