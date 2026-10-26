//! Shared authenticated-encryption primitives.

use aes_gcm::{aead::consts::U16, aes::Aes256, AesGcm, Nonce};
use uuid::{Uuid, Variant, Version};

use crate::{Error, Result};

pub const AES_256_KEY_BYTES: usize = 32;
pub const UUID_NONCE_BYTES: usize = 16;
// XChaCha20-Poly1305 was the pre-release design; only this AES-GCM scheme is accepted.
pub const WALLET_KEY_ENCRYPTION_SCHEME: &str = "aes-256-gcm-uuid-v4-nonce-envelope-v1";

/// AES-256-GCM using the complete 128-bit UUID v4 value as its nonce.
pub type Aes256GcmUuidNonce = AesGcm<Aes256, U16>;
pub type UuidNonce = Nonce<U16>;

pub struct UuidNonceCodec;

impl UuidNonceCodec {
    /// Generates a UUID v4 nonce without truncating its 122 random bits.
    #[must_use]
    pub fn generate() -> [u8; UUID_NONCE_BYTES] {
        Uuid::new_v4().into_bytes()
    }

    /// Parses and validates a complete RFC 4122 UUID v4 nonce.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not exactly one RFC 4122 UUID v4 value.
    pub fn parse(bytes: &[u8]) -> Result<[u8; UUID_NONCE_BYTES]> {
        let nonce: [u8; UUID_NONCE_BYTES] = bytes
            .try_into()
            .map_err(|_| Error::Crypto("AES-GCM nonce is not a UUID v4".to_owned()))?;
        let uuid = Uuid::from_bytes(nonce);
        if uuid.get_variant() != Variant::RFC4122 || uuid.get_version() != Some(Version::Random) {
            return Err(Error::Crypto("AES-GCM nonce is not a UUID v4".to_owned()));
        }
        Ok(nonce)
    }
}

#[cfg(test)]
#[path = "../../tests/utils/aead_test.rs"]
mod tests;
