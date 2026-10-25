use serde::{Deserialize, Serialize};

use crate::model::{canonicalize_payment_recipient, canonicalize_wallet_address, wallet_addresses_match};

/// Persisted wallet metadata. Custody material is stored separately and is
/// intentionally absent from this entity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Wallet {
    pub id: String,
    pub algorithm: String,
    pub address: String,
    pub public_key: String,
    pub created_at: i64,
}

impl Wallet {
    /// Returns metadata with its address validated and canonicalized.
    ///
    /// # Errors
    ///
    /// Returns `address` for a malformed address.
    pub fn canonicalized(mut self) -> Result<Self, &'static str> {
        self.address = canonicalize_wallet_address(&self.algorithm, &self.address)?;
        Ok(self)
    }

    /// Additionally validates the address family required by a payment
    /// network.
    ///
    /// # Errors
    ///
    /// Returns `address` for malformed payment metadata.
    pub fn canonicalized_for_payment_network(mut self, network: &str) -> Result<Self, &'static str> {
        self = self.canonicalized()?;
        self.address = canonicalize_payment_recipient(network, &self.address)?;
        Ok(self)
    }

    #[must_use]
    pub fn address_matches(&self, candidate: &str) -> bool {
        wallet_addresses_match(&self.algorithm, &self.address, candidate)
    }
}

#[cfg(test)]
#[path = "../../../tests/model/domain/wallet_test.rs"]
mod tests;
