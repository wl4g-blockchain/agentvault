//! Typed payment API projections for the standard HTTP endpoints.
use crate::{
    model::Amount, model::ApprovalDecisionKind, model::ApprovalRequest, model::AssetIdentity, model::BudgetUnit,
    model::PaymentAuthorizationResult, model::PaymentCredential, model::PaymentDirection, model::PaymentIntent,
    model::PaymentPolicy, model::PaymentPolicyLimits, model::PaymentReceipt, model::PaymentScenario,
    model::PaymentStatus, storage::LedgerQuery, storage::PageRequest, storage::PaymentQuery, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
pub const PAGE_SIZE: u32 = 1_000;
const DEFAULT_PAGE_SIZE: u32 = 10;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationContextDto {
    pub scenario_id: String,
    pub run_id: String,
}

impl OperationContextDto {
    ///
    /// # Errors
    /// Returns an invalid-request error for an unsafe scenario or run identifier.
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("scenario_id", self.scenario_id.as_str()),
            ("run_id", self.run_id.as_str()),
        ] {
            Self::validate_identifier(name, value)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentRequiredDto {
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizePaymentRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub challenge: PaymentRequiredDto,
}

#[derive(Clone, Serialize)]
pub struct AuthorizePaymentResponse {
    pub authorization: PaymentAuthorizationResult,
    pub intent: PaymentIntent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<PaymentCredentialDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportOutboundResponseRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub protocol: String,
    pub evidence: ProtocolEvidenceDto,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolEvidenceDto {
    pub header_name: String,
    pub header_value: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportOutboundResponseResponse {
    pub intent: PaymentIntent,
    pub accepted: bool,
    pub replayed: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentCredentialDto {
    pub protocol: String,
    pub challenge_hash: String,
    pub credential_hash: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpCredentialDto>,
}

impl PaymentCredentialDto {
    #[must_use]
    pub fn into_domain(self) -> PaymentCredential {
        PaymentCredential {
            protocol: self.protocol,
            challenge_hash: self.challenge_hash,
            credential_hash: self.credential_hash,
            payload: self.payload,
        }
    }

    ///
    /// # Errors
    /// Returns a protocol error when the credential cannot be bound to the HTTP origin.
    pub fn for_http(
        credential: PaymentCredential,
        origin: &str,
        protocol: &dyn crate::model::protocol::PaymentProtocol,
    ) -> Result<Self> {
        if credential.protocol != protocol.name() {
            return Err(Error::PaymentProtocol(
                "HTTP credential adapter does not match its protocol".to_owned(),
            ));
        }
        let http = protocol.encode_http_credential(&credential)?;
        Ok(Self {
            protocol: credential.protocol,
            challenge_hash: credential.challenge_hash,
            credential_hash: credential.credential_hash,
            payload: credential.payload,
            http: Some(HttpCredentialDto {
                bound_origin: origin.to_owned(),
                header_name: http.header_name,
                header_value: http.header_value,
            }),
        })
    }
}

impl std::fmt::Debug for PaymentCredentialDto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaymentCredentialDto")
            .field("protocol", &self.protocol)
            .field("challenge_hash", &"<redacted>")
            .field("credential_hash", &"<redacted>")
            .field("payload", &"<redacted>")
            .field("http", &self.http)
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpCredentialDto {
    pub bound_origin: String,
    pub header_name: String,
    pub header_value: String,
}

impl std::fmt::Debug for HttpCredentialDto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpCredentialDto")
            .field("bound_origin", &self.bound_origin)
            .field("header_name", &self.header_name)
            .field("header_value", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct ScenarioQuery {
    pub scenario_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateScenarioRequest {
    pub id: String,
    pub client_id: String,
    pub name: String,
    pub policy: InitialPolicyRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialPolicyRequest {
    pub id: String,
    pub wallet_ids: Vec<String>,
    pub protocols: Vec<String>,
    pub networks: Vec<String>,
    pub assets: Vec<AssetIdentity>,
    pub budget_unit: BudgetUnit,
    #[serde(default)]
    pub allowed_recipients: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub inbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default)]
    pub outbound_limits: Option<PaymentPolicyLimits>,
    #[serde(default = "InitialPolicyRequest::enabled_by_default")]
    pub enabled: bool,
}

impl InitialPolicyRequest {
    #[must_use]
    pub const fn enabled_by_default() -> bool {
        true
    }
}

impl CreateScenarioRequest {
    #[must_use]
    pub fn into_domain(self, now: i64) -> (PaymentScenario, PaymentPolicy) {
        let mut wallet_ids = self.policy.wallet_ids;
        wallet_ids.sort_unstable();
        wallet_ids.dedup();
        let scenario = PaymentScenario {
            id: self.id,
            client_id: self.client_id,
            name: self.name,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let policy = PaymentPolicy {
            id: self.policy.id,
            scenario_id: scenario.id.clone(),
            wallet_ids,
            protocols: self.policy.protocols,
            networks: self.policy.networks,
            assets: self.policy.assets,
            budget_unit: self.policy.budget_unit,
            allowed_recipients: self.policy.allowed_recipients,
            allowed_origins: self.policy.allowed_origins,
            inbound_limits: self.policy.inbound_limits,
            outbound_limits: self.policy.outbound_limits,
            enabled: self.policy.enabled,
            created_at: now,
            updated_at: now,
        };
        (scenario, policy)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct AdminPageQuery {
    pub offset: Option<u64>,
    pub limit: Option<u32>,
}

impl AdminPageQuery {
    ///
    /// # Errors
    /// Returns an invalid-request error for an unsafe pagination range.
    pub fn page(self) -> Result<PageRequest> {
        let page = PageRequest {
            offset: self.offset.unwrap_or(0),
            limit: self.limit.unwrap_or(DEFAULT_PAGE_SIZE),
        };
        if page.limit == 0 || page.limit > PAGE_SIZE {
            return Err(Error::InvalidRequest(format!(
                "page limit must be between 1 and {PAGE_SIZE}"
            )));
        }
        Ok(page)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PaymentListQuery {
    pub scenario_id: Option<String>,
    pub wallet_id: Option<String>,
    pub run_id: Option<String>,
    pub direction: Option<PaymentDirection>,
    pub status: Option<PaymentStatus>,
    pub from_created_at: Option<i64>,
    pub to_created_at: Option<i64>,
    #[serde(flatten)]
    pub page: AdminPageQuery,
}

impl PaymentListQuery {
    ///
    /// # Errors
    /// Returns an invalid-request error for inconsistent query filters or pagination.
    pub fn into_domain(self) -> Result<PaymentQuery> {
        Ok(PaymentQuery {
            scenario_id: self.scenario_id,
            wallet_id: self.wallet_id,
            run_id: self.run_id,
            direction: self.direction,
            status: self.status,
            from_created_at: self.from_created_at,
            to_created_at: self.to_created_at,
            page: self.page.page()?,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LedgerListQuery {
    pub scenario_id: Option<String>,
    pub wallet_id: Option<String>,
    pub intent_id: Option<String>,
    pub direction: Option<PaymentDirection>,
    pub network: Option<String>,
    pub asset: Option<String>,
    pub from_effective_at: Option<i64>,
    pub to_effective_at: Option<i64>,
    #[serde(flatten)]
    pub page: AdminPageQuery,
}

impl LedgerListQuery {
    ///
    /// # Errors
    /// Returns an invalid-request error for inconsistent query filters or pagination.
    pub fn into_domain(self) -> Result<LedgerQuery> {
        Ok(LedgerQuery {
            scenario_id: self.scenario_id,
            wallet_id: self.wallet_id,
            intent_id: self.intent_id,
            direction: self.direction,
            network: self.network,
            asset: self.asset,
            from_effective_at: self.from_effective_at,
            to_effective_at: self.to_effective_at,
            page: self.page.page()?,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct ClientPageQuery {
    pub client_id: String,
    #[serde(flatten)]
    pub page: AdminPageQuery,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioPageQuery {
    pub scenario_id: String,
    #[serde(flatten)]
    pub page: AdminPageQuery,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BalanceQuery {
    pub scenario_id: String,
    pub wallet_id: String,
    pub network: String,
    pub asset: String,
    pub token_contract: Option<String>,
    pub decimals: u8,
}

impl BalanceQuery {
    #[must_use]
    pub fn asset_identity(&self) -> AssetIdentity {
        AssetIdentity {
            network: self.network.clone(),
            asset: self.asset.clone(),
            token_contract: self.token_contract.clone(),
            decimals: self.decimals,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PaymentBalance {
    pub scenario_id: String,
    pub wallet_id: String,
    pub asset: AssetIdentity,
    pub confirmed_inbound: Amount,
    pub confirmed_outbound: Amount,
    pub reserved_outbound: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<Amount>,
    pub as_of: i64,
}

#[derive(Debug, Deserialize)]
pub struct ApprovalQuery {
    pub scenario_id: String,
    pub status: Option<String>,
    pub offset: Option<u64>,
    pub limit: Option<u32>,
}

impl ApprovalQuery {
    ///
    /// # Errors
    /// Returns an invalid-request error for an unsafe pagination range.
    pub fn page(&self) -> Result<PageRequest> {
        let limit = self.limit.unwrap_or(DEFAULT_PAGE_SIZE);
        if limit == 0 || limit > PAGE_SIZE {
            return Err(Error::InvalidRequest(format!(
                "approval page limit must be between 1 and {PAGE_SIZE}"
            )));
        }
        Ok(PageRequest {
            offset: self.offset.unwrap_or_default(),
            limit,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ApprovalPage {
    pub items: Vec<ApprovalRequest>,
    pub total: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitApprovalDecisionRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub approval_id: String,
    pub decision: ApprovalDecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_amount: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

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

impl OperationContextDto {
    ///
    /// # Errors
    /// Returns an invalid-request error for an empty, oversized or unsafe identifier.
    pub fn validate_identifier(name: &str, value: &str) -> Result<()> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        {
            return Err(Error::InvalidRequest(format!("{name} is invalid")));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/model/payment_api_test.rs"]
mod tests;
