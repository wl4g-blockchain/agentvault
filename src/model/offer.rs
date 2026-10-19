use serde::{Deserialize, Serialize};

use super::payment::{Amount, AssetIdentity};

/// Fixed-price offer for one HTTP route or MCP tool.
///
/// `wallet_id` resolves the recipient server-side. A caller cannot override the
/// receiving address when creating a challenge.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentOffer {
    pub id: String,
    pub scenario_id: String,
    /// Inbound-capable policy authorizing this offer and recipient wallet.
    pub policy_id: String,
    pub resource: String,
    pub protocols: Vec<String>,
    pub asset: AssetIdentity,
    pub amount: Amount,
    pub wallet_id: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PaymentOffer {
    #[must_use]
    pub fn supports_protocol(&self, protocol: &str) -> bool {
        self.enabled && self.protocols.iter().any(|supported| supported == protocol)
    }

    /// Validates immutable pricing and recipient references.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.scenario_id.trim().is_empty() {
            return Err("scenario_id");
        }
        if self.policy_id.trim().is_empty() {
            return Err("policy_id");
        }
        if self.resource.trim().is_empty() {
            return Err("resource");
        }
        if self.protocols.is_empty()
            || self.protocols.iter().any(|protocol| protocol.trim().is_empty())
            || self
                .protocols
                .iter()
                .enumerate()
                .any(|(index, protocol)| self.protocols[..index].contains(protocol))
        {
            return Err("protocols");
        }
        self.asset.validate()?;
        if self.amount.is_zero() {
            return Err("amount");
        }
        if self.wallet_id.trim().is_empty() {
            return Err("wallet_id");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/model/offer_test.rs"]
mod tests;
