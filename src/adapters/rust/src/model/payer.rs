use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    model_base::validate_required, ApprovalRequest, AssetIdentity, Error, OperationContext, PaymentCredential,
    PaymentIntent, Result,
};

/// Stable identity for one logical workflow operation. Reuse it unchanged for
/// every application retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayerRequestContext {
    pub run_id: String,
    pub idempotency_key: crate::IdempotencyKey,
}

impl PayerRequestContext {
    pub fn for_operation(run_id: impl Into<String>, operation: &str) -> Result<Self> {
        let run_id = run_id.into();
        let idempotency_key = crate::IdempotencyKey::for_operation(&run_id, operation)?;
        Ok(Self {
            run_id,
            idempotency_key,
        })
    }

    pub fn new(run_id: impl Into<String>, idempotency_key: crate::IdempotencyKey) -> Result<Self> {
        let run_id = run_id.into();
        if run_id.trim().is_empty() {
            return Err(Error::InvalidArgument("run_id cannot be blank".to_owned()));
        }
        Ok(Self {
            run_id,
            idempotency_key,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentRequired {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_hint: Option<String>,
    pub origin: String,
    pub resource: String,
    pub content_type: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizePaymentRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub wallet_id: String,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub amount: String,
    pub recipient: String,
    pub resource: String,
    pub challenge: PaymentRequired,
}

impl AuthorizePaymentRequest {
    pub(crate) fn validate(&self) -> Result<()> {
        self.context.validate_with_run_id()?;
        for (name, value) in [
            ("wallet_id", self.wallet_id.as_str()),
            ("protocol", self.protocol.as_str()),
            ("asset.network", self.asset.network.as_str()),
            ("asset.asset", self.asset.asset.as_str()),
            ("amount", self.amount.as_str()),
            ("recipient", self.recipient.as_str()),
            ("resource", self.resource.as_str()),
            ("challenge.origin", self.challenge.origin.as_str()),
            ("challenge.resource", self.challenge.resource.as_str()),
            ("challenge.content_type", self.challenge.content_type.as_str()),
        ] {
            validate_required(name, value)?;
        }
        if self.resource != self.challenge.resource {
            return Err(Error::InvalidArgument(
                "resource must exactly match the challenge resource".to_owned(),
            ));
        }
        if self
            .challenge
            .protocol_hint
            .as_ref()
            .is_some_and(|protocol| protocol != &self.protocol)
        {
            return Err(Error::InvalidArgument(
                "protocol must match the 402 protocol hint".to_owned(),
            ));
        }
        if self.challenge.origin != self.challenge.origin.trim() {
            return Err(Error::InvalidArgument(
                "challenge origin cannot contain surrounding whitespace".to_owned(),
            ));
        }
        if !http_url_matches_origin(&self.challenge.resource, &self.challenge.origin) {
            return Err(Error::InvalidArgument(
                "challenge resource URL does not match its canonical HTTP(S) origin".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn http_url_matches_origin(url: &str, origin: &str) -> bool {
    let authority = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"));
    let valid_origin = authority.is_some_and(|value| {
        !value.is_empty()
            && !value
                .chars()
                .any(|character| matches!(character, '/' | '?' | '#' | '@'))
    });
    let valid_url = url.strip_prefix(origin).is_some_and(|suffix| {
        suffix.is_empty() || suffix.starts_with('/') || suffix.starts_with('?') || suffix.starts_with('#')
    });
    valid_origin && valid_url
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PaymentAuthorizationResult {
    Denied { intent_id: String, reason: String },
    RequiresApproval { intent_id: String, approval_id: String },
    Authorized { intent_id: String, reserved_amount: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorizePaymentResponse {
    pub authorization: PaymentAuthorizationResult,
    pub intent: PaymentIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<PaymentCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportOutboundResponseRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub payment_response: String,
}

impl ReportOutboundResponseRequest {
    pub(crate) fn validate(&self) -> Result<()> {
        self.context.validate_with_run_id()?;
        if self.payment_response.is_empty()
            || self.payment_response.len() > 16 * 1024
            || !self.payment_response.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(Error::InvalidArgument(
                "payment_response must be a bounded official x402 header value".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportOutboundResponseResponse {
    pub intent: PaymentIntent,
    pub accepted: bool,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceQuery {
    pub scenario_id: String,
    pub wallet_id: String,
    pub asset: AssetIdentity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentBalance {
    pub scenario_id: String,
    pub wallet_id: String,
    pub asset: AssetIdentity,
    pub confirmed_inbound: String,
    pub confirmed_outbound: String,
    pub reserved_outbound: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<String>,
    pub as_of: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PayerHttpRequest {
    pub method: String,
    pub url: String,
    pub origin: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub follow_redirects: bool,
}

impl fmt::Debug for PayerHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PayerHttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("origin", &self.origin)
            .field("headers", &"[REDACTED]")
            .field("body", &"[REDACTED]")
            .field("follow_redirects", &self.follow_redirects)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayerHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Intent-bound material returned by `AgentVault` after policy, approval, and
/// signing complete.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorizedPayment {
    pub intent: PaymentIntent,
    pub credential: PaymentCredential,
}

#[cfg(test)]
#[path = "../../tests/model/payer_test.rs"]
mod tests;
