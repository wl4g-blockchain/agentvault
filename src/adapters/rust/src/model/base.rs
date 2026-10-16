use std::{error, fmt};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidArgument(String),
    Transport(String),
    Encode(String),
    Decode(String),
    Entropy(String),
    Api { status: u16, code: String, message: String },
    Conflict { code: String, message: String },
    UnsafeRetry(String),
}

impl Error {
    #[must_use]
    pub const fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) => write!(formatter, "invalid argument: {message}"),
            Self::Transport(message) => write!(formatter, "transport failed: {message}"),
            Self::Encode(message) => write!(formatter, "request encoding failed: {message}"),
            Self::Decode(message) => write!(formatter, "response decoding failed: {message}"),
            Self::Entropy(message) => write!(formatter, "secure randomness failed: {message}"),
            Self::Api { status, code, message } => {
                write!(formatter, "AgentVault API error {status} ({code}): {message}")
            }
            Self::Conflict { code, message } => {
                write!(formatter, "AgentVault idempotency conflict ({code}): {message}")
            }
            Self::UnsafeRetry(message) => write!(formatter, "unsafe paid HTTP retry rejected: {message}"),
        }
    }
}

impl error::Error for Error {}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_idempotency_key(&value)?;
        Ok(Self(value))
    }

    pub fn for_operation(run_id: &str, operation: &str) -> Result<Self> {
        validate_required("run_id", run_id)?;
        validate_required("operation", operation)?;
        if run_id.contains(':') || operation.contains(':') {
            return Err(Error::InvalidArgument(
                "run_id and operation cannot contain ':' when deriving an idempotency key".to_owned(),
            ));
        }
        Self::new(format!("{run_id}:{operation}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("IdempotencyKey").field(&self.0).finish()
    }
}

impl Serialize for IdempotencyKey {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

fn validate_idempotency_key(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 {
        return Err(Error::InvalidArgument(
            "idempotency_key must contain 1..=128 characters".to_owned(),
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(Error::InvalidArgument(
            "idempotency_key may contain only ASCII letters, digits, '-', '_', '.', and ':'".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationContext {
    pub scenario_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub idempotency_key: IdempotencyKey,
}

impl OperationContext {
    pub fn new(scenario_id: impl Into<String>, idempotency_key: IdempotencyKey) -> Result<Self> {
        let scenario_id = scenario_id.into();
        validate_required("scenario_id", &scenario_id)?;
        Ok(Self {
            scenario_id,
            run_id: None,
            idempotency_key,
        })
    }

    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_required("scenario_id", &self.scenario_id)?;
        if self.run_id.as_ref().is_some_and(|value| value.trim().is_empty()) {
            return Err(Error::InvalidArgument("run_id cannot be blank".to_owned()));
        }
        validate_idempotency_key(self.idempotency_key.as_str())
    }

    pub(crate) fn validate_with_run_id(&self) -> Result<()> {
        self.validate()?;
        if self.run_id.is_none() {
            return Err(Error::InvalidArgument(
                "run_id is required for payment operations".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_required(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::InvalidArgument(format!("{name} cannot be blank")));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Created,
    PendingApproval,
    Authorized,
    Signed,
    Submitted,
    PendingSettlement,
    Settled,
    Failed,
    Expired,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    Unreserved,
    Reserved,
    Consumed,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetIdentity {
    pub network: String,
    pub asset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_contract: Option<String>,
    pub decimals: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentChallenge {
    pub scenario_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_id: Option<String>,
    pub resource: String,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub amount: String,
    pub recipient: String,
    pub nonce: String,
    pub challenge_hash: String,
    pub expires_at: i64,
    pub payload: Value,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpCredential {
    pub bound_origin: String,
    pub header_name: String,
    pub header_value: String,
}

impl fmt::Debug for HttpCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpCredential")
            .field("bound_origin", &self.bound_origin)
            .field("header_name", &self.header_name)
            .field("header_value", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentCredential {
    pub protocol: String,
    pub challenge_hash: String,
    pub credential_hash: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpCredential>,
}

impl fmt::Debug for PaymentCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaymentCredential")
            .field("protocol", &self.protocol)
            .field("challenge_hash", &"[REDACTED]")
            .field("credential_hash", &"[REDACTED]")
            .field("payload", &"[REDACTED]")
            .field("http", &self.http)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentIntent {
    pub id: String,
    pub scenario_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_id: Option<String>,
    pub direction: PaymentDirection,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub recipient: String,
    pub amount: String,
    pub settled_amount: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_hash: Option<String>,
    pub idempotency_key: String,
    pub request_hash: String,
    pub status: PaymentStatus,
    pub reservation_status: ReservationStatus,
    pub reserved_amount: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_reconcile_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentReceipt {
    pub intent_id: String,
    pub scenario_id: String,
    pub direction: PaymentDirection,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub recipient: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    pub requested_amount: String,
    pub settled_amount: String,
    pub status: PaymentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement_ref: Option<String>,
    pub evidence_hash: String,
    pub confirmed_at: i64,
}

#[cfg(test)]
#[path = "../../tests/model/base_test.rs"]
mod tests;
