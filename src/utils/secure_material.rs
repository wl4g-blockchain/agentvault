use std::{collections::BTreeMap, fmt};

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::config::RecoveryEncryptionConfig;
use crate::utils::aead::Aes256GcmUuidNonce;
use crate::utils::aead::UuidNonce;
use crate::utils::aead::UuidNonceCodec;
use crate::utils::aead::AES_256_KEY_BYTES;
use crate::utils::secret::SecretReader;
use crate::Error;
use crate::Result;

const MATERIAL_VERSION: u8 = 2;
const AAD_DOMAIN: &[u8] = b"agentvault.sensitive-material.v2";

/// Envelope stored in immutable events. The plaintext is available only to a
/// configured protector that retains the referenced key version.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SealedSensitiveMaterial {
    pub version: u8,
    pub key_id: String,
    pub nonce: String,
    pub ciphertext: String,
}

impl fmt::Debug for SealedSensitiveMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedSensitiveMaterial")
            .field("version", &self.version)
            .field("key_id", &self.key_id)
            .field("nonce", &"<redacted>")
            .field("ciphertext", &"<redacted>")
            .finish()
    }
}

/// Protects short-lived recovery material before it enters append-only
/// persistence. Implementations must authenticate the supplied context.
pub trait SensitiveMaterialProtector: Send + Sync {
    /// Encrypts and authenticates sensitive recovery material.
    ///
    /// # Errors
    ///
    /// Returns an error when the context or plaintext is invalid, key material
    /// is unavailable, randomness fails, or encryption cannot complete.
    fn seal(&self, context: &str, plaintext: &[u8]) -> Result<SealedSensitiveMaterial>;

    /// Authenticates and decrypts sensitive recovery material.
    ///
    /// # Errors
    ///
    /// Returns an error when the envelope/context is invalid, its key version
    /// is unavailable, or authentication fails.
    fn open(&self, context: &str, material: &SealedSensitiveMaterial) -> Result<Zeroizing<Vec<u8>>>;
}

/// AES-256-GCM key ring. Rotation selects a new active key while old
/// key versions remain available until no pending recovery material uses them.
pub struct Aes256GcmMaterialProtector {
    active_key_id: String,
    keys: BTreeMap<String, Zeroizing<[u8; AES_256_KEY_BYTES]>>,
}

impl fmt::Debug for Aes256GcmMaterialProtector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Aes256GcmMaterialProtector")
            .field("active_key_id", &self.active_key_id)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Aes256GcmMaterialProtector {
    /// Creates a versioned protector. The active key must be present in the
    /// key ring; callers retain retired keys until all pending intents expire.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe identifier, an empty key ring, duplicate
    /// key identifiers, or a missing active key.
    pub fn new(
        active_key_id: impl Into<String>,
        keys: impl IntoIterator<Item = (String, [u8; AES_256_KEY_BYTES])>,
    ) -> Result<Self> {
        let active_key_id = active_key_id.into();
        Aes256GcmMaterialProtector::validate_key_id(&active_key_id)?;
        let mut key_ring = BTreeMap::new();
        for (key_id, key) in keys {
            Aes256GcmMaterialProtector::validate_key_id(&key_id)?;
            if key_ring.insert(key_id.clone(), Zeroizing::new(key)).is_some() {
                return Err(Error::Config(format!("duplicate sensitive-material key id: {key_id}")));
            }
        }
        if key_ring.is_empty() || !key_ring.contains_key(&active_key_id) {
            return Err(Error::Config(
                "sensitive-material active key is missing from the key ring".to_owned(),
            ));
        }
        Ok(Self {
            active_key_id,
            keys: key_ring,
        })
    }

    /// Resolves a versioned material key ring from indirect secret references.
    /// Values must be unpadded base64url encodings of exactly 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when a secret is unavailable, malformed, or does not
    /// decode to the required AES-256-GCM key length.
    pub fn from_config(config: &RecoveryEncryptionConfig) -> Result<Self> {
        let mut keys = Vec::with_capacity(config.keys.len());
        for (key_id, reference) in &config.keys {
            let encoded = SecretReader::read_ref(reference, &format!("sensitive-material key {key_id}"), 1024)?;
            let decoded = URL_SAFE_NO_PAD
                .decode(encoded.trim_end_matches(['\r', '\n']))
                .map(Zeroizing::new)
                .map_err(|_| Error::Config(format!("sensitive-material key {key_id} is not valid base64url")))?;
            let key: [u8; AES_256_KEY_BYTES] = decoded.as_slice().try_into().map_err(|_| {
                Error::Config(format!(
                    "sensitive-material key {key_id} must decode to {AES_256_KEY_BYTES} bytes"
                ))
            })?;
            keys.push((key_id.clone(), key));
        }
        Self::new(config.active_key_id.clone(), keys)
    }
}

impl SensitiveMaterialProtector for Aes256GcmMaterialProtector {
    fn seal(&self, context: &str, plaintext: &[u8]) -> Result<SealedSensitiveMaterial> {
        Aes256GcmMaterialProtector::validate_context(context)?;
        if plaintext.is_empty() {
            return Err(Error::Crypto("sensitive material must not be empty".to_owned()));
        }
        let key = self
            .keys
            .get(&self.active_key_id)
            .ok_or_else(|| Error::Crypto("active sensitive-material key is unavailable".to_owned()))?;
        let nonce = UuidNonceCodec::generate();
        let aad = Aes256GcmMaterialProtector::aad(context, &self.active_key_id);
        let cipher = Aes256GcmUuidNonce::new_from_slice(key.as_ref())
            .map_err(|_| Error::Crypto("sensitive-material key length is invalid".to_owned()))?;
        let ciphertext = cipher
            .encrypt(
                UuidNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::Crypto("sensitive-material encryption failed".to_owned()))?;
        Ok(SealedSensitiveMaterial {
            version: MATERIAL_VERSION,
            key_id: self.active_key_id.clone(),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
        })
    }

    fn open(&self, context: &str, material: &SealedSensitiveMaterial) -> Result<Zeroizing<Vec<u8>>> {
        Aes256GcmMaterialProtector::validate_context(context)?;
        if material.version != MATERIAL_VERSION {
            return Err(Error::Crypto(
                "unsupported sensitive-material envelope version".to_owned(),
            ));
        }
        Aes256GcmMaterialProtector::validate_key_id(&material.key_id)?;
        let key = self
            .keys
            .get(&material.key_id)
            .ok_or_else(|| Error::Crypto("sensitive-material key version is unavailable".to_owned()))?;
        let nonce = URL_SAFE_NO_PAD
            .decode(&material.nonce)
            .map_err(|_| Error::Crypto("sensitive-material nonce is invalid".to_owned()))?;
        let nonce = UuidNonceCodec::parse(&nonce)
            .map_err(|_| Error::Crypto("sensitive-material nonce is invalid".to_owned()))?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(&material.ciphertext)
            .map_err(|_| Error::Crypto("sensitive-material ciphertext is invalid".to_owned()))?;
        let aad = Aes256GcmMaterialProtector::aad(context, &material.key_id);
        let cipher = Aes256GcmUuidNonce::new_from_slice(key.as_ref())
            .map_err(|_| Error::Crypto("sensitive-material key length is invalid".to_owned()))?;
        cipher
            .decrypt(
                UuidNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::Crypto("sensitive-material authentication failed".to_owned()))
    }
}

impl Aes256GcmMaterialProtector {
    fn aad(context: &str, key_id: &str) -> Vec<u8> {
        let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + context.len() + key_id.len() + 2);
        aad.extend_from_slice(AAD_DOMAIN);
        aad.push(0);
        aad.extend_from_slice(context.as_bytes());
        aad.push(0);
        aad.extend_from_slice(key_id.as_bytes());
        aad
    }

    fn validate_context(context: &str) -> Result<()> {
        if context.is_empty() || context.len() > 512 || context.chars().any(char::is_control) {
            return Err(Error::Crypto("sensitive-material context is invalid".to_owned()));
        }
        Ok(())
    }

    fn validate_key_id(key_id: &str) -> Result<()> {
        if key_id.is_empty()
            || key_id.len() > 128
            || !key_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        {
            return Err(Error::Config("sensitive-material key id is invalid".to_owned()));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/utils/secure_material_test.rs"]
mod tests;
