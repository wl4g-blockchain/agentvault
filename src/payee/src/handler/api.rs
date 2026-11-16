use super::{PriceResolver, SettlementCoordinator};
use crate::apm::AgentVaultMetrics;
use crate::handler::dto::CreateChallengeRequest;
use crate::handler::dto::PaymentVerification;
use crate::handler::dto::ResolvePriceRequest;
use crate::handler::dto::SettlePaymentRequest;
use crate::handler::dto::SettledPaymentResponse;
use crate::handler::dto::VerifyPaymentRequest;
use crate::handler::settlement::IssuePaymentChallenge;
use crate::handler::settlement::SettleIncomingPayment;
use crate::handler::settlement::VerifyIncomingPayment;
use crate::model::PaymentChallenge;
use crate::model::PaymentPrice;
use crate::storage::IPriceRepository;
use crate::Error;
use crate::Result;
use agentvault_core::handler::scenario::ScenarioHandler;
use std::sync::Arc;
pub struct PayeeApi {
    pub scenarios: Arc<ScenarioHandler>,
    pub prices: Arc<dyn IPriceRepository>,
    pub settlement_handler: Arc<SettlementCoordinator>,
    pub price_resolver: Arc<PriceResolver>,
    pub metrics: AgentVaultMetrics,
}
impl PayeeApi {
    pub(crate) async fn resolve_price(&self, principal: &str, request: &ResolvePriceRequest) -> Result<PaymentPrice> {
        let protocol = if let Some(protocol) = request.protocol.as_deref() {
            protocol.to_owned()
        } else {
            let _ = self.scenarios.authorize(principal, &request.scenario_id).await?;
            let price = self
                .prices
                .resolve_resource(&request.scenario_id, &request.resource)
                .await?
                .ok_or_else(|| Error::NotFound("payment price".to_owned()))?;
            price
                .protocols
                .first()
                .cloned()
                .ok_or_else(|| Error::Storage("payment price has no protocol".to_owned()))?
        };
        self.price_resolver
            .resolve(principal, &request.scenario_id, &request.resource, &protocol)
            .await
    }
    pub(crate) async fn create_challenge(
        &self,
        principal: &str,
        request: &CreateChallengeRequest,
        now: i64,
    ) -> Result<PaymentChallenge> {
        let mut operation_metric = self.metrics.payment_operation("authorize", "inbound");
        request.context.validate()?;
        let challenge = self
            .settlement_handler
            .issue_challenge(
                principal,
                &IssuePaymentChallenge {
                    scenario_id: request.context.scenario_id.clone(),
                    run_id: request.context.run_id.clone(),
                    price_id: request.price_id.clone(),
                    resource: request.resource.clone(),
                    origin: request.origin.clone(),
                    protocol: request.protocol.clone(),
                    request_hash: request.request_hash.clone(),
                },
                now,
            )
            .await?;
        operation_metric.succeed();
        Ok(challenge)
    }
    pub(crate) async fn verify_payment(
        &self,
        principal: &str,
        request: &VerifyPaymentRequest,
        now: i64,
    ) -> Result<PaymentVerification> {
        let mut operation_metric = self.metrics.payment_operation("verify", "inbound");
        request.context.validate()?;
        let verification = self
            .settlement_handler
            .verify_payment(
                principal,
                &VerifyIncomingPayment {
                    scenario_id: request.context.scenario_id.clone(),
                    run_id: request.context.run_id.clone(),
                    challenge_hash: request.challenge_hash.clone(),
                    request_hash: request.request_hash.clone(),
                    credential: request.credential.clone().into_domain(),
                },
                now,
            )
            .await?;
        let response = PaymentVerification {
            intent: verification.intent,
            valid: verification.valid,
            verification_ref: verification.verification_ref,
        };
        operation_metric.succeed();
        Ok(response)
    }
    pub(crate) async fn settle_payment(
        &self,
        principal: &str,
        request: &SettlePaymentRequest,
        now: i64,
    ) -> Result<SettledPaymentResponse> {
        let mut operation_metric = self.metrics.payment_operation("settle", "inbound");
        request.context.validate()?;
        let settled = self
            .settlement_handler
            .settle_payment(
                principal,
                &SettleIncomingPayment {
                    scenario_id: request.context.scenario_id.clone(),
                    run_id: request.context.run_id.clone(),
                    intent_id: request.intent_id.clone(),
                    verification_ref: request.verification_ref.clone(),
                },
                now,
            )
            .await?;
        let response = SettledPaymentResponse {
            intent: settled.intent,
            receipt: settled.receipt,
        };
        operation_metric.succeed();
        Ok(response)
    }
}
