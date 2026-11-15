//! Encrypted records in `wallet_key_version`; plaintext keys are never representable.
use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletKeyStatus {
    Active,
    Retired,
    Compromised,
}
impl WalletKeyStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Compromised => "compromised",
        }
    }
}
/// Envelope-encrypted private-key material for one immutable wallet version.
/// Plaintext private keys are intentionally not representable by this port.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedWalletKeyVersion {
    pub wallet_id: String,
    pub version: u32,
    pub algorithm: String,
    pub address: String,
    pub public_key: String,
    pub encryption_scheme: String,
    pub kek_id: String,
    pub wrapped_data_key: Vec<u8>,
    pub wrap_nonce: Vec<u8>,
    pub key_ciphertext: Vec<u8>,
    pub key_nonce: Vec<u8>,
    pub aad: Vec<u8>,
    pub status: WalletKeyStatus,
    pub activated_at: i64,
    pub retired_at: Option<i64>,
    pub created_at: i64,
}
impl fmt::Debug for EncryptedWalletKeyVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedWalletKeyVersion")
            .field("wallet_id", &self.wallet_id)
            .field("version", &self.version)
            .field("algorithm", &self.algorithm)
            .field("address", &self.address)
            .field("public_key", &self.public_key)
            .field("encryption_scheme", &self.encryption_scheme)
            .field("kek_id", &self.kek_id)
            .field("wrapped_data_key", &"<redacted>")
            .field("wrap_nonce", &"<redacted>")
            .field("key_ciphertext", &"<redacted>")
            .field("key_nonce", &"<redacted>")
            .field("aad", &"<redacted>")
            .field("status", &self.status)
            .field("activated_at", &self.activated_at)
            .field("retired_at", &self.retired_at)
            .field("created_at", &self.created_at)
            .finish()
    }
}
