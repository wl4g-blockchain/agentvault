//! Price administration requests reuse the resource-import price definition.
use crate::{model::resource::PriceSpec, model::PaymentPrice, Error, Result};
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
