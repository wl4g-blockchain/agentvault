//! Strict versioned MQTT transport DTOs; business commands use shared API DTOs.
use crate::model::payment_api::{ReportOutboundResponseRequest, SubmitApprovalDecisionRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
pub(crate) struct OutboundResponseCommand {
    pub(crate) intent_id: String,
    pub(crate) request: ReportOutboundResponseRequest,
}

#[derive(Debug)]
pub(crate) struct ApprovalDecisionCommand {
    pub(crate) request: SubmitApprovalDecisionRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RequestEnvelope {
    #[serde(rename = "type")]
    pub(crate) message_type: String,
    pub(crate) version: u8,
    pub(crate) request_span_id: String,
    pub(crate) request: WireRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResponseEnvelope {
    #[serde(rename = "type")]
    pub(crate) message_type: String,
    pub(crate) version: u8,
    pub(crate) request_span_id: String,
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PaymentEventEnvelope<'a> {
    #[serde(rename = "type")]
    pub(crate) message_type: &'static str,
    pub(crate) version: u8,
    pub(crate) run_id: &'a str,
    pub(crate) aggregate_type: &'a str,
    pub(crate) aggregate_id: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) payload: &'a Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovalRequiredEnvelope {
    #[serde(rename = "type")]
    pub(crate) message_type: &'static str,
    pub(crate) version: u8,
    pub(crate) approval_id: String,
    pub(crate) intent_id: String,
    pub(crate) run_id: String,
    pub(crate) scenario_id: String,
    pub(crate) requested_amount: String,
    pub(crate) expires_at: i64,
}

#[cfg(test)]
#[path = "../../tests/model/mqtt_test.rs"]
mod tests;
