use aws_lc_rs::rsa::{OaepPrivateDecryptingKey, PrivateDecryptingKey, OAEP_SHA256_MGF1SHA256};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use solana_pubkey::Pubkey;
use std::str::FromStr as _;
use zeroize::Zeroizing;

pub use super::domain::Wallet;
use super::{Amount, BudgetUnit};
use crate::{utils::secure_material::SealedSensitiveMaterial, Error, Result as AppResult};

pub(crate) const RSA_CIPHERTEXT_BYTES: usize = 384;
const EOA_PRIVATE_KEY_BYTES: usize = 32;

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
pub fn canonicalize_wallet_address(algorithm: &str, address: &str) -> std::result::Result<String, &'static str> {
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
pub fn canonicalize_payment_recipient(network: &str, address: &str) -> std::result::Result<String, &'static str> {
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

/// Public half of a principal-bound, one-use RSA-3072 OAEP/SHA-256 session.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WalletImportSession {
    pub session_id: String,
    pub server_public_key: String,
    pub expires_at: i64,
}

/// One-use server key retained only in the configured TTL cache. This is an
/// internal transport record, not an authoritative SQL entity.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CachedWalletImportSession {
    pub(crate) version: u8,
    pub(crate) principal_hash: String,
    pub(crate) sealed_server_private_key: SealedSensitiveMaterial,
    pub(crate) expires_at: i64,
}

impl CachedWalletImportSession {
    pub(crate) fn principal_hash(principal: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(principal.as_bytes()))
    }

    pub(crate) fn context(session_id: &str, principal: &str) -> String {
        format!("wallet-import-session:{session_id}:{}", Self::principal_hash(principal))
    }
}

/// Browser-produced RSA ciphertext containing exactly one EOA private key.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptedWalletPrivateKey {
    pub session_id: String,
    pub ciphertext: String,
}

impl EncryptedWalletPrivateKey {
    pub(crate) fn label(&self, wallet_id: &str, algorithm: &str) -> String {
        format!(
            "agentvault.wallet-import.v2\0{}\0{wallet_id}\0{algorithm}",
            self.session_id
        )
    }

    pub(crate) fn decrypt(
        &self,
        server_key: &[u8],
        wallet_id: &str,
        algorithm: &str,
    ) -> AppResult<Zeroizing<[u8; 32]>> {
        let invalid = || Error::InvalidRequest("encrypted wallet private key is invalid".to_owned());
        if self.ciphertext.len() != RSA_CIPHERTEXT_BYTES * 4 / 3 {
            return Err(invalid());
        }
        let ciphertext = URL_SAFE_NO_PAD.decode(&self.ciphertext).map_err(|_| invalid())?;
        if ciphertext.len() != RSA_CIPHERTEXT_BYTES || URL_SAFE_NO_PAD.encode(&ciphertext) != self.ciphertext {
            return Err(invalid());
        }
        let key = PrivateDecryptingKey::from_pkcs8(server_key)
            .map_err(|_| Error::Crypto("wallet import server key is invalid".to_owned()))?;
        let key = OaepPrivateDecryptingKey::new(key)
            .map_err(|_| Error::Crypto("wallet import server key is invalid".to_owned()))?;
        let mut output = Zeroizing::new([0_u8; RSA_CIPHERTEXT_BYTES]);
        let label = self.label(wallet_id, algorithm);
        let plaintext = key
            .decrypt(
                &OAEP_SHA256_MGF1SHA256,
                &ciphertext,
                output.as_mut(),
                Some(label.as_bytes()),
            )
            .map_err(|_| invalid())?;
        if plaintext.len() != EOA_PRIVATE_KEY_BYTES {
            return Err(invalid());
        }
        let mut private_key = Zeroizing::new([0_u8; EOA_PRIVATE_KEY_BYTES]);
        private_key.copy_from_slice(plaintext);
        Ok(private_key)
    }
}

impl std::fmt::Debug for EncryptedWalletPrivateKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncryptedWalletPrivateKey")
            .field("session_id", &self.session_id)
            .field("ciphertext", &"<redacted>")
            .finish()
    }
}

/// Complete atomic wallet-creation request after HTTP authentication.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEncryptedWallet {
    pub id: String,
    pub algorithm: String,
    #[serde(default = "CreateEncryptedWallet::enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub budget_unit: Option<BudgetUnit>,
    #[serde(default)]
    pub max_per_payment: Option<Amount>,
    #[serde(default)]
    pub max_per_day: Option<Amount>,
    pub encrypted_private_key: EncryptedWalletPrivateKey,
}

impl CreateEncryptedWallet {
    const fn enabled_by_default() -> bool {
        true
    }
}

#[cfg(test)]
#[path = "../../tests/model/wallet_test.rs"]
mod tests;
