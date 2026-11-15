//! Price administration requests reuse the resource-import price definition.
use crate::{
    model::{Amount, AssetIdentity},
    Error, Result,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(transparent)]
pub struct CreatePriceRequest(pub PriceSpec);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePriceRequest {
    pub price: PriceSpec,
    pub expected_updated_at: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletePriceRequest {
    pub expected_updated_at: i64,
}

impl UpdatePriceRequest {
    ///
    /// # Errors
    /// Returns an invalid-request error when the update changes immutable price identity.
    pub fn into_domain(self, id: &str, stored: &PaymentPrice, now: i64) -> Result<(PaymentPrice, i64)> {
        if self.price.id != id
            || self.price.id != stored.id
            || self.price.scenario_id != stored.scenario_id
            || self.price.policy_id != stored.policy_id
            || self.expected_updated_at < 0
        {
            return Err(Error::InvalidRequest(
                "price identity and ownership are immutable".to_owned(),
            ));
        }
        let next_revision = stored
            .updated_at
            .checked_add(1)
            .ok_or_else(|| Error::conflict("price revision exhausted"))?;
        let mut price = self.price.into_domain(now.max(next_revision));
        price.created_at = stored.created_at;
        Ok((price, self.expected_updated_at))
    }
}

#[cfg(test)]
#[path = "../../tests/model/price_test.rs"]
mod tests;

mod entity {
    use serde::{Deserialize, Serialize};

    use crate::{model::Amount, model::AssetIdentity};

    crate::storage::repository_entity! { "pay_price", Catalog;
        keys: ["id"], immutable: ["created_at"], revision: Some("updated_at");
        /// Payee resource price for one HTTP route or MCP tool.
        ///
        /// `wallet_id` resolves the recipient server-side. A caller cannot override the
        /// receiving address when creating a challenge.
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
        #[serde(deny_unknown_fields)]
        pub struct PaymentPrice {
            pub id: String,
            pub scenario_id: String,
            /// Inbound-capable policy authorizing this price and recipient wallet.
            pub policy_id: String,
            pub resource: String,
            #[sqlx(json)]
            pub protocols: Vec<String>,
            #[sqlx(flatten)]
            pub asset: AssetIdentity,
            pub amount: Amount,
            pub wallet_id: String,
            pub enabled: bool,
            pub created_at: i64,
            pub updated_at: i64,
        }
    }

    impl PaymentPrice {
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
    mod tests {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/model/price_entity_test.rs"));
    }
}
pub use entity::*;

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
    #[serde(default = "PriceSpec::default_enabled")]
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
impl PriceSpec {
    const fn default_enabled() -> bool {
        true
    }
}
