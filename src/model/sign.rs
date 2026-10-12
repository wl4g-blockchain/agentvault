use std::{collections::BTreeMap, str::FromStr};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{crypto::DIGEST_BYTES, Error, Result};

pub const PROTOCOL_VERSION: &str = "wallet.sign.v1";
pub const EIP712_SECP256K1: &str = "eip712-secp256k1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientPolicy {
    pub client_id_prefix: String,
    pub wallet_ids: Vec<String>,
    pub purposes: Vec<String>,
}

/// Stable request contract used by Flowgent, Sigbot, and other payment agents.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentSignRequest {
    pub version: String,
    pub request_id: Uuid,
    pub client_id: String,
    pub wallet_id: String,
    pub purpose: String,
    pub scheme: String,
    pub digest_b64: String,
    pub issued_at: i64,
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentSignResponse {
    pub version: String,
    pub request_id: Uuid,
    pub client_id: String,
    pub wallet_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignOutcome {
    Succeeded,
    InvalidRequest,
    Expired,
    Conflict,
    WalletNotFound,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignTransport {
    Http,
    Local,
    Mqtt,
}

/// Durable audit metadata. Digests and signatures are deliberately excluded.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentSignRecord {
    pub event_id: Uuid,
    pub request_id: Uuid,
    pub client_id: String,
    pub wallet_id: String,
    pub purpose: String,
    pub scheme: String,
    pub transport: SignTransport,
    pub outcome: SignOutcome,
    pub error_code: Option<String>,
    pub issued_at: i64,
    pub completed_at: i64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignHistoryQuery {
    pub request_id: Option<Uuid>,
    pub client_id: Option<String>,
    pub wallet_id: Option<String>,
    pub purpose: Option<String>,
    pub outcome: Option<SignOutcome>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PaymentSignHistoryPage {
    pub items: Vec<PaymentSignRecord>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

impl PaymentSignRequest {
    /// Validates authorization, time bounds, scheme, metadata, and digest size.
    ///
    /// # Errors
    ///
    /// Returns an error when any protocol or client-policy constraint fails.
    pub fn validate(
        &self,
        now: i64,
        max_ttl: i64,
        clock_skew: i64,
        client_policies: &[ClientPolicy],
    ) -> Result<Vec<u8>> {
        if self.version != PROTOCOL_VERSION {
            return Err(Error::InvalidRequest(format!(
                "unsupported protocol version: {}",
                self.version
            )));
        }
        validate_identifier("client_id", &self.client_id)?;
        validate_identifier("wallet_id", &self.wallet_id)?;
        validate_identifier("purpose", &self.purpose)?;
        let longest_prefix = client_policies
            .iter()
            .filter(|policy| self.client_id.starts_with(&policy.client_id_prefix))
            .map(|policy| policy.client_id_prefix.len())
            .max();
        let authorized = longest_prefix.is_some_and(|prefix_length| {
            client_policies.iter().any(|policy| {
                policy.client_id_prefix.len() == prefix_length
                    && self.client_id.starts_with(&policy.client_id_prefix)
                    && policy.wallet_ids.iter().any(|wallet_id| wallet_id == &self.wallet_id)
                    && policy.purposes.iter().any(|purpose| purpose == &self.purpose)
            })
        });
        if !authorized {
            return Err(Error::InvalidRequest(
                "client is not authorized for the requested wallet and purpose".to_owned(),
            ));
        }
        if self.scheme != EIP712_SECP256K1 {
            return Err(Error::UnsupportedScheme(self.scheme.clone()));
        }
        if self.issued_at > now.saturating_add(clock_skew) {
            return Err(Error::InvalidRequest("issued_at is in the future".to_owned()));
        }
        if self.expires_at < now.saturating_sub(clock_skew) {
            return Err(Error::RequestExpired);
        }
        let validity = self
            .expires_at
            .checked_sub(self.issued_at)
            .filter(|validity| *validity > 0 && *validity <= max_ttl);
        if validity.is_none() {
            return Err(Error::InvalidRequest("request validity window is invalid".to_owned()));
        }
        if self.metadata.len() > 16
            || self
                .metadata
                .iter()
                .any(|(key, value)| key.len() > 64 || value.len() > 256)
        {
            return Err(Error::InvalidRequest("metadata exceeds protocol limits".to_owned()));
        }

        let digest = BASE64
            .decode(&self.digest_b64)
            .map_err(|_| Error::InvalidRequest("digest_b64 is not valid base64".to_owned()))?;
        if digest.len() != DIGEST_BYTES {
            return Err(Error::InvalidRequest(format!("digest must be {DIGEST_BYTES} bytes")));
        }
        Ok(digest)
    }

    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        use sha2::{Digest as _, Sha256};

        let mut fingerprint = Sha256::new();
        hash_field(&mut fingerprint, self.version.as_bytes());
        hash_field(&mut fingerprint, self.request_id.as_bytes());
        hash_field(&mut fingerprint, self.client_id.as_bytes());
        hash_field(&mut fingerprint, self.wallet_id.as_bytes());
        hash_field(&mut fingerprint, self.purpose.as_bytes());
        hash_field(&mut fingerprint, self.scheme.as_bytes());
        hash_field(&mut fingerprint, self.digest_b64.as_bytes());
        hash_field(&mut fingerprint, &self.issued_at.to_be_bytes());
        hash_field(&mut fingerprint, &self.expires_at.to_be_bytes());
        for (key, value) in &self.metadata {
            hash_field(&mut fingerprint, key.as_bytes());
            hash_field(&mut fingerprint, value.as_bytes());
        }
        fingerprint.finalize().into()
    }
}

impl PaymentSignResponse {
    #[must_use]
    pub fn success(request: &PaymentSignRequest, address: String, signature: String, signed_at: i64) -> Self {
        Self {
            version: PROTOCOL_VERSION.to_owned(),
            request_id: request.request_id,
            client_id: request.client_id.clone(),
            wallet_id: request.wallet_id.clone(),
            address: Some(address),
            signature: Some(signature),
            signed_at: Some(signed_at),
            error: None,
        }
    }

    #[must_use]
    pub fn failure(request: &PaymentSignRequest, error: &Error) -> Self {
        let (code, message) = protocol_error(error);
        Self {
            version: PROTOCOL_VERSION.to_owned(),
            request_id: request.request_id,
            client_id: request.client_id.clone(),
            wallet_id: request.wallet_id.clone(),
            address: None,
            signature: None,
            signed_at: None,
            error: Some(ProtocolError {
                code: code.to_owned(),
                message,
            }),
        }
    }

    #[must_use]
    pub fn outcome(&self) -> SignOutcome {
        match &self.error {
            None => SignOutcome::Succeeded,
            Some(error) => match error.code.as_str() {
                "INVALID_REQUEST" | "UNSUPPORTED_SCHEME" => SignOutcome::InvalidRequest,
                "REQUEST_EXPIRED" => SignOutcome::Expired,
                "REQUEST_ID_CONFLICT" => SignOutcome::Conflict,
                "WALLET_NOT_FOUND" => SignOutcome::WalletNotFound,
                _ => SignOutcome::Failed,
            },
        }
    }
}

impl PaymentSignRecord {
    #[must_use]
    pub fn from_exchange(
        request: &PaymentSignRequest,
        response: &PaymentSignResponse,
        transport: SignTransport,
        completed_at: i64,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            request_id: request.request_id,
            client_id: request.client_id.clone(),
            wallet_id: request.wallet_id.clone(),
            purpose: request.purpose.clone(),
            scheme: request.scheme.clone(),
            transport,
            outcome: response.outcome(),
            error_code: response.error.as_ref().map(|error| error.code.clone()),
            issued_at: request.issued_at,
            completed_at,
        }
    }
}

impl SignOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::InvalidRequest => "invalid_request",
            Self::Expired => "expired",
            Self::Conflict => "conflict",
            Self::WalletNotFound => "wallet_not_found",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for SignOutcome {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "succeeded" => Ok(Self::Succeeded),
            "invalid_request" => Ok(Self::InvalidRequest),
            "expired" => Ok(Self::Expired),
            "conflict" => Ok(Self::Conflict),
            "wallet_not_found" => Ok(Self::WalletNotFound),
            "failed" => Ok(Self::Failed),
            _ => Err(Error::InvalidRequest("unknown sign history outcome".to_owned())),
        }
    }
}

impl SignTransport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Local => "local",
            Self::Mqtt => "mqtt",
        }
    }
}

fn hash_field(hasher: &mut sha2::Sha256, value: &[u8]) {
    use sha2::Digest as _;

    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(length.to_be_bytes());
    hasher.update(value);
}

/// Validates an identifier used in protocol and routing configuration.
///
/// # Errors
///
/// Returns an error for empty, oversized, or unsafe identifiers.
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

fn protocol_error(error: &Error) -> (&'static str, String) {
    match error {
        Error::InvalidRequest(message) => ("INVALID_REQUEST", message.clone()),
        Error::RequestExpired => ("REQUEST_EXPIRED", "signing request expired".to_owned()),
        Error::RequestConflict => (
            "REQUEST_ID_CONFLICT",
            "request ID was reused for different signing data".to_owned(),
        ),
        Error::UnsupportedScheme(scheme) => ("UNSUPPORTED_SCHEME", format!("unsupported signing scheme: {scheme}")),
        Error::KeyNotFound(_) => ("WALLET_NOT_FOUND", "wallet key not found".to_owned()),
        _ => ("SIGNING_FAILED", "wallet could not sign the request".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(now: i64) -> PaymentSignRequest {
        PaymentSignRequest {
            version: PROTOCOL_VERSION.to_owned(),
            request_id: Uuid::new_v4(),
            client_id: "flowgent.tm-1".to_owned(),
            wallet_id: "payer".to_owned(),
            purpose: "x402.payment".to_owned(),
            scheme: EIP712_SECP256K1.to_owned(),
            digest_b64: BASE64.encode([7_u8; 32]),
            issued_at: now,
            expires_at: now + 30,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn validates_bounded_request() {
        let now = 1_700_000_000;
        let digest = request(now).validate(now, 30, 5, &[client_policy()]).unwrap();
        assert_eq!(digest, vec![7_u8; 32]);
    }

    #[test]
    fn rejects_expired_request() {
        let now = 1_700_000_100;
        assert!(matches!(
            request(now - 100).validate(now, 30, 0, &[client_policy()]),
            Err(Error::RequestExpired)
        ));
    }

    #[test]
    fn most_specific_policy_cannot_fall_back() {
        let now = 1_700_000_000;
        let mut request = request(now);
        request.client_id = "flowgent-admin-1".to_owned();
        let policies = vec![
            ClientPolicy {
                client_id_prefix: "flowgent-".to_owned(),
                wallet_ids: vec!["payer".to_owned()],
                purposes: vec!["x402.payment".to_owned()],
            },
            ClientPolicy {
                client_id_prefix: "flowgent-admin-".to_owned(),
                wallet_ids: vec!["treasury".to_owned()],
                purposes: vec!["x402.payment".to_owned()],
            },
        ];
        assert!(request.validate(now, 30, 5, &policies).is_err());
    }

    fn client_policy() -> ClientPolicy {
        ClientPolicy {
            client_id_prefix: "flowgent".to_owned(),
            wallet_ids: vec!["payer".to_owned()],
            purposes: vec!["x402.payment".to_owned()],
        }
    }
}
