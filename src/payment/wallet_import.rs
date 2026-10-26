//! One-use RSA-OAEP encryption for browser EOA imports. Temporary private keys
//! are sealed in the configured TTL cache; durable custody remains owned by
//! [`WalletKeyManager`], which encrypts the EOA under the Wallet KEK before SQL.

use crate::{
    cache::CacheError, cache::ICache, model::wallet::CachedWalletImportSession, model::wallet::EVM_EOA_ALGORITHM,
    model::CreateEncryptedWallet, model::Wallet, model::WalletImportSession, storage::WalletCreationConfig,
    storage::WalletKeyManager, utils::secure_material::SensitiveMaterialProtector, Error, Result,
};
use aws_lc_rs::{
    encoding::AsDer as _, encoding::Pkcs8V1Der, encoding::PublicKeyX509Der, rsa::KeySize, rsa::PrivateDecryptingKey,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use uuid::Uuid;

#[cfg(test)]
use aws_lc_rs::rsa::{OaepPublicEncryptingKey, PublicEncryptingKey, OAEP_SHA256_MGF1SHA256};
#[cfg(test)]
use zeroize::Zeroizing;

#[cfg(test)]
use crate::model::{wallet::RSA_CIPHERTEXT_BYTES, EncryptedWalletPrivateKey};

const CACHE_NAMESPACE: &str = "wallet-import";
const SESSION_VERSION: u8 = 2;

/// Coordinates temporary public-key encryption with existing Wallet custody.
pub struct WalletImportService {
    cache: Arc<dyn ICache>,
    keys: Arc<dyn WalletKeyManager>,
    protector: Arc<dyn SensitiveMaterialProtector>,
    session_ttl: Duration,
    key_generation: Arc<Semaphore>,
}

impl WalletImportService {
    #[must_use]
    pub fn new(
        cache: Arc<dyn ICache>,
        keys: Arc<dyn WalletKeyManager>,
        protector: Arc<dyn SensitiveMaterialProtector>,
        session_ttl: Duration,
    ) -> Self {
        Self {
            cache,
            keys,
            protector,
            session_ttl,
            key_generation: Arc::new(Semaphore::new(2)),
        }
    }

    /// Creates a principal-bound one-use transport key session.
    ///
    /// # Errors
    /// Returns an error when authentication, key generation, protection or cache
    /// storage fails. Key generation is bounded and does not block the async runtime.
    pub async fn create_import_session(&self, principal: &str, now: i64) -> Result<WalletImportSession> {
        Self::validate_principal(principal)?;
        let ttl_seconds = i64::try_from(self.session_ttl.as_secs())
            .map_err(|_| Error::Config("wallet import session TTL is too large".to_owned()))?;
        let expires_at = now
            .checked_add(ttl_seconds)
            .ok_or_else(|| Error::InvalidRequest("wallet import session expiry overflowed".to_owned()))?;
        let session_id = Uuid::new_v4().to_string();
        let (server_public_key, private_key) = self.generate_import_key().await?;
        let context = CachedWalletImportSession::context(&session_id, principal);
        let record = CachedWalletImportSession {
            version: SESSION_VERSION,
            principal_hash: CachedWalletImportSession::principal_hash(principal),
            sealed_server_private_key: self.protector.seal(&context, private_key.as_ref())?,
            expires_at,
        };
        // AWS-LC's DER buffer zeroizes on drop; only sealed key material enters Cache.
        drop(private_key);
        let encoded = serde_json::to_vec(&record)
            .map_err(|_| Error::Crypto("wallet import session encoding failed".to_owned()))?;
        if !self
            .cache
            .set_if_absent(CACHE_NAMESPACE, &session_id, encoded, self.session_ttl)
            .await
            .map_err(Self::cache_error)?
        {
            return Err(Error::DependencyUnavailable(
                "wallet import session identifier collision".to_owned(),
            ));
        }
        Ok(WalletImportSession {
            session_id,
            server_public_key,
            expires_at,
        })
    }

    async fn generate_import_key(&self) -> Result<(String, Pkcs8V1Der<'static>)> {
        let permit = Arc::clone(&self.key_generation)
            .try_acquire_owned()
            .map_err(|_| Error::DependencyUnavailable("wallet import key generation is busy".to_owned()))?;
        tokio::task::spawn_blocking(move || {
            // Retain the permit until generation ends, including if the caller disconnects.
            let _permit = permit;
            let key = PrivateDecryptingKey::generate(KeySize::Rsa3072)
                .map_err(|_| Error::Crypto("wallet import key generation failed".to_owned()))?;
            let public_key: PublicKeyX509Der<'static> = key
                .public_key()
                .as_der()
                .map_err(|_| Error::Crypto("wallet import public key encoding failed".to_owned()))?;
            let private_key: Pkcs8V1Der<'static> = key
                .as_der()
                .map_err(|_| Error::Crypto("wallet import private key encoding failed".to_owned()))?;
            Ok((URL_SAFE_NO_PAD.encode(public_key.as_ref()), private_key))
        })
        .await
        .map_err(|_| Error::Crypto("wallet import key generation interrupted".to_owned()))?
    }

    /// Consumes a cached private key, decrypts the EOA, and delegates KEK encryption
    /// and the durable Wallet transaction to [`WalletKeyManager`].
    ///
    /// # Errors
    /// Returns an error for malformed envelopes, missing/expired/replayed or
    /// differently owned sessions, invalid ciphertext, or storage failure.
    pub async fn create_wallet(&self, principal: &str, request: CreateEncryptedWallet, now: i64) -> Result<Wallet> {
        Self::validate_principal(principal)?;
        if request.id.is_empty() || request.id.chars().any(char::is_control) {
            return Err(Error::InvalidRequest("wallet ID is invalid".to_owned()));
        }
        if request.algorithm != EVM_EOA_ALGORITHM {
            return Err(Error::InvalidRequest(
                "encrypted wallet import supports only secp256k1 EOA keys".to_owned(),
            ));
        }
        let config = WalletCreationConfig {
            enabled: request.enabled,
            budget_unit: request.budget_unit,
            max_per_payment: request.max_per_payment,
            max_per_day: request.max_per_day,
        };
        config
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("wallet {field} is invalid")))?;
        let envelope = request.encrypted_private_key;
        let cached = self
            .cache
            .take(CACHE_NAMESPACE, &envelope.session_id)
            .await
            .map_err(Self::cache_error)?
            .ok_or_else(|| Error::NotFound("wallet import session".to_owned()))?;
        let record: CachedWalletImportSession = serde_json::from_slice(&cached)
            .map_err(|_| Error::Crypto("wallet import session is invalid".to_owned()))?;
        if record.version != SESSION_VERSION
            || record.principal_hash != CachedWalletImportSession::principal_hash(principal)
            || record.expires_at <= now
        {
            return Err(Error::Forbidden);
        }
        let context = CachedWalletImportSession::context(&envelope.session_id, principal);
        let server_key = self.protector.open(&context, &record.sealed_server_private_key)?;
        let private_key = envelope.decrypt(server_key.as_slice(), &request.id, &request.algorithm)?;
        drop(server_key);
        self.keys
            .create_local(&request.id, &request.algorithm, private_key.as_ref(), &config, now)
            .await
    }

    fn validate_principal(principal: &str) -> Result<()> {
        if principal.is_empty() || principal.len() > 512 || principal.chars().any(char::is_control) {
            return Err(Error::Unauthenticated);
        }
        Ok(())
    }

    fn cache_error(error: CacheError) -> Error {
        match error {
            CacheError::Unavailable(reason) => Error::DependencyUnavailable(reason),
            CacheError::InvalidValue(reason) => {
                Error::Crypto(format!("wallet import cache value is invalid: {reason}"))
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/payment/wallet_import_test.rs"]
mod tests;
