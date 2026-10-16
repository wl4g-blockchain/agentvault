use std::{
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, HeaderMap, StatusCode},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac as _};
use serde::Deserialize;
use sha2::Sha256;

use crate::{
    config::AuthGuardIntegrationConfig, handler::UNRESTRICTED_PRINCIPAL, utils::secret::read_secret_ref, Error, Result,
};

const ACCESS_CONTEXT_HEADER: &str = "x-authguard-context";
const ACCESS_CONTEXT_PREFIX: &str = "agctx1";
const ACCESS_CONTEXT_VERSION: u8 = 3;
const MAX_CONTEXT_BYTES: usize = 8 * 1024;
const MAX_PRINCIPAL_BYTES: usize = 512;
const MIN_HMAC_KEY_BYTES: usize = 32;
const CLOCK_SKEW_SECONDS: u64 = 30;

/// Principal identity resolved from `AuthGuard`'s signed workload context.
///
/// Authentication, sessions, and authorization policies remain entirely in
/// `AuthGuard`. `AgentVault` consumes only the official direct-context adapter
/// wire and then applies its own payment-domain Client/Scenario mappings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerPrincipal(Arc<str>);

impl CallerPrincipal {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn trusted(value: &str) -> Option<Self> {
        let value = value.trim();
        (!value.is_empty()
            && value != UNRESTRICTED_PRINCIPAL
            && value.len() <= MAX_PRINCIPAL_BYTES
            && !value.chars().any(char::is_control))
        .then(|| Self(Arc::from(value)))
    }

    #[must_use]
    pub(crate) fn unrestricted() -> Self {
        Self(Arc::from(UNRESTRICTED_PRINCIPAL))
    }
}

impl<S> FromRequestParts<S> for CallerPrincipal
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> std::result::Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Clone)]
enum PrincipalMode {
    Open,
    AuthGuard { mac: Hmac<Sha256> },
}

/// Adapter for `AuthGuard`'s signed v3 direct access context.
///
/// This is deliberately not an authentication subsystem: it accepts no JWT,
/// cookie, password, or API key and performs no policy decision. The edge owns
/// those concerns and both sides share one deployment-managed HMAC key solely
/// to prevent an upstream client from forging the delivered principal.
#[derive(Clone)]
pub struct PrincipalContext {
    mode: PrincipalMode,
}

impl fmt::Debug for PrincipalContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrincipalContext")
            .field(
                "mode",
                &match self.mode {
                    PrincipalMode::Open => "open",
                    PrincipalMode::AuthGuard { .. } => "authguard",
                },
            )
            .finish()
    }
}

#[derive(Deserialize)]
struct AuthGuardAccessContext {
    version: u8,
    principal_id: String,
    issued_at_epoch_seconds: u64,
    expires_at_epoch_seconds: u64,
}

impl PrincipalContext {
    /// Builds the workload adapter from runtime configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured shared context key is unavailable,
    /// too short, or rejected by the HMAC implementation.
    pub fn from_config(config: &AuthGuardIntegrationConfig) -> Result<Self> {
        if !config.enabled {
            return Ok(Self {
                mode: PrincipalMode::Open,
            });
        }
        let key = read_secret_ref(
            &config.direct_context_hmac_key,
            "AuthGuard direct-context HMAC key",
            MAX_CONTEXT_BYTES,
        )?;
        if key.len() < MIN_HMAC_KEY_BYTES {
            return Err(Error::Config(format!(
                "AuthGuard direct-context HMAC key must contain at least {MIN_HMAC_KEY_BYTES} bytes"
            )));
        }
        let mac = Hmac::<Sha256>::new_from_slice(key.as_bytes())
            .map_err(|_| Error::Config("AuthGuard direct-context HMAC key is invalid".to_owned()))?;
        Ok(Self {
            mode: PrincipalMode::AuthGuard { mac },
        })
    }

    #[must_use]
    pub fn resolve(&self, headers: &HeaderMap) -> Option<CallerPrincipal> {
        match &self.mode {
            PrincipalMode::Open => Some(CallerPrincipal::unrestricted()),
            PrincipalMode::AuthGuard { mac } => {
                let signed = headers.get(ACCESS_CONTEXT_HEADER)?.to_str().ok()?;
                verify_access_context(mac, signed, epoch_seconds())
                    .and_then(|context| CallerPrincipal::trusted(&context.principal_id))
            }
        }
    }
}

fn verify_access_context(
    template: &Hmac<Sha256>,
    signed: &str,
    now_epoch_seconds: u64,
) -> Option<AuthGuardAccessContext> {
    if signed.len() > MAX_CONTEXT_BYTES {
        return None;
    }
    let mut parts = signed.split('.');
    let prefix = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if prefix != ACCESS_CONTEXT_PREFIX || payload.is_empty() || signature.is_empty() || parts.next().is_some() {
        return None;
    }
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    let mut mac = template.clone();
    mac.update(format!("{ACCESS_CONTEXT_PREFIX}.{payload}").as_bytes());
    mac.verify_slice(&signature).ok()?;

    let json = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let context: AuthGuardAccessContext = serde_json::from_slice(&json).ok()?;
    if context.version != ACCESS_CONTEXT_VERSION
        || context.expires_at_epoch_seconds <= context.issued_at_epoch_seconds
        || context.issued_at_epoch_seconds > now_epoch_seconds.saturating_add(CLOCK_SKEW_SECONDS)
        || context.expires_at_epoch_seconds <= now_epoch_seconds
    {
        return None;
    }
    Some(context)
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
#[path = "../../../tests/controller/http/principal_test.rs"]
mod tests;
