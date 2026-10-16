use serde::{Deserialize, Serialize};

use crate::OperationContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    Approve,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub intent_id: String,
    pub requested_amount: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_amount: Option<String>,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub status: ApprovalStatus,
    pub intent_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_jti: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_claims_hash: Option<String>,
    pub expires_at: i64,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalQuery {
    pub scenario_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalPage {
    pub items: Vec<ApprovalRequest>,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitApprovalDecisionRequest {
    #[serde(flatten)]
    pub context: OperationContext,
    pub approval_id: String,
    pub decision: ApprovalDecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_amount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
#[path = "../../tests/model/approval_test.rs"]
mod tests;
