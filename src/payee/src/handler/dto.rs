//! Payee HTTP request and response projections.
use crate::model::{OperationContextDto, PaymentCredentialDto, PaymentIntent, PaymentReceipt};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvePriceRequest {
    pub scenario_id: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChallengeRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub price_id: String,
    pub resource: String,
    pub origin: String,
    pub protocol: String,
    pub request_hash: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyPaymentRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub challenge_hash: String,
    pub request_hash: String,
    pub credential: PaymentCredentialDto,
}
#[derive(Clone, Debug, Serialize)]
pub struct PaymentVerification {
    pub intent: PaymentIntent,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_ref: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettlePaymentRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub intent_id: String,
    pub verification_ref: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct SettledPaymentResponse {
    pub intent: PaymentIntent,
    pub receipt: PaymentReceipt,
}
