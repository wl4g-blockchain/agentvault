use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AssetIdentity, Error, OperationContext, PaymentChallenge, PaymentCredential, PaymentIntent, PaymentReceipt, Result,
};

/// One protected inbound business operation. The run ID and request hash stay
/// stable across every legal retry.
#[derive(Clone, Debug, PartialEq)]
pub struct PayeeOperation {
    pub scenario_id: String,
    pub run_id: String,
    pub offer_id: String,
    pub resource: String,
    pub origin: String,
    pub protocol: String,
    pub request_hash: String,
    pub credential_payload: Option<Value>,
}

impl PayeeOperation {
    pub(crate) fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("scenario_id", self.scenario_id.as_str()),
            ("run_id", self.run_id.as_str()),
            ("offer_id", self.offer_id.as_str()),
            ("resource", self.resource.as_str()),
            ("origin", self.origin.as_str()),
            ("protocol", self.protocol.as_str()),
            ("request_hash", self.request_hash.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Error::InvalidArgument(format!("{name} cannot be blank")));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvePriceRequest {
    pub scenario_id: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentPrice {
    pub id: String,
    pub scenario_id: String,
    pub resource: String,
    pub protocols: Vec<String>,
    pub asset: AssetIdentity,
    pub amount: String,
    pub wallet_id: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateChallengeRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub offer_id: String,
    pub resource: String,
    pub origin: String,
    pub protocol: String,
    pub request_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyPaymentRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub challenge_hash: String,
    pub request_hash: String,
    pub credential: PaymentCredential,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentVerification {
    pub intent: PaymentIntent,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlePaymentRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub intent_id: String,
    pub verification_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettledIncomingPayment {
    pub intent: PaymentIntent,
    pub receipt: PaymentReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpProtectionRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub resource: String,
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<PaymentCredential>,
    pub request_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpProtectionRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub tool: String,
    pub arguments_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<PaymentCredential>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProtectionOutcome {
    PaymentRequired { challenge: Box<PaymentChallenge> },
    Authorized { payment: Box<SettledIncomingPayment> },
}

#[cfg(test)]
#[path = "../../tests/model/payee_test.rs"]
mod tests;
