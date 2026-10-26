use crate::{
    handler::ScenarioHandler, model::PaymentChallenge, model::PaymentPrice, protocol::ProtocolRegistry,
    storage::IPolicyRepository, storage::IPriceRepository, storage::IWalletRepository, Error, Result,
};
use std::sync::Arc;

/// Payee price resolution. Pricing and recipient address always come from
/// server-managed records, never request overrides.
pub struct PriceResolver {
    scenarios: Arc<ScenarioHandler>,
    prices: Arc<dyn IPriceRepository>,
    policies: Arc<dyn IPolicyRepository>,
    wallets: Arc<dyn IWalletRepository>,
    protocols: Arc<ProtocolRegistry>,
}

impl PriceResolver {
    #[must_use]
    pub fn new(
        scenarios: Arc<ScenarioHandler>,
        prices: Arc<dyn IPriceRepository>,
        policies: Arc<dyn IPolicyRepository>,
        wallets: Arc<dyn IWalletRepository>,
        protocols: Arc<ProtocolRegistry>,
    ) -> Self {
        Self {
            scenarios,
            prices,
            policies,
            wallets,
            protocols,
        }
    }

    /// Resolves an enabled server-owned price for an authorized scenario.
    ///
    /// # Errors
    ///
    /// Returns an error when scenario access is denied, price lookup fails, or
    /// no enabled price supports the requested resource and protocol.
    pub async fn resolve(
        &self,
        principal: &str,
        scenario_id: &str,
        resource: &str,
        protocol: &str,
    ) -> Result<PaymentPrice> {
        let _access = self.scenarios.authorize(principal, scenario_id).await?;
        let price = self
            .prices
            .resolve(scenario_id, resource, protocol)
            .await?
            .filter(|price| price.enabled && price.supports_protocol(protocol))
            .ok_or_else(|| Error::NotFound("payment price".to_owned()))?;
        self.policies
            .get(&price.policy_id)
            .await?
            .filter(|policy| {
                policy.enabled
                    && policy.scenario_id == scenario_id
                    && policy.allows_direction(crate::model::PaymentDirection::Inbound)
                    && policy.allows_wallet(&price.wallet_id)
            })
            .ok_or_else(|| Error::PolicyDenied("price has no enabled inbound policy-wallet binding".to_owned()))?;
        Ok(price)
    }

    /// Builds a challenge using the price's server-managed recipient wallet.
    ///
    /// # Errors
    ///
    /// Returns an error when price resolution or wallet lookup fails, the
    /// protocol is unsupported, or the adapter rejects challenge construction.
    pub async fn create_challenge(
        &self,
        principal: &str,
        scenario_id: &str,
        resource: &str,
        protocol: &str,
        now: i64,
    ) -> Result<PaymentChallenge> {
        let price = self.resolve(principal, scenario_id, resource, protocol).await?;
        let wallet = self
            .wallets
            .get_payment_wallet(&price.wallet_id)
            .await?
            .filter(|wallet| wallet.enabled)
            .ok_or_else(|| Error::NotFound("price recipient wallet".to_owned()))?
            .canonicalized_for_payment_network(&price.asset.network)
            .map_err(|field| Error::Storage(format!("invalid price recipient wallet {field}")))?;
        let challenge = self
            .protocols
            .get(protocol)?
            .build_challenge(&price, &wallet.wallet.address, now)
            .await?;
        if !wallet.wallet.address_matches(&challenge.recipient) {
            return Err(Error::PaymentProtocol(
                "adapter challenge recipient does not match the price wallet".to_owned(),
            ));
        }
        Ok(challenge)
    }
}

#[cfg(test)]
#[path = "../../tests/payment/price_test.rs"]
mod tests;
