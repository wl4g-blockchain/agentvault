use std::{collections::HashMap, sync::Arc};

use crate::{
    model::{PaymentChallenge, PaymentOffer},
    protocol::PaymentProtocol,
    storage::{OfferStorage, PolicyStorage, WalletStorage},
    Error, Result,
};

use super::ScenarioHandler;

/// Immutable protocol registry shared by payer and payee handlers.
pub struct ProtocolRegistry {
    adapters: HashMap<&'static str, Arc<dyn PaymentProtocol>>,
}

impl ProtocolRegistry {
    /// Creates a registry and rejects duplicate adapter names.
    ///
    /// # Errors
    ///
    /// Returns an error when the adapter set contains duplicate protocol
    /// names. An empty registry represents a disabled payment direction.
    pub fn new(adapters: impl IntoIterator<Item = Arc<dyn PaymentProtocol>>) -> Result<Self> {
        let mut registered = HashMap::new();
        for adapter in adapters {
            let name = adapter.name();
            if registered.insert(name, adapter).is_some() {
                return Err(Error::Config(format!("duplicate payment protocol adapter: {name}")));
            }
        }
        Ok(Self { adapters: registered })
    }

    /// Resolves a protocol adapter by its stable wire name.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested protocol is not registered.
    pub fn get(&self, protocol: &str) -> Result<Arc<dyn PaymentProtocol>> {
        self.adapters
            .get(protocol)
            .cloned()
            .ok_or_else(|| Error::PaymentProtocol(format!("unsupported payment protocol: {protocol}")))
    }
}

/// Payee offer resolution. Pricing and recipient address always come from
/// server-managed records, never request overrides.
pub struct OfferHandler {
    scenarios: Arc<ScenarioHandler>,
    offers: Arc<dyn OfferStorage>,
    policies: Arc<dyn PolicyStorage>,
    wallets: Arc<dyn WalletStorage>,
    protocols: Arc<ProtocolRegistry>,
}

impl OfferHandler {
    #[must_use]
    pub fn new(
        scenarios: Arc<ScenarioHandler>,
        offers: Arc<dyn OfferStorage>,
        policies: Arc<dyn PolicyStorage>,
        wallets: Arc<dyn WalletStorage>,
        protocols: Arc<ProtocolRegistry>,
    ) -> Self {
        Self {
            scenarios,
            offers,
            policies,
            wallets,
            protocols,
        }
    }

    /// Resolves an enabled server-owned offer for an authorized scenario.
    ///
    /// # Errors
    ///
    /// Returns an error when scenario access is denied, offer lookup fails, or
    /// no enabled offer supports the requested resource and protocol.
    pub async fn resolve(
        &self,
        principal: &str,
        scenario_id: &str,
        resource: &str,
        protocol: &str,
    ) -> Result<PaymentOffer> {
        let _access = self.scenarios.authorize(principal, scenario_id).await?;
        let offer = self
            .offers
            .resolve(scenario_id, resource, protocol)
            .await?
            .filter(|offer| offer.enabled && offer.supports_protocol(protocol))
            .ok_or_else(|| Error::NotFound("payment offer".to_owned()))?;
        self.policies
            .get(&offer.policy_id)
            .await?
            .filter(|policy| {
                policy.enabled
                    && policy.scenario_id == scenario_id
                    && policy.allows_direction(crate::model::PaymentDirection::Inbound)
                    && policy.allows_wallet(&offer.wallet_id)
            })
            .ok_or_else(|| Error::PolicyDenied("offer has no enabled inbound policy-wallet binding".to_owned()))?;
        Ok(offer)
    }

    /// Builds a challenge using the offer's server-managed recipient wallet.
    ///
    /// # Errors
    ///
    /// Returns an error when offer resolution or wallet lookup fails, the
    /// protocol is unsupported, or the adapter rejects challenge construction.
    pub async fn create_challenge(
        &self,
        principal: &str,
        scenario_id: &str,
        resource: &str,
        protocol: &str,
        now: i64,
    ) -> Result<PaymentChallenge> {
        let offer = self.resolve(principal, scenario_id, resource, protocol).await?;
        let wallet = self
            .wallets
            .get_payment_wallet(&offer.wallet_id)
            .await?
            .filter(|wallet| wallet.enabled)
            .ok_or_else(|| Error::NotFound("offer recipient wallet".to_owned()))?
            .canonicalized_for_payment_network(&offer.asset.network)
            .map_err(|field| Error::Storage(format!("invalid offer recipient wallet {field}")))?;
        let challenge = self
            .protocols
            .get(protocol)?
            .build_challenge(&offer, &wallet.wallet.address, now)
            .await?;
        if !wallet.wallet.address_matches(&challenge.recipient) {
            return Err(Error::PaymentProtocol(
                "adapter challenge recipient does not match the offer wallet".to_owned(),
            ));
        }
        Ok(challenge)
    }
}

#[cfg(test)]
#[path = "../../tests/handler/offer_handler_test.rs"]
mod tests;
