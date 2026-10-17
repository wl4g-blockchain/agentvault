use reqwest::Url;

use crate::{Error, Result};

// The official x402 EVM client backdates EIP-3009 authorizations by ten
// minutes. A confirmed block timestamp may therefore legitimately precede
// the application wall clock that created the local intent. Conversely, EVM
// consensus and RPC propagation can expose a block timestamp slightly ahead
// of a request-start timestamp. Keep both tolerances explicit and bounded.
const MAX_EFFECTIVE_TIME_PAST_SKEW_SECONDS: i64 = 10 * 60;
const MAX_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS: i64 = 60;

/// Validates bounded identifiers accepted by payment application commands.
pub(super) fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(Error::InvalidRequest(format!("{field} is invalid")));
    }
    Ok(())
}

/// Returns a normalized HTTP(S) origin after rejecting URL forms that can
/// confuse credential, policy, or audit boundaries.
pub(super) fn canonical_http_origin(resource: &str) -> Result<String> {
    let parsed = Url::parse(resource)
        .map_err(|_| Error::InvalidRequest("payment resource is not an absolute URL".to_owned()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Error::InvalidRequest("payment resource URL is unsafe".to_owned()));
    }
    Ok(parsed.origin().ascii_serialization())
}

pub(super) const fn utc_day_start(now: i64) -> i64 {
    now.saturating_sub(now.rem_euclid(86_400))
}

/// Validates a trusted chain/provider timestamp against bounded clock skew.
/// Transaction identity, amount, recipient, and authorization-window checks
/// remain the responsibility of the evidence adapter that produced it.
pub(super) fn validate_trusted_effective_time(created_at: i64, effective_at: i64, observed_at: i64) -> Result<()> {
    let earliest = created_at.saturating_sub(MAX_EFFECTIVE_TIME_PAST_SKEW_SECONDS);
    let latest = observed_at.saturating_add(MAX_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS);
    if effective_at < 0 || effective_at < earliest || effective_at > latest {
        return Err(Error::Settlement(
            "trusted settlement effective time exceeds bounded clock skew".to_owned(),
        ));
    }
    Ok(())
}

/// Domain-separated AEAD context shared by signing and crash-recovery paths.
pub(super) fn outbound_credential_context(intent_id: &str) -> String {
    format!("payment:{intent_id}:outbound-credential")
}

#[cfg(test)]
#[path = "../../tests/handler/common_test.rs"]
mod tests;
