use crate::{Error, Result};
use reqwest::Url;

pub struct PaymentValidation;

impl PaymentValidation {
    /// Validates bounded identifiers accepted by payment application commands.
    ///
    /// # Errors
    /// Returns an invalid-request error for an empty, oversized or unsafe identifier.
    pub fn validate_identifier(field: &str, value: &str) -> Result<()> {
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
    ///
    /// # Errors
    /// Returns an invalid-request error for an unsafe or non-HTTP resource URL.
    pub fn canonical_http_origin(resource: &str) -> Result<String> {
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

    #[must_use]
    pub const fn utc_day_start(now: i64) -> i64 {
        now.saturating_sub(now.rem_euclid(86_400))
    }

    /// Validates a trusted chain/provider timestamp against bounded clock skew.
    /// Transaction identity, amount, recipient, and authorization-window checks
    /// remain the responsibility of the evidence adapter that produced it.
    /// Domain-separated AEAD context shared by signing and crash-recovery paths.
    #[must_use]
    pub fn outbound_credential_context(intent_id: &str) -> String {
        format!("payment:{intent_id}:outbound-credential")
    }
}
#[cfg(test)]
#[path = "../../tests/util/validation_test.rs"]
mod tests;
