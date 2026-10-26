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
use tokio::sync::OnceCell;
use tonic::{
    metadata::MetadataValue,
    transport::{Channel, Endpoint},
    Code, Request as GrpcRequest,
};

use crate::apm::inject_current_trace_context;
use crate::authguard_access_wire::access_context_service_client::AccessContextServiceClient;
use crate::config::AuthGuardIntegrationConfig;
use crate::config::AuthGuardScopeResolverConfig;
use crate::model::scenario::UNRESTRICTED_PRINCIPAL;
use crate::utils::secret::SecretReader;
use crate::Error;
use crate::Result;

const ACCESS_CONTEXT_HEADER: &str = "x-authguard-context";
const SCOPE_TOKEN_HEADER: &str = "x-authguard-scope-token";
const ACCESS_CONTEXT_PREFIX: &str = "agctx1";
const ACCESS_CONTEXT_VERSION: u8 = 3;
const MAX_CONTEXT_BYTES: usize = 8 * 1024;
const MAX_PRINCIPAL_BYTES: usize = 512;
const MIN_HMAC_KEY_BYTES: usize = 32;
const CLOCK_SKEW_SECONDS: u64 = 30;
const SCOPE_TOKEN_PREFIX: &str = "ags_";
const SCOPE_TOKEN_ENCODED_BYTES: usize = 43;

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
    AuthGuard(Box<AuthGuardMode>),
}

#[derive(Clone)]
struct AuthGuardMode {
    mac: Hmac<Sha256>,
    scope_resolver: Option<AuthGuardScopeResolver>,
}

#[derive(Clone)]
struct AuthGuardScopeResolver {
    endpoint: Endpoint,
    timeout: std::time::Duration,
    channel: Arc<OnceCell<Channel>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrincipalResolutionError {
    Invalid,
    Unavailable,
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
                    PrincipalMode::AuthGuard(_) => "authguard",
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
        let key = SecretReader::read_ref(
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
        let scope_resolver = config
            .scope_resolver
            .enabled
            .then(|| AuthGuardScopeResolver::from_config(&config.scope_resolver))
            .transpose()?;
        Ok(Self {
            mode: PrincipalMode::AuthGuard(Box::new(AuthGuardMode { mac, scope_resolver })),
        })
    }

    /// Resolves either `AuthGuard`'s signed direct context or its opaque scope
    /// token. Direct contexts are verified locally; only opaque tokens invoke
    /// the internal `AuthGuard` resolver.
    pub(crate) async fn resolve_request(
        &self,
        headers: &HeaderMap,
    ) -> std::result::Result<Option<CallerPrincipal>, PrincipalResolutionError> {
        match &self.mode {
            PrincipalMode::Open => Ok(Some(CallerPrincipal::unrestricted())),
            PrincipalMode::AuthGuard(authguard) => {
                if headers.contains_key(ACCESS_CONTEXT_HEADER) && headers.contains_key(SCOPE_TOKEN_HEADER) {
                    return Err(PrincipalResolutionError::Invalid);
                }
                if let Some(signed) = headers.get(ACCESS_CONTEXT_HEADER) {
                    return signed
                        .to_str()
                        .ok()
                        .and_then(|signed| {
                            PrincipalContext::verify_access_context(
                                &authguard.mac,
                                signed,
                                PrincipalContext::epoch_seconds(),
                            )
                        })
                        .and_then(|context| CallerPrincipal::trusted(&context.principal_id))
                        .map(Some)
                        .ok_or(PrincipalResolutionError::Invalid);
                }
                let Some(token_header) = headers.get(SCOPE_TOKEN_HEADER) else {
                    return Ok(None);
                };
                let token = token_header
                    .to_str()
                    .ok()
                    .filter(|token| PrincipalContext::valid_scope_token(token))
                    .ok_or(PrincipalResolutionError::Invalid)?;
                let resolver = authguard
                    .scope_resolver
                    .as_ref()
                    .ok_or(PrincipalResolutionError::Unavailable)?;
                let context = resolver.resolve(token).await?;
                CallerPrincipal::trusted(&context.principal_id)
                    .map(Some)
                    .ok_or(PrincipalResolutionError::Invalid)
            }
        }
    }
}

impl AuthGuardScopeResolver {
    fn from_config(config: &AuthGuardScopeResolverConfig) -> Result<Self> {
        let endpoint = Endpoint::from_shared(config.target.clone())
            .map_err(|error| Error::Config(format!("invalid AuthGuard scope resolver target: {error}")))?
            .connect_timeout(config.timeout)
            .timeout(config.timeout);
        Ok(Self {
            endpoint,
            timeout: config.timeout,
            channel: Arc::new(OnceCell::new()),
        })
    }

    async fn resolve(&self, token: &str) -> std::result::Result<AuthGuardAccessContext, PrincipalResolutionError> {
        let channel = tokio::time::timeout(self.timeout, self.channel.get_or_try_init(|| self.endpoint.connect()))
            .await
            .map_err(|_| PrincipalResolutionError::Unavailable)?
            .map_err(|error| {
                tracing::warn!(%error, "AuthGuard scope resolver connection failed");
                PrincipalResolutionError::Unavailable
            })?;
        let mut client = AccessContextServiceClient::new(channel.clone());
        let mut request = GrpcRequest::new(token.to_owned());
        let mut trace_headers = HeaderMap::new();
        inject_current_trace_context(&mut trace_headers);
        for name in ["traceparent", "tracestate"] {
            if let Some(value) = trace_headers.get(name).and_then(|value| value.to_str().ok()) {
                if let Ok(value) = MetadataValue::try_from(value) {
                    request.metadata_mut().insert(name, value);
                }
            }
        }
        let encoded = tokio::time::timeout(self.timeout, client.resolve_scope(request))
            .await
            .map_err(|_| PrincipalResolutionError::Unavailable)?
            .map_err(|error| {
                tracing::warn!(grpc_status = %error.code(), "AuthGuard scope token resolution failed");
                match error.code() {
                    Code::InvalidArgument | Code::Unauthenticated | Code::PermissionDenied | Code::NotFound => {
                        PrincipalResolutionError::Invalid
                    }
                    _ => PrincipalResolutionError::Unavailable,
                }
            })?
            .into_inner();
        PrincipalContext::decode_access_context(&encoded, PrincipalContext::epoch_seconds())
            .ok_or(PrincipalResolutionError::Unavailable)
    }
}

#[cfg(test)]
#[path = "../../tests/controller/principal_context_test.rs"]
mod tests;

impl PrincipalContext {
    fn valid_scope_token(token: &str) -> bool {
        token.len() == SCOPE_TOKEN_PREFIX.len() + SCOPE_TOKEN_ENCODED_BYTES
            && token.starts_with(SCOPE_TOKEN_PREFIX)
            && token[SCOPE_TOKEN_PREFIX.len()..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
        PrincipalContext::valid_access_context(context, now_epoch_seconds)
    }

    fn decode_access_context(encoded: &str, now_epoch_seconds: u64) -> Option<AuthGuardAccessContext> {
        if encoded.len() > MAX_CONTEXT_BYTES {
            return None;
        }
        let json = URL_SAFE_NO_PAD.decode(encoded).ok()?;
        let context = serde_json::from_slice(&json).ok()?;
        PrincipalContext::valid_access_context(context, now_epoch_seconds)
    }

    fn valid_access_context(context: AuthGuardAccessContext, now_epoch_seconds: u64) -> Option<AuthGuardAccessContext> {
        (context.version == ACCESS_CONTEXT_VERSION
            && context.expires_at_epoch_seconds > context.issued_at_epoch_seconds
            && context.issued_at_epoch_seconds <= now_epoch_seconds.saturating_add(CLOCK_SKEW_SECONDS)
            && context.expires_at_epoch_seconds > now_epoch_seconds)
            .then_some(context)
    }

    fn epoch_seconds() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }
}
