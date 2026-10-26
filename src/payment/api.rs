//! Authenticated payment API shared by HTTP and message transports.
//!
//! Each transport uses the same typed projection and application orchestration.

use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use reqwest::Url;

use crate::apm::AgentVaultMetrics;
use crate::handler::{
    ApprovalHandler, EventHandler, IntentHandler, LedgerHandler, PayClientHandler, PolicyHandler, PriceHandler,
    ScenarioHandler, WalletHandler,
};
use crate::model::Amount;
use crate::model::ApprovalDecisionKind;
use crate::model::ApprovalRequest;
use crate::model::AuthorizePayment;
use crate::model::IssuePaymentChallenge;
use crate::model::LedgerEntryType;
use crate::model::PaymentAuthorizationResult;
use crate::model::PaymentChallenge;
use crate::model::PaymentDirection;
use crate::model::PaymentEventType;
use crate::model::PaymentIntent;
use crate::model::PaymentLedgerEntry;
use crate::model::PaymentPrice;
use crate::model::PaymentReceipt;
use crate::model::RecordOutboundPaymentResponse;
use crate::model::ReservationStatus;
use crate::model::SettleIncomingPayment;
use crate::model::SignAuthorizedPayment;
use crate::model::SubmitApprovalDecision;
use crate::model::VerifyIncomingPayment;
use crate::payment::ApprovalPrincipalAuthorizer;
use crate::payment::ApprovalWorkflow;
use crate::payment::MerchantEvidenceRecorder;
use crate::payment::OutboundSigner;
use crate::payment::PriceResolver;
use crate::payment::SettlementCoordinator;
use crate::payment::WalletBalanceSource;
use crate::payment::WalletImportService;
use crate::protocol::ProtocolRegistry;
use crate::storage::IApprovalRepository;
use crate::storage::IClientRepository;
use crate::storage::IEventRepository;
use crate::storage::IIntentRepository;
use crate::storage::ILedgerRepository;
use crate::storage::IPolicyRepository;
use crate::storage::IPriceRepository;
use crate::storage::IWalletRepository;
use crate::storage::LedgerQuery;
use crate::storage::PageRequest;
use crate::storage::PaymentQuery;
use crate::Error;
use crate::Result;

use crate::model::payment_api::{
    ApprovalPage, AuthorizePaymentRequest, AuthorizePaymentResponse, BalanceQuery, CreateChallengeRequest,
    PaymentBalance, PaymentCredentialDto, PaymentVerification, ReportOutboundResponseRequest,
    ReportOutboundResponseResponse, ResolvePriceRequest, SettlePaymentRequest, SettledPaymentResponse,
    SubmitApprovalDecisionRequest, VerifyPaymentRequest, PAGE_SIZE,
};

/// Dependencies for the transport-neutral payment application façade.
pub struct PaymentApiDependencies {
    pub clients: Arc<dyn IClientRepository>,
    pub scenarios: Arc<ScenarioHandler>,
    pub payments_handler: Arc<crate::payment::PaymentAuthorizer>,
    pub signing_handler: Arc<OutboundSigner>,
    pub outbound_evidence_handler: Arc<MerchantEvidenceRecorder>,
    pub settlement_handler: Arc<SettlementCoordinator>,
    pub price_resolver: Arc<PriceResolver>,
    pub approval_handler: Arc<ApprovalWorkflow>,
    pub approval_authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
    pub payer_protocols: Arc<ProtocolRegistry>,
    pub payments: Arc<dyn IIntentRepository>,
    pub approvals: Arc<dyn IApprovalRepository>,
    pub prices: Arc<dyn IPriceRepository>,
    pub policies: Arc<dyn IPolicyRepository>,
    pub wallets: Arc<dyn IWalletRepository>,
    pub wallet_handler: Arc<WalletImportService>,
    pub events: Arc<dyn IEventRepository>,
    pub ledger: Arc<dyn ILedgerRepository>,
    pub balances: Arc<dyn WalletBalanceSource>,
    pub metrics: AgentVaultMetrics,
}

/// One authenticated application service used by HTTP, gRPC, and MQTT
/// adapters. Tenant identity is always supplied by trusted transport context.
pub struct PaymentApi {
    pub(crate) clients: Arc<PayClientHandler>,
    pub(crate) scenarios: Arc<ScenarioHandler>,
    pub(crate) payments_handler: Arc<crate::payment::PaymentAuthorizer>,
    pub(crate) signing_handler: Arc<OutboundSigner>,
    pub(crate) outbound_evidence_handler: Arc<MerchantEvidenceRecorder>,
    pub(crate) settlement_handler: Arc<SettlementCoordinator>,
    pub(crate) price_resolver: Arc<PriceResolver>,
    pub(crate) approval_handler: Arc<ApprovalWorkflow>,
    pub(crate) approval_authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
    pub(crate) payer_protocols: Arc<ProtocolRegistry>,
    pub(crate) payments: Arc<IntentHandler>,
    pub(crate) approvals: Arc<ApprovalHandler>,
    pub(crate) prices: Arc<PriceHandler>,
    pub(crate) policies: Arc<PolicyHandler>,
    pub(crate) wallets: Arc<WalletHandler>,
    pub(crate) wallet_handler: Arc<WalletImportService>,
    pub(crate) events: Arc<EventHandler>,
    pub(crate) ledger: Arc<LedgerHandler>,
    pub(crate) balances: Arc<dyn WalletBalanceSource>,
    pub(crate) metrics: AgentVaultMetrics,
}

impl PaymentApi {
    #[must_use]
    pub fn new(dependencies: PaymentApiDependencies) -> Self {
        Self {
            clients: Arc::new(PayClientHandler::new(dependencies.clients)),
            scenarios: dependencies.scenarios,
            payments_handler: dependencies.payments_handler,
            signing_handler: dependencies.signing_handler,
            outbound_evidence_handler: dependencies.outbound_evidence_handler,
            settlement_handler: dependencies.settlement_handler,
            price_resolver: dependencies.price_resolver,
            approval_handler: dependencies.approval_handler,
            approval_authorizer: dependencies.approval_authorizer,
            payer_protocols: dependencies.payer_protocols,
            payments: Arc::new(IntentHandler::new(dependencies.payments)),
            approvals: Arc::new(ApprovalHandler::new(dependencies.approvals)),
            prices: Arc::new(PriceHandler::new(dependencies.prices)),
            policies: Arc::new(PolicyHandler::new(dependencies.policies)),
            wallets: Arc::new(WalletHandler::new(dependencies.wallets)),
            wallet_handler: dependencies.wallet_handler,
            events: Arc::new(EventHandler::new(dependencies.events)),
            ledger: Arc::new(LedgerHandler::new(dependencies.ledger)),
            balances: dependencies.balances,
            metrics: dependencies.metrics,
        }
    }

    /// Parses, authorizes, and signs one merchant challenge. An approval
    /// replay uses the same run identity and reaches signing only after the
    /// one-time internal grant has been consumed transactionally.
    pub(crate) async fn authorize(
        &self,
        principal: &str,
        request: &AuthorizePaymentRequest,
        now: i64,
    ) -> Result<AuthorizePaymentResponse> {
        let mut operation_metric = self.metrics.payment_operation("authorize", "outbound");
        request.context.validate()?;
        let challenge = self.parse_outbound_challenge(request, now).await?;
        let authorization = self
            .payments_handler
            .authorize(
                principal,
                &AuthorizePayment {
                    scenario_id: request.context.scenario_id.clone(),
                    run_id: request.context.run_id.clone(),
                    challenge: challenge.clone(),
                },
                now,
            )
            .await?;
        let intent_id = Self::authorization_intent_id(&authorization);
        let mut intent = self
            .payments
            .get_for_scenario(&request.context.scenario_id, intent_id)
            .await?
            .ok_or_else(|| Error::Storage("authorized payment intent disappeared".to_owned()))?;
        let mut credential = None;
        let mut approval = None;
        match &authorization {
            PaymentAuthorizationResult::Authorized { .. } => {
                let mut signing_metric = self.metrics.payment_operation("sign", "outbound");
                let signed = self
                    .signing_handler
                    .sign(
                        principal,
                        &SignAuthorizedPayment {
                            intent_id: intent.id.clone(),
                            challenge,
                        },
                        now,
                    )
                    .await?;
                signing_metric.succeed();
                intent = signed.intent;
                let protocol = self.payer_protocols.get(&signed.credential.protocol)?;
                credential = Some(PaymentCredentialDto::for_http(
                    signed.credential,
                    &request.challenge.origin,
                    protocol.as_ref(),
                )?);
            }
            PaymentAuthorizationResult::RequiresApproval { approval_id, .. } => {
                approval = Some(
                    self.approvals
                        .get(approval_id)
                        .await?
                        .ok_or_else(|| Error::Storage("payment approval disappeared".to_owned()))?,
                );
            }
            PaymentAuthorizationResult::Denied { .. } => {}
        }
        let response = AuthorizePaymentResponse {
            authorization,
            intent,
            credential,
            approval,
        };
        operation_metric.succeed();
        Ok(response)
    }

    pub(crate) async fn parse_outbound_challenge(
        &self,
        request: &AuthorizePaymentRequest,
        now: i64,
    ) -> Result<PaymentChallenge> {
        request.context.validate()?;
        let origin = Self::canonical_origin(&request.challenge.resource)?;
        if request.challenge.origin != origin {
            return Err(Error::InvalidRequest(
                "opaque 402 metadata does not match the requested resource".to_owned(),
            ));
        }
        let (adapter, wire) = self.payer_protocols.resolve_http_challenge(
            request.challenge.protocol_hint.as_deref(),
            &request.challenge.resource,
            &request.challenge.headers,
            &request.challenge.body,
        )?;
        let protocol = adapter.name();
        let challenge = adapter
            .parse_challenge(
                &request.context.scenario_id,
                &request.challenge.resource,
                &wire,
                now,
                Self::parse_advertised_expiry(request.challenge.expires_at.as_deref())?,
            )
            .await?;
        if challenge.protocol != protocol || challenge.resource != request.challenge.resource {
            return Err(Error::PaymentConflict(
                "parsed protocol challenge differs from the captured 402 metadata".to_owned(),
            ));
        }
        Ok(challenge)
    }

    pub(crate) async fn get_payment(
        &self,
        principal: &str,
        scenario_id: &str,
        intent_id: &str,
    ) -> Result<PaymentIntent> {
        let _ = self.scenarios.authorize(principal, scenario_id).await?;
        self.payments
            .get_for_scenario(scenario_id, intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))
    }

    pub(crate) async fn get_receipt(
        &self,
        principal: &str,
        scenario_id: &str,
        intent_id: &str,
    ) -> Result<PaymentReceipt> {
        let intent = self.get_payment(principal, scenario_id, intent_id).await?;
        let mut offset = 0_u64;
        loop {
            let page = self
                .events
                .list_for_intent(
                    &intent.id,
                    PageRequest {
                        offset,
                        limit: PAGE_SIZE,
                    },
                )
                .await?;
            for event in page.items.into_iter().rev() {
                if matches!(
                    event.event_type,
                    PaymentEventType::Settled | PaymentEventType::Reconciled
                ) {
                    if let Some(receipt) = event.payload.get("receipt") {
                        return serde_json::from_value(receipt.clone())
                            .map_err(|_| Error::Storage("settlement event contains an invalid receipt".to_owned()));
                    }
                }
            }
            offset = offset.saturating_add(u64::from(PAGE_SIZE));
            if offset >= page.total {
                break;
            }
        }
        Err(Error::NotFound("payment receipt".to_owned()))
    }

    pub(crate) async fn report_outbound_response(
        &self,
        principal: &str,
        intent_id: &str,
        request: &ReportOutboundResponseRequest,
        now: i64,
    ) -> Result<ReportOutboundResponseResponse> {
        let mut operation_metric = self.metrics.payment_operation("settle", "outbound");
        request.context.validate()?;
        let evidence = &request.evidence;
        let recorded = self
            .outbound_evidence_handler
            .record_merchant_response(
                principal,
                &RecordOutboundPaymentResponse {
                    scenario_id: request.context.scenario_id.clone(),
                    run_id: request.context.run_id.clone(),
                    intent_id: intent_id.to_owned(),
                    protocol: request.protocol.clone(),
                    header_name: evidence.header_name.clone(),
                    header_value: evidence.header_value.clone(),
                },
                now,
            )
            .await?;
        let response = ReportOutboundResponseResponse {
            intent: recorded.intent,
            accepted: recorded.accepted,
            replayed: recorded.replayed,
        };
        operation_metric.succeed();
        Ok(response)
    }

    pub(crate) async fn get_balance(&self, principal: &str, query: &BalanceQuery, now: i64) -> Result<PaymentBalance> {
        let asset = query.asset_identity();
        let _ = self.scenarios.authorize(principal, &query.scenario_id).await?;
        if !self
            .policies
            .has_enabled_wallet_binding(&query.scenario_id, &query.wallet_id)
            .await?
        {
            return Err(Error::Forbidden);
        }
        let wallet = self
            .wallets
            .get_payment_wallet(&query.wallet_id)
            .await?
            .filter(|wallet| wallet.enabled)
            .ok_or_else(|| Error::NotFound("payment wallet".to_owned()))?;
        let entries = self.ledger_entries(query).await?;
        let (confirmed_inbound, confirmed_outbound) = Self::confirmed_totals(&entries)?;
        let reserved_outbound = self.reserved_outbound(query).await?;
        let available = self.balances.load(&wallet.wallet, &asset).await?.asset_available;
        Ok(PaymentBalance {
            scenario_id: query.scenario_id.clone(),
            wallet_id: query.wallet_id.clone(),
            asset,
            confirmed_inbound,
            confirmed_outbound,
            reserved_outbound,
            available: Some(available),
            as_of: now,
        })
    }

    pub(crate) async fn ledger_entries(&self, query: &BalanceQuery) -> Result<Vec<PaymentLedgerEntry>> {
        let asset = query.asset_identity();
        let mut entries = Vec::new();
        let mut offset = 0_u64;
        loop {
            let page = self
                .ledger
                .list(&LedgerQuery {
                    scenario_id: Some(query.scenario_id.clone()),
                    wallet_id: Some(query.wallet_id.clone()),
                    network: Some(asset.network.clone()),
                    asset: Some(asset.asset.clone()),
                    page: PageRequest {
                        offset,
                        limit: PAGE_SIZE,
                    },
                    ..LedgerQuery::default()
                })
                .await?;
            entries.extend(page.items.into_iter().filter(|entry| entry.asset == asset));
            offset = offset.saturating_add(u64::from(PAGE_SIZE));
            if offset >= page.total {
                break;
            }
        }
        Ok(entries)
    }

    pub(crate) async fn reserved_outbound(&self, query: &BalanceQuery) -> Result<Amount> {
        let asset = query.asset_identity();
        let mut total = Amount::ZERO;
        let mut offset = 0_u64;
        loop {
            let page = self
                .payments
                .list(&PaymentQuery {
                    scenario_id: Some(query.scenario_id.clone()),
                    wallet_id: Some(query.wallet_id.clone()),
                    direction: Some(PaymentDirection::Outbound),
                    page: PageRequest {
                        offset,
                        limit: PAGE_SIZE,
                    },
                    ..PaymentQuery::default()
                })
                .await?;
            for intent in page.items {
                if intent.asset == asset && intent.reservation_status == ReservationStatus::Reserved {
                    total = Self::checked_add(total, intent.reserved_amount)?;
                }
            }
            offset = offset.saturating_add(u64::from(PAGE_SIZE));
            if offset >= page.total {
                break;
            }
        }
        Ok(total)
    }

    pub(crate) async fn pending_approvals(
        &self,
        principal: &str,
        scenario_id: &str,
        page: PageRequest,
        now: i64,
    ) -> Result<ApprovalPage> {
        self.approval_authorizer
            .authorize_approver(principal, scenario_id)
            .await?;
        let page = self.approvals.list_pending(scenario_id, now, page).await?;
        Ok(ApprovalPage {
            items: page.items,
            total: usize::try_from(page.total).map_err(|_| Error::Storage("approval count is too large".to_owned()))?,
        })
    }

    pub(crate) async fn get_approval(
        &self,
        principal: &str,
        scenario_id: &str,
        approval_id: &str,
    ) -> Result<ApprovalRequest> {
        self.approval_authorizer
            .authorize_approver(principal, scenario_id)
            .await?;
        let approval = self
            .approvals
            .get(approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        let intent = self
            .payments
            .get_for_scenario(scenario_id, &approval.intent_id)
            .await?
            .ok_or(Error::Forbidden)?;
        if intent.scenario_id != scenario_id {
            return Err(Error::Forbidden);
        }
        Ok(approval)
    }

    pub(crate) async fn decide_approval(
        &self,
        principal: &str,
        request: &SubmitApprovalDecisionRequest,
        now: i64,
    ) -> Result<ApprovalRequest> {
        let mut operation_metric = self.metrics.payment_operation("approve", "outbound");
        request.context.validate()?;
        let approval = self
            .get_approval(principal, &request.context.scenario_id, &request.approval_id)
            .await?;
        let intent = self
            .payments
            .get_for_scenario(&request.context.scenario_id, &approval.intent_id)
            .await?
            .ok_or(Error::Forbidden)?;
        if intent.run_id != request.context.run_id {
            return Err(Error::PaymentConflict(
                "approval decision run_id does not match its payment intent".to_owned(),
            ));
        }
        let decision = self
            .approval_handler
            .submit_decision(
                principal,
                &SubmitApprovalDecision {
                    approval_id: approval.id.clone(),
                    intent_id: approval.intent_id.clone(),
                    decision: request.decision,
                    approved_amount: request.approved_amount,
                    reason: request.reason.clone(),
                },
                now,
            )
            .await?;
        if request.decision == ApprovalDecisionKind::Approve
            && decision.approval.status != crate::model::ApprovalStatus::Consumed
        {
            self.approval_handler
                .consume(principal, &approval.id, &approval.intent_id, now)
                .await?;
        }
        let approval = self
            .approvals
            .get(&approval.id)
            .await?
            .ok_or_else(|| Error::Storage("decided approval disappeared".to_owned()))?;
        operation_metric.succeed();
        Ok(approval)
    }

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

impl PaymentApi {
    pub(crate) fn parse_advertised_expiry(value: Option<&str>) -> Result<Option<i64>> {
        value
            .map(|value| {
                if value.is_empty()
                    || value.len() > 19
                    || (value.len() > 1 && value.starts_with('0'))
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(Error::InvalidRequest(
                        "payment-expires-at must be canonical Unix seconds".to_owned(),
                    ));
                }
                value
                    .parse::<i64>()
                    .map_err(|_| Error::InvalidRequest("payment-expires-at must be canonical Unix seconds".to_owned()))
            })
            .transpose()
    }
}

impl PaymentApi {
    pub(crate) fn authorization_intent_id(result: &PaymentAuthorizationResult) -> &str {
        match result {
            PaymentAuthorizationResult::Denied { intent_id, .. }
            | PaymentAuthorizationResult::RequiresApproval { intent_id, .. }
            | PaymentAuthorizationResult::Authorized { intent_id, .. } => intent_id,
        }
    }
}

impl PaymentApi {
    pub(crate) fn canonical_origin(resource: &str) -> Result<String> {
        let url = Url::parse(resource)
            .map_err(|_| Error::InvalidRequest("payment resource is not an absolute URL".to_owned()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::InvalidRequest("payment resource URL is unsafe".to_owned()));
        }
        Ok(url.origin().ascii_serialization())
    }
}

impl PaymentApi {
    pub(crate) fn confirmed_totals(entries: &[PaymentLedgerEntry]) -> Result<(Amount, Amount)> {
        let by_id = entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let mut inbound = Amount::ZERO;
        let mut outbound = Amount::ZERO;
        for entry in entries
            .iter()
            .filter(|entry| entry.entry_type != LedgerEntryType::Reversal)
        {
            match entry.direction {
                PaymentDirection::Inbound => inbound = Self::checked_add(inbound, entry.amount)?,
                PaymentDirection::Outbound => outbound = Self::checked_add(outbound, entry.amount)?,
            }
        }
        for entry in entries
            .iter()
            .filter(|entry| entry.entry_type == LedgerEntryType::Reversal)
        {
            let original = entry
                .reversal_of
                .as_deref()
                .and_then(|id| by_id.get(id).copied())
                .ok_or_else(|| Error::Storage("ledger reversal original is outside its balance scope".to_owned()))?;
            match original.direction {
                PaymentDirection::Inbound => {
                    inbound = inbound
                        .checked_sub(entry.amount)
                        .ok_or_else(|| Error::Storage("inbound ledger total underflow".to_owned()))?;
                }
                PaymentDirection::Outbound => {
                    outbound = outbound
                        .checked_sub(entry.amount)
                        .ok_or_else(|| Error::Storage("outbound ledger total underflow".to_owned()))?;
                }
            }
        }
        Ok((inbound, outbound))
    }
}

impl PaymentApi {
    pub(crate) fn checked_add(left: Amount, right: Amount) -> Result<Amount> {
        left.checked_add(right)
            .ok_or_else(|| Error::Storage("payment amount aggregate overflow".to_owned()))
    }
}

impl PaymentApi {
    pub(crate) fn unix_time() -> Result<i64> {
        let seconds = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| Error::InvalidTransition("system clock is before the Unix epoch".to_owned()))?
            .as_secs();
        i64::try_from(seconds).map_err(|_| Error::InvalidTransition("system clock is out of range".to_owned()))
    }
}

#[cfg(test)]
#[path = "../../tests/payment/api_test.rs"]
mod tests;
