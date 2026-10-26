use std::{error, fmt};

use serde::{Deserialize, Serialize};
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
                write!(formatter, "AgentVault run conflict ({code}): {message}")
            }
            Self::UnsafeRetry(message) => write!(formatter, "unsafe paid HTTP retry rejected: {message}"),
        }
    }
}

impl error::Error for Error {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationContext {
    pub scenario_id: String,
    pub run_id: String,
}

impl OperationContext {
    pub fn new(scenario_id: impl Into<String>, run_id: impl Into<String>) -> Result<Self> {
        let scenario_id = scenario_id.into();
        let run_id = run_id.into();
        validate_required("scenario_id", &scenario_id)?;
        validate_identifier("run_id", &run_id)?;
        Ok(Self { scenario_id, run_id })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_required("scenario_id", &self.scenario_id)?;
        validate_identifier("run_id", &self.run_id)
    }
}

fn validate_identifier(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(Error::InvalidArgument(format!(
            "{name} must contain 1..=128 safe ASCII characters"
        )));
    }
    Ok(())
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
    pub price_id: Option<String>,
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
    pub price_id: Option<String>,
    pub direction: PaymentDirection,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub recipient: String,
    pub amount: String,
    pub settled_amount: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_hash: Option<String>,
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
