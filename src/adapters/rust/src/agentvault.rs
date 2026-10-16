use crate::{
    model_base::validate_required, percent_encode, ApiClient, ApprovalDecisionKind, ApprovalPage, ApprovalQuery,
    ApprovalRequest, Error, PayeeConfig, PayeeMiddleware, PayerConfig, PayerMiddleware, Result,
    SubmitApprovalDecisionRequest, VaultTransport,
};

pub struct AgentVault<T> {
    api: ApiClient<T>,
}

impl<T> AgentVault<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            api: ApiClient::new(transport),
        }
    }

    pub fn payer(&self, config: PayerConfig) -> Result<PayerMiddleware<T>> {
        PayerMiddleware::from_api(self.api.clone(), config)
    }

    pub fn payee(&self, config: PayeeConfig) -> Result<PayeeMiddleware<T>> {
        PayeeMiddleware::from_api(self.api.clone(), config)
    }
}

impl<T: VaultTransport> AgentVault<T> {
    pub async fn get_pending_approvals(&self, query: &ApprovalQuery) -> Result<ApprovalPage> {
        validate_required("scenario_id", &query.scenario_id)?;
        self.api
            .get(format!(
                "{}?scenario_id={}&status=pending",
                paths::APPROVALS,
                percent_encode(&query.scenario_id)
            ))
            .await
    }

    pub async fn submit_approval_decision(&self, request: &SubmitApprovalDecisionRequest) -> Result<ApprovalRequest> {
        request.context.validate()?;
        validate_required("approval_id", &request.approval_id)?;
        match (request.decision, request.approved_amount.as_deref()) {
            (ApprovalDecisionKind::Approve, Some(amount)) => validate_required("approved_amount", amount)?,
            (ApprovalDecisionKind::Approve, None) => {
                return Err(Error::InvalidArgument(
                    "approved_amount is required when approving".to_owned(),
                ));
            }
            (ApprovalDecisionKind::Reject, Some(_)) => {
                return Err(Error::InvalidArgument(
                    "approved_amount must be omitted when rejecting".to_owned(),
                ));
            }
            (ApprovalDecisionKind::Reject, None) => {}
        }
        self.api
            .post(
                format!("{}/{}/decision", paths::APPROVALS, percent_encode(&request.approval_id)),
                request,
                Some(&request.context.idempotency_key),
            )
            .await
    }

    pub async fn get_approval_status(&self, scenario_id: &str, approval_id: &str) -> Result<ApprovalRequest> {
        validate_required("scenario_id", scenario_id)?;
        validate_required("approval_id", approval_id)?;
        self.api
            .get(format!(
                "{}/{approval_id}?scenario_id={scenario_id}",
                paths::APPROVALS,
                approval_id = percent_encode(approval_id),
                scenario_id = percent_encode(scenario_id)
            ))
            .await
    }
}

pub(crate) mod paths {
    pub const AUTHORIZE_PAYMENT: &str = "/api/v1/payments/authorize";
    pub const PAYMENTS: &str = "/api/v1/payments";
    pub const PAYMENT_BALANCE: &str = "/api/v1/payments/balance";
    pub const APPROVALS: &str = "/api/v1/approvals";
    pub const RESOLVE_OFFER: &str = "/api/v1/offers/resolve";
    pub const CREATE_CHALLENGE: &str = "/api/v1/payee/challenges";
    pub const VERIFY_PAYMENT: &str = "/api/v1/payee/verify";
    pub const SETTLE_PAYMENT: &str = "/api/v1/payee/settle";
}
