//! Authenticated payment API shared by HTTP and message transports.
//!
//! Wire DTOs live here because transport delivery fields (for example the
//! HTTP credential header) are deliberately not part of the payment domain.

use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use x402_types::util::b64::Base64Bytes;

use crate::{
    apm::AgentVaultMetrics,
    handler::{
        ApprovalHandler, ApprovalPrincipalAuthorizer, AuthorizePayment, IssuePaymentChallenge, OfferHandler,
        OutboundEvidenceHandler, OutboundSigningHandler, ProtocolRegistry, RecordOutboundPaymentResponse,
        ScenarioHandler, SettleIncomingPayment, SettlementHandler, SignAuthorizedPayment, SubmitApprovalDecision,
        VerifyIncomingPayment, WalletBalanceSource,
    },
    model::{
        Amount, ApprovalDecisionKind, ApprovalRequest, AssetIdentity, LedgerEntryType, PaymentAuthorizationResult,
        PaymentChallenge, PaymentCredential, PaymentDirection, PaymentEventType, PaymentIntent, PaymentLedgerEntry,
        PaymentOffer, PaymentReceipt, ReservationStatus,
    },
    storage::{
        ApprovalStorage, ClientStorage, EventStorage, LedgerQuery, LedgerStorage, OfferStorage, Page, PageRequest,
        PaymentQuery, PaymentStorage, PolicyStorage, ScenarioStorage, WalletStorage,
    },
    Error, Result,
};

use super::{ApiError, CallerPrincipal};

const PAGE_SIZE: u32 = 1_000;

/// Dependencies for the transport-neutral payment application façade.
pub struct PaymentApiDependencies {
    pub clients: Arc<dyn ClientStorage>,
    pub scenario_store: Arc<dyn ScenarioStorage>,
    pub scenarios: Arc<ScenarioHandler>,
    pub payments_handler: Arc<crate::handler::PaymentHandler>,
    pub signing_handler: Arc<OutboundSigningHandler>,
    pub outbound_evidence_handler: Arc<OutboundEvidenceHandler>,
    pub settlement_handler: Arc<SettlementHandler>,
    pub offer_handler: Arc<OfferHandler>,
    pub approval_handler: Arc<ApprovalHandler>,
    pub approval_authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
    pub payer_protocols: Arc<ProtocolRegistry>,
    pub payments: Arc<dyn PaymentStorage>,
    pub approvals: Arc<dyn ApprovalStorage>,
    pub offers: Arc<dyn OfferStorage>,
    pub policies: Arc<dyn PolicyStorage>,
    pub wallets: Arc<dyn WalletStorage>,
    pub events: Arc<dyn EventStorage>,
    pub ledger: Arc<dyn LedgerStorage>,
    pub balances: Arc<dyn WalletBalanceSource>,
    pub metrics: AgentVaultMetrics,
}

/// One authenticated application service used by HTTP, Unix socket, and MQTT
/// adapters. Tenant identity is always supplied by trusted transport context.
pub struct PaymentApi {
    clients: Arc<dyn ClientStorage>,
    scenario_store: Arc<dyn ScenarioStorage>,
    scenarios: Arc<ScenarioHandler>,
    payments_handler: Arc<crate::handler::PaymentHandler>,
    signing_handler: Arc<OutboundSigningHandler>,
    outbound_evidence_handler: Arc<OutboundEvidenceHandler>,
    settlement_handler: Arc<SettlementHandler>,
    offer_handler: Arc<OfferHandler>,
    approval_handler: Arc<ApprovalHandler>,
    approval_authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
    payer_protocols: Arc<ProtocolRegistry>,
    payments: Arc<dyn PaymentStorage>,
    approvals: Arc<dyn ApprovalStorage>,
    offers: Arc<dyn OfferStorage>,
    policies: Arc<dyn PolicyStorage>,
    wallets: Arc<dyn WalletStorage>,
    events: Arc<dyn EventStorage>,
    ledger: Arc<dyn LedgerStorage>,
    balances: Arc<dyn WalletBalanceSource>,
    metrics: AgentVaultMetrics,
}

impl PaymentApi {
    #[must_use]
    pub fn new(dependencies: PaymentApiDependencies) -> Self {
        Self {
            clients: dependencies.clients,
            scenario_store: dependencies.scenario_store,
            scenarios: dependencies.scenarios,
            payments_handler: dependencies.payments_handler,
            signing_handler: dependencies.signing_handler,
            outbound_evidence_handler: dependencies.outbound_evidence_handler,
            settlement_handler: dependencies.settlement_handler,
            offer_handler: dependencies.offer_handler,
            approval_handler: dependencies.approval_handler,
            approval_authorizer: dependencies.approval_authorizer,
            payer_protocols: dependencies.payer_protocols,
            payments: dependencies.payments,
            approvals: dependencies.approvals,
            offers: dependencies.offers,
            policies: dependencies.policies,
            wallets: dependencies.wallets,
            events: dependencies.events,
            ledger: dependencies.ledger,
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
        let intent_id = authorization_intent_id(&authorization);
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
                credential = Some(PaymentCredentialDto::for_http(
                    signed.credential,
                    &request.challenge.origin,
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

    async fn parse_outbound_challenge(&self, request: &AuthorizePaymentRequest, now: i64) -> Result<PaymentChallenge> {
        request.context.validate()?;
        let origin = canonical_origin(&request.challenge.resource)?;
        if request.challenge.origin != origin {
            return Err(Error::InvalidRequest(
                "opaque 402 metadata does not match the requested resource".to_owned(),
            ));
        }
        let protocol = payment_protocol(&request.challenge)?;
        let wire = match protocol {
            "x402" => serde_json::from_str::<Value>(&request.challenge.body)
                .map_err(|_| Error::InvalidRequest("x402 402 body is not valid JSON".to_owned()))?,
            "mpp" => {
                let authenticate = header_value(&request.challenge.headers, "www-authenticate").ok_or_else(|| {
                    Error::InvalidRequest("MPP 402 response has no WWW-Authenticate header".to_owned())
                })?;
                json!({"www_authenticate": authenticate})
            }
            unsupported => {
                return Err(Error::InvalidRequest(format!(
                    "unsupported outbound payment protocol: {unsupported}"
                )))
            }
        };
        let challenge = self
            .payer_protocols
            .get(protocol)?
            .parse_challenge(
                &request.context.scenario_id,
                &request.challenge.resource,
                &wire,
                now,
                parse_advertised_expiry(request.challenge.expires_at.as_deref())?,
            )
            .await?;
        if challenge.protocol != protocol || challenge.resource != request.challenge.resource {
            return Err(Error::PaymentConflict(
                "parsed protocol challenge differs from the captured 402 metadata".to_owned(),
            ));
        }
        Ok(challenge)
    }

    async fn get_payment(&self, principal: &str, scenario_id: &str, intent_id: &str) -> Result<PaymentIntent> {
        let _ = self.scenarios.authorize(principal, scenario_id).await?;
        self.payments
            .get_for_scenario(scenario_id, intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))
    }

    async fn get_receipt(&self, principal: &str, scenario_id: &str, intent_id: &str) -> Result<PaymentReceipt> {
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
        let recorded = self
            .outbound_evidence_handler
            .record_merchant_response(
                principal,
                &RecordOutboundPaymentResponse {
                    scenario_id: request.context.scenario_id.clone(),
                    run_id: request.context.run_id.clone(),
                    intent_id: intent_id.to_owned(),
                    payment_response: request.payment_response.clone(),
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

    async fn get_balance(&self, principal: &str, query: &BalanceQuery, now: i64) -> Result<PaymentBalance> {
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
        let (confirmed_inbound, confirmed_outbound) = confirmed_totals(&entries)?;
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

    async fn ledger_entries(&self, query: &BalanceQuery) -> Result<Vec<PaymentLedgerEntry>> {
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

    async fn reserved_outbound(&self, query: &BalanceQuery) -> Result<Amount> {
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
                    total = checked_add(total, intent.reserved_amount)?;
                }
            }
            offset = offset.saturating_add(u64::from(PAGE_SIZE));
            if offset >= page.total {
                break;
            }
        }
        Ok(total)
    }

    async fn pending_approvals(
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

    async fn get_approval(&self, principal: &str, scenario_id: &str, approval_id: &str) -> Result<ApprovalRequest> {
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

    pub(crate) async fn resolve_offer(&self, principal: &str, request: &ResolveOfferRequest) -> Result<PaymentOffer> {
        let protocol = if let Some(protocol) = request.protocol.as_deref() {
            protocol.to_owned()
        } else {
            let _ = self.scenarios.authorize(principal, &request.scenario_id).await?;
            let offer = self
                .offers
                .resolve_resource(&request.scenario_id, &request.resource)
                .await?
                .ok_or_else(|| Error::NotFound("payment offer".to_owned()))?;
            offer
                .protocols
                .first()
                .cloned()
                .ok_or_else(|| Error::Storage("payment offer has no protocol".to_owned()))?
        };
        self.offer_handler
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
                    offer_id: request.offer_id.clone(),
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

fn parse_advertised_expiry(value: Option<&str>) -> Result<Option<i64>> {
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

/// Builds the canonical v1 payment routes. Authentication middleware injects
/// [`CallerPrincipal`] before any endpoint runs.
pub fn payment_router(api: Arc<PaymentApi>) -> Router {
    Router::new()
        .route("/admin/clients", get(list_clients))
        .route("/admin/scenarios", get(list_scenarios))
        .route("/admin/policies", get(list_policies))
        .route("/admin/wallets", get(list_wallets))
        .route("/admin/offers", get(list_offers))
        .route("/payments/authorize", post(authorize_payment))
        .route("/payments/balance", get(get_balance))
        .route("/payments/{intent_id}", get(get_payment))
        .route("/payments/{intent_id}/receipt", get(get_receipt))
        .route(
            "/payments/{intent_id}/outbound-response",
            post(report_outbound_response),
        )
        .route("/approvals", get(list_approvals))
        .route("/approvals/{approval_id}", get(get_approval))
        .route("/approvals/{approval_id}/decision", post(decide_approval))
        .route("/offers/resolve", post(resolve_offer))
        .route("/payee/challenges", post(create_challenge))
        .route("/payee/verify", post(verify_payment))
        .route("/payee/settle", post(settle_payment))
        .with_state(api)
}

async fn list_clients(
    State(api): State<Arc<PaymentApi>>,
    _principal: CallerPrincipal,
    Query(query): Query<AdminPageQuery>,
) -> std::result::Result<Json<Page<crate::model::PaymentClient>>, ApiError> {
    Ok(Json(api.clients.list(query.page()?).await?))
}

async fn list_scenarios(
    State(api): State<Arc<PaymentApi>>,
    _principal: CallerPrincipal,
    Query(query): Query<ClientPageQuery>,
) -> std::result::Result<Json<Page<crate::model::PaymentScenario>>, ApiError> {
    Ok(Json(
        api.scenario_store
            .list_for_client(&query.client_id, query.page.page()?)
            .await?,
    ))
}

async fn list_policies(
    State(api): State<Arc<PaymentApi>>,
    _principal: CallerPrincipal,
    Query(query): Query<ScenarioPageQuery>,
) -> std::result::Result<Json<Page<crate::model::PaymentPolicy>>, ApiError> {
    Ok(Json(
        api.policies
            .list_for_scenario(&query.scenario_id, query.page.page()?)
            .await?,
    ))
}

async fn list_wallets(
    State(api): State<Arc<PaymentApi>>,
    _principal: CallerPrincipal,
    Query(query): Query<AdminPageQuery>,
) -> std::result::Result<Json<Page<crate::model::wallet::Wallet>>, ApiError> {
    Ok(Json(api.wallets.list(query.page()?).await?))
}

async fn list_offers(
    State(api): State<Arc<PaymentApi>>,
    _principal: CallerPrincipal,
    Query(query): Query<ScenarioPageQuery>,
) -> std::result::Result<Json<Page<PaymentOffer>>, ApiError> {
    Ok(Json(
        api.offers
            .list_for_scenario(&query.scenario_id, query.page.page()?)
            .await?,
    ))
}

async fn authorize_payment(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Json(request): Json<AuthorizePaymentRequest>,
) -> std::result::Result<Json<AuthorizePaymentResponse>, ApiError> {
    Ok(Json(api.authorize(principal.as_str(), &request, unix_time()?).await?))
}

async fn get_payment(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Path(intent_id): Path<String>,
    Query(query): Query<ScenarioQuery>,
) -> std::result::Result<Json<PaymentIntent>, ApiError> {
    Ok(Json(
        api.get_payment(principal.as_str(), &query.scenario_id, &intent_id)
            .await?,
    ))
}

async fn get_receipt(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Path(intent_id): Path<String>,
    Query(query): Query<ScenarioQuery>,
) -> std::result::Result<Json<PaymentReceipt>, ApiError> {
    Ok(Json(
        api.get_receipt(principal.as_str(), &query.scenario_id, &intent_id)
            .await?,
    ))
}

async fn report_outbound_response(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Path(intent_id): Path<String>,
    Json(request): Json<ReportOutboundResponseRequest>,
) -> std::result::Result<Json<ReportOutboundResponseResponse>, ApiError> {
    Ok(Json(
        api.report_outbound_response(principal.as_str(), &intent_id, &request, unix_time()?)
            .await?,
    ))
}

async fn get_balance(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Query(query): Query<BalanceQuery>,
) -> std::result::Result<Json<PaymentBalance>, ApiError> {
    Ok(Json(api.get_balance(principal.as_str(), &query, unix_time()?).await?))
}

async fn list_approvals(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Query(query): Query<ApprovalQuery>,
) -> std::result::Result<Json<ApprovalPage>, ApiError> {
    if query.status.as_deref().is_some_and(|status| status != "pending") {
        return Err(Error::InvalidRequest("only pending approval queries are supported".to_owned()).into());
    }
    let page = approval_page_request(&query)?;
    Ok(Json(
        api.pending_approvals(principal.as_str(), &query.scenario_id, page, unix_time()?)
            .await?,
    ))
}

async fn get_approval(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Path(approval_id): Path<String>,
    Query(query): Query<ScenarioQuery>,
) -> std::result::Result<Json<ApprovalRequest>, ApiError> {
    Ok(Json(
        api.get_approval(principal.as_str(), &query.scenario_id, &approval_id)
            .await?,
    ))
}

async fn decide_approval(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Path(approval_id): Path<String>,
    Json(request): Json<SubmitApprovalDecisionRequest>,
) -> std::result::Result<Json<ApprovalRequest>, ApiError> {
    if approval_id != request.approval_id {
        return Err(Error::PaymentConflict("approval path and body differ".to_owned()).into());
    }
    Ok(Json(
        api.decide_approval(principal.as_str(), &request, unix_time()?).await?,
    ))
}

async fn resolve_offer(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Json(request): Json<ResolveOfferRequest>,
) -> std::result::Result<Json<PaymentOffer>, ApiError> {
    Ok(Json(api.resolve_offer(principal.as_str(), &request).await?))
}

async fn create_challenge(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Json(request): Json<CreateChallengeRequest>,
) -> std::result::Result<Json<PaymentChallenge>, ApiError> {
    Ok(Json(
        api.create_challenge(principal.as_str(), &request, unix_time()?).await?,
    ))
}

async fn verify_payment(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Json(request): Json<VerifyPaymentRequest>,
) -> std::result::Result<Json<PaymentVerification>, ApiError> {
    Ok(Json(
        api.verify_payment(principal.as_str(), &request, unix_time()?).await?,
    ))
}

async fn settle_payment(
    State(api): State<Arc<PaymentApi>>,
    principal: CallerPrincipal,
    Json(request): Json<SettlePaymentRequest>,
) -> std::result::Result<Json<SettledPaymentResponse>, ApiError> {
    Ok(Json(
        api.settle_payment(principal.as_str(), &request, unix_time()?).await?,
    ))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationContextDto {
    pub scenario_id: String,
    pub run_id: String,
}

impl OperationContextDto {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("scenario_id", self.scenario_id.as_str()),
            ("run_id", self.run_id.as_str()),
        ] {
            validate_identifier(name, value)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaymentRequiredDto {
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
pub(crate) struct AuthorizePaymentRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    pub challenge: PaymentRequiredDto,
}

#[derive(Clone, Serialize)]
pub(crate) struct AuthorizePaymentResponse {
    pub authorization: PaymentAuthorizationResult,
    pub intent: PaymentIntent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<PaymentCredentialDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportOutboundResponseRequest {
    #[serde(flatten)]
    pub context: OperationContextDto,
    /// Official x402 v2 `PAYMENT-RESPONSE` header value (standard base64).
    pub payment_response: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReportOutboundResponseResponse {
    pub intent: PaymentIntent,
    pub accepted: bool,
    pub replayed: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaymentCredentialDto {
    pub protocol: String,
    pub challenge_hash: String,
    pub credential_hash: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpCredentialDto>,
}

impl PaymentCredentialDto {
    fn into_domain(self) -> PaymentCredential {
        PaymentCredential {
            protocol: self.protocol,
            challenge_hash: self.challenge_hash,
            credential_hash: self.credential_hash,
            payload: self.payload,
        }
    }

    fn for_http(credential: PaymentCredential, origin: &str) -> Result<Self> {
        let (header_name, header_value) = match credential.protocol.as_str() {
            "x402" => (
                "payment-signature".to_owned(),
                Base64Bytes::encode(serde_json::to_vec(&credential.payload)?).to_string(),
            ),
            "mpp" => {
                let value = credential
                    .payload
                    .get("authorization")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::PaymentProtocol("MPP credential has no Authorization value".to_owned()))?;
                ("authorization".to_owned(), value.to_owned())
            }
            protocol => {
                return Err(Error::PaymentProtocol(format!(
                    "unsupported HTTP credential protocol: {protocol}"
                )))
            }
        };
        Ok(Self {
            protocol: credential.protocol,
            challenge_hash: credential.challenge_hash,
            credential_hash: credential.credential_hash,
            payload: credential.payload,
            http: Some(HttpCredentialDto {
                bound_origin: origin.to_owned(),
                header_name,
                header_value,
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
pub(crate) struct HttpCredentialDto {
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
struct ScenarioQuery {
    scenario_id: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct AdminPageQuery {
    offset: Option<u64>,
    limit: Option<u32>,
}

impl AdminPageQuery {
    fn page(self) -> Result<PageRequest> {
        let page = PageRequest {
            offset: self.offset.unwrap_or(0),
            limit: self.limit.unwrap_or(100),
        };
        if page.limit == 0 || page.limit > PAGE_SIZE {
            return Err(Error::InvalidRequest(format!(
                "page limit must be between 1 and {PAGE_SIZE}"
            )));
        }
        Ok(page)
    }
}

#[derive(Debug, Deserialize)]
struct ClientPageQuery {
    client_id: String,
    #[serde(flatten)]
    page: AdminPageQuery,
}

#[derive(Debug, Deserialize)]
struct ScenarioPageQuery {
    scenario_id: String,
    #[serde(flatten)]
    page: AdminPageQuery,
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
    fn asset_identity(&self) -> AssetIdentity {
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
struct ApprovalQuery {
    scenario_id: String,
    status: Option<String>,
    offset: Option<u64>,
    limit: Option<u32>,
}

fn approval_page_request(query: &ApprovalQuery) -> Result<PageRequest> {
    let limit = query.limit.unwrap_or(PAGE_SIZE);
    if limit == 0 || limit > PAGE_SIZE {
        return Err(Error::InvalidRequest(format!(
            "approval page limit must be between 1 and {PAGE_SIZE}"
        )));
    }
    Ok(PageRequest {
        offset: query.offset.unwrap_or_default(),
        limit,
    })
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
pub struct ResolveOfferRequest {
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
    pub offer_id: String,
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

fn authorization_intent_id(result: &PaymentAuthorizationResult) -> &str {
    match result {
        PaymentAuthorizationResult::Denied { intent_id, .. }
        | PaymentAuthorizationResult::RequiresApproval { intent_id, .. }
        | PaymentAuthorizationResult::Authorized { intent_id, .. } => intent_id,
    }
}

fn validate_identifier(name: &str, value: &str) -> Result<()> {
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

fn canonical_origin(resource: &str) -> Result<String> {
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

fn header_value<'a>(headers: &'a BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.as_str())
}

fn payment_protocol(challenge: &PaymentRequiredDto) -> Result<&str> {
    if let Some(hint) = challenge.protocol_hint.as_deref() {
        if hint != hint.trim() || !matches!(hint, "x402" | "mpp") {
            return Err(Error::PaymentProtocol(format!(
                "unsupported payment protocol hint: {hint}"
            )));
        }
        return Ok(hint);
    }
    if header_value(&challenge.headers, "www-authenticate").is_some() {
        Ok("mpp")
    } else {
        Ok("x402")
    }
}

fn confirmed_totals(entries: &[PaymentLedgerEntry]) -> Result<(Amount, Amount)> {
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
            PaymentDirection::Inbound => inbound = checked_add(inbound, entry.amount)?,
            PaymentDirection::Outbound => outbound = checked_add(outbound, entry.amount)?,
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

fn checked_add(left: Amount, right: Amount) -> Result<Amount> {
    left.checked_add(right)
        .ok_or_else(|| Error::Storage("payment amount aggregate overflow".to_owned()))
}

fn unix_time() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| Error::InvalidTransition("system clock is before the Unix epoch".to_owned()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::InvalidTransition("system clock is out of range".to_owned()))
}

#[cfg(test)]
#[path = "../../../tests/controller/http/payment_test.rs"]
mod tests;
