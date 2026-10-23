use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use std::str::FromStr as _;

/// Deterministic wallet-pool selection used for outbound payments.
///
/// Both strategies are replay-safe: the same `run_id` resolves to the already
/// persisted intent, and a pre-persistence retry uses the same request hash.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WalletSelectionStrategy {
    /// Prefer the wallet with the least confirmed-plus-reserved daily spend.
    #[default]
    LeastSpent,
    /// Spread runs by a stable hash of the immutable request and candidate.
    StableRandom,
}

impl WalletSelectionStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeastSpent => "least_spent",
            Self::StableRandom => "stable_random",
        }
    }
}

/// Canonical algorithm emitted by the built-in Ethereum EOA signer.
pub const EVM_EOA_ALGORITHM: &str = "secp256k1";
/// Canonical algorithm emitted by the built-in Solana custody signer.
pub const SOLANA_ED25519_ALGORITHM: &str = "ed25519";

/// Returns whether an algorithm explicitly identifies an EVM-style address.
///
/// Explicit EVM algorithms reject malformed/opaque addresses. Independently,
/// a strict EVM-shaped address is canonicalized for external custody too.
#[must_use]
pub fn is_evm_wallet_algorithm(algorithm: &str) -> bool {
    matches!(
        algorithm.to_ascii_lowercase().as_str(),
        EVM_EOA_ALGORITHM | "evm" | "ethereum" | "eip155"
    )
}

/// Returns whether an algorithm explicitly identifies a Solana ed25519 key.
#[must_use]
pub fn is_solana_wallet_algorithm(algorithm: &str) -> bool {
    matches!(
        algorithm.to_ascii_lowercase().as_str(),
        SOLANA_ED25519_ALGORITHM | "solana"
    )
}

fn evm_address_body(address: &str) -> Option<&str> {
    address
        .strip_prefix("0x")
        .or_else(|| address.strip_prefix("0X"))
        .filter(|encoded| encoded.len() == 40 && encoded.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Returns whether the payment network uses EIP-155/EVM recipient addresses.
#[must_use]
pub fn is_evm_payment_network(network: &str) -> bool {
    network
        .strip_prefix("eip155:")
        .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Returns whether the payment network is a canonical Solana CAIP-2 ID.
#[must_use]
pub fn is_solana_payment_network(network: &str) -> bool {
    network
        .strip_prefix("solana:")
        .is_some_and(|reference| !reference.is_empty() && reference.len() <= 128)
}

fn canonical_solana_address(address: &str) -> Option<String> {
    Pubkey::from_str(address).ok().map(|address| address.to_string())
}

/// Validates and canonicalizes an address according to its wallet algorithm
/// and unambiguous address shape.
///
/// EVM addresses must be exactly `0x` plus 20 bytes of hexadecimal data and
/// are stored lower-case. A strict EVM-shaped address is canonicalized even
/// for external custody, whose `algorithm` describes custody rather than the
/// address family. Opaque addresses for other algorithms remain byte-for-byte
/// unchanged.
///
/// # Errors
///
/// Returns `address` when an explicitly EVM algorithm has an invalid address.
pub fn canonicalize_wallet_address(algorithm: &str, address: &str) -> Result<String, &'static str> {
    if let Some(encoded) = evm_address_body(address) {
        return Ok(format!("0x{}", encoded.to_ascii_lowercase()));
    }
    if is_evm_wallet_algorithm(algorithm) {
        return Err("address");
    }
    if is_solana_wallet_algorithm(algorithm) {
        return canonical_solana_address(address).ok_or("address");
    }
    Ok(address.to_owned())
}

/// Validates and canonicalizes a recipient for its payment network.
///
/// # Errors
///
/// Returns `address` when an EIP-155 recipient is not a 20-byte EVM address.
pub fn canonicalize_payment_recipient(network: &str, address: &str) -> Result<String, &'static str> {
    if is_evm_payment_network(network) {
        return evm_address_body(address)
            .map(|encoded| format!("0x{}", encoded.to_ascii_lowercase()))
            .ok_or("address");
    }
    if is_solana_payment_network(network) {
        return canonical_solana_address(address).ok_or("address");
    }
    Ok(address.to_owned())
}

/// Compares two addresses using the canonical rules for their algorithm.
#[must_use]
pub fn wallet_addresses_match(algorithm: &str, left: &str, right: &str) -> bool {
    match (
        canonicalize_wallet_address(algorithm, left),
        canonicalize_wallet_address(algorithm, right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Compares payment recipients using EVM canonicalization only for EIP-155
/// networks. Other networks retain exact matching.
#[must_use]
pub fn payment_recipients_match(network: &str, left: &str, right: &str) -> bool {
    match (
        canonicalize_payment_recipient(network, left),
        canonicalize_payment_recipient(network, right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Wallet {
    pub id: String,
    pub algorithm: String,
    pub address: String,
    pub public_key: String,
    pub created_at: i64,
}

impl Wallet {
    /// Returns wallet metadata with its address validated and canonicalized.
    ///
    /// # Errors
    ///
    /// Returns `address` for a malformed EVM address.
    pub fn canonicalized(mut self) -> Result<Self, &'static str> {
        self.address = canonicalize_wallet_address(&self.algorithm, &self.address)?;
        Ok(self)
    }

    /// Additionally validates the address family required by a payment
    /// network. This keeps external custody separate from EVM address type.
    ///
    /// # Errors
    ///
    /// Returns `address` for malformed EIP-155 recipient metadata.
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
#[path = "../../tests/model/wallet_test.rs"]
mod tests;
