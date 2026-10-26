//! Axum extraction and routing for the shared payment API.
use crate::controller::principal_context::CallerPrincipal;
use crate::payment::PaymentApi;
use crate::{
    model::payment_api::{
        AdminPageQuery, ApprovalPage, ApprovalQuery, AuthorizePaymentRequest, AuthorizePaymentResponse, BalanceQuery,
        ClientPageQuery, CreateChallengeRequest, CreateScenarioRequest, LedgerListQuery, PaymentBalance,
        PaymentListQuery, PaymentVerification, ReportOutboundResponseRequest, ReportOutboundResponseResponse,
        ResolvePriceRequest, ScenarioPageQuery, ScenarioQuery, SettlePaymentRequest, SettledPaymentResponse,
        SubmitApprovalDecisionRequest, VerifyPaymentRequest,
    },
    model::price::{CreatePriceRequest, DeletePriceRequest, UpdatePriceRequest},
    model::{ApprovalRequest, PaymentChallenge, PaymentIntent, PaymentLedgerEntry, PaymentPrice, PaymentReceipt},
    storage::{IdempotentWrite, Page, ScenarioConfiguration},
    Error,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, MethodRouter},
    Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
pub struct ApiController;
impl ApiController {
    /// Builds the canonical v1 payment routes. Authentication middleware injects
    /// [`CallerPrincipal`] before any endpoint runs.
    pub fn router(api: Arc<PaymentApi>) -> Router {
        let routes = Router::new()
            .route("/admin/clients", get(Self::list_clients))
            .route(
                "/admin/scenarios",
                get(Self::list_scenarios).post(Self::create_scenario),
            )
            .route("/admin/policies", get(Self::list_policies))
            .route("/admin/wallets", get(Self::list_wallets).post(Self::create_wallet))
            .route(
                "/admin/wallets/encryption-sessions",
                post(Self::create_wallet_import_session),
            )
            .route("/admin/payments", get(Self::list_payments))
            .route("/admin/ledger", get(Self::list_ledger))
            .route("/payments/authorize", post(Self::authorize_payment))
            .route("/payments/balance", get(Self::get_balance))
            .route("/payments/{intent_id}", get(Self::get_payment))
            .route("/payments/{intent_id}/receipt", get(Self::get_receipt))
            .route(
                "/payments/{intent_id}/outbound-response",
                post(Self::report_outbound_response),
            )
            .route("/approvals", get(Self::list_approvals))
            .route("/approvals/{approval_id}", get(Self::get_approval))
            .route("/approvals/{approval_id}/decision", post(Self::decide_approval))
            .route("/payee/challenges", post(Self::create_challenge))
            .route("/payee/verify", post(Self::verify_payment))
            .route("/payee/settle", post(Self::settle_payment));
        Self::price_routes(routes, post(Self::resolve_offer), get(Self::list_offers))
            .route("/admin/prices", post(Self::create_price))
            .route(
                "/admin/prices/{id}",
                get(Self::get_price).put(Self::update_price).delete(Self::delete_price),
            )
            .with_state(api)
    }

    fn price_routes<S>(router: Router<S>, resolve: MethodRouter<S>, list: MethodRouter<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        router
            .route("/prices/resolve", resolve.clone())
            .route("/offers/resolve", resolve)
            .route("/admin/prices", list.clone())
            .route("/admin/offers", list)
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
            api.scenarios
                .list_for_client(&query.client_id, query.page.page()?)
                .await?,
        ))
    }

    async fn create_scenario(
        State(api): State<Arc<PaymentApi>>,
        _principal: CallerPrincipal,
        Json(request): Json<CreateScenarioRequest>,
    ) -> std::result::Result<Json<ScenarioConfiguration>, ApiError> {
        let now = PaymentApi::unix_time()?;
        let (scenario, policy) = request.into_domain(now);
        if api.clients.get(&scenario.client_id).await?.is_none() {
            return Err(Error::NotFound(format!("payment client {}", scenario.client_id)).into());
        }
        let configuration = match api.scenarios.register_with_policy(&scenario, &policy).await? {
            IdempotentWrite::Inserted(configuration) | IdempotentWrite::Replay(configuration) => configuration,
        };
        Ok(Json(configuration))
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

    async fn create_wallet_import_session(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
    ) -> std::result::Result<axum::response::Response, ApiError> {
        let session = api
            .wallet_handler
            .create_import_session(principal.as_str(), PaymentApi::unix_time()?)
            .await?;
        Ok(([(header::CACHE_CONTROL, "no-store")], Json(session)).into_response())
    }

    async fn create_wallet(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<crate::model::CreateEncryptedWallet>,
    ) -> std::result::Result<Json<crate::model::wallet::Wallet>, ApiError> {
        Ok(Json(
            api.wallet_handler
                .create_wallet(principal.as_str(), request, PaymentApi::unix_time()?)
                .await?,
        ))
    }

    async fn list_offers(
        State(api): State<Arc<PaymentApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<ScenarioPageQuery>,
    ) -> std::result::Result<Json<Page<PaymentPrice>>, ApiError> {
        Ok(Json(
            api.offers
                .list_for_scenario(&query.scenario_id, query.page.page()?)
                .await?,
        ))
    }

    async fn create_price(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<CreatePriceRequest>,
    ) -> std::result::Result<(StatusCode, Json<PaymentPrice>), ApiError> {
        api.scenarios
            .authorize(principal.as_str(), &request.0.scenario_id)
            .await?;
        let price = request.0.into_domain(PaymentApi::unix_time()?);
        Ok((StatusCode::CREATED, Json(api.offers.register(&price).await?)))
    }

    async fn authorized_price(
        api: &PaymentApi,
        principal: &str,
        id: &str,
    ) -> std::result::Result<PaymentPrice, ApiError> {
        let price = api
            .offers
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment price {id}")))?;
        api.scenarios.authorize(principal, &price.scenario_id).await?;
        Ok(price)
    }

    async fn get_price(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Path(id): Path<String>,
    ) -> std::result::Result<Json<PaymentPrice>, ApiError> {
        Ok(Json(Self::authorized_price(&api, principal.as_str(), &id).await?))
    }

    async fn update_price(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Path(id): Path<String>,
        Json(request): Json<UpdatePriceRequest>,
    ) -> std::result::Result<Json<PaymentPrice>, ApiError> {
        let stored = Self::authorized_price(&api, principal.as_str(), &id).await?;
        let (price, expected) = request.into_domain(&id, &stored, PaymentApi::unix_time()?)?;
        Ok(Json(api.offers.update(&price, expected).await?))
    }

    async fn delete_price(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Path(id): Path<String>,
        Query(request): Query<DeletePriceRequest>,
    ) -> std::result::Result<StatusCode, ApiError> {
        Self::authorized_price(&api, principal.as_str(), &id).await?;
        api.offers.delete(&id, request.expected_updated_at).await?;
        Ok(StatusCode::NO_CONTENT)
    }

    async fn list_payments(
        State(api): State<Arc<PaymentApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<PaymentListQuery>,
    ) -> std::result::Result<Json<Page<PaymentIntent>>, ApiError> {
        Ok(Json(api.payments.list(&query.into_domain()?).await?))
    }

    async fn list_ledger(
        State(api): State<Arc<PaymentApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<LedgerListQuery>,
    ) -> std::result::Result<Json<Page<PaymentLedgerEntry>>, ApiError> {
        Ok(Json(api.ledger.list(&query.into_domain()?).await?))
    }

    async fn authorize_payment(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<AuthorizePaymentRequest>,
    ) -> std::result::Result<Json<AuthorizePaymentResponse>, ApiError> {
        Ok(Json(
            api.authorize(principal.as_str(), &request, PaymentApi::unix_time()?)
                .await?,
        ))
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
            api.report_outbound_response(principal.as_str(), &intent_id, &request, PaymentApi::unix_time()?)
                .await?,
        ))
    }

    async fn get_balance(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Query(query): Query<BalanceQuery>,
    ) -> std::result::Result<Json<PaymentBalance>, ApiError> {
        Ok(Json(
            api.get_balance(principal.as_str(), &query, PaymentApi::unix_time()?)
                .await?,
        ))
    }

    async fn list_approvals(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Query(query): Query<ApprovalQuery>,
    ) -> std::result::Result<Json<ApprovalPage>, ApiError> {
        if query.status.as_deref().is_some_and(|status| status != "pending") {
            return Err(Error::InvalidRequest("only pending approval queries are supported".to_owned()).into());
        }
        let page = query.page()?;
        Ok(Json(
            api.pending_approvals(principal.as_str(), &query.scenario_id, page, PaymentApi::unix_time()?)
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
            api.decide_approval(principal.as_str(), &request, PaymentApi::unix_time()?)
                .await?,
        ))
    }

    async fn resolve_offer(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<ResolvePriceRequest>,
    ) -> std::result::Result<Json<PaymentPrice>, ApiError> {
        Ok(Json(api.resolve_offer(principal.as_str(), &request).await?))
    }

    async fn create_challenge(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<CreateChallengeRequest>,
    ) -> std::result::Result<Json<PaymentChallenge>, ApiError> {
        Ok(Json(
            api.create_challenge(principal.as_str(), &request, PaymentApi::unix_time()?)
                .await?,
        ))
    }

    async fn verify_payment(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<VerifyPaymentRequest>,
    ) -> std::result::Result<Json<PaymentVerification>, ApiError> {
        Ok(Json(
            api.verify_payment(principal.as_str(), &request, PaymentApi::unix_time()?)
                .await?,
        ))
    }

    async fn settle_payment(
        State(api): State<Arc<PaymentApi>>,
        principal: CallerPrincipal,
        Json(request): Json<SettlePaymentRequest>,
    ) -> std::result::Result<Json<SettledPaymentResponse>, ApiError> {
        Ok(Json(
            api.settle_payment(principal.as_str(), &request, PaymentApi::unix_time()?)
                .await?,
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/controller/api_test.rs"]
mod tests;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl ApiError {
    #[must_use]
    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "UNAUTHORIZED",
            message: "a valid AuthGuard access context is required".to_owned(),
        }
    }

    #[must_use]
    pub fn dependency_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "DEPENDENCY_UNAVAILABLE",
            message: "a required identity dependency is unavailable".to_owned(),
        }
    }

    #[must_use]
    pub fn not_found(resource: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "NOT_FOUND",
            message: format!("{resource} was not found"),
        }
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        match error {
            Error::InvalidRequest(_) | Error::PaymentProtocol(_) => Self {
                status: StatusCode::BAD_REQUEST,
                code: "INVALID_REQUEST",
                message: error.to_string(),
            },
            Error::Unauthenticated => Self::unauthorized(),
            Error::Forbidden | Error::PolicyDenied(_) => Self {
                status: StatusCode::FORBIDDEN,
                code: "FORBIDDEN",
                message: error.to_string(),
            },
            Error::NotFound(_) => Self {
                status: StatusCode::NOT_FOUND,
                code: "NOT_FOUND",
                message: error.to_string(),
            },
            Error::PaymentConflict(_) | Error::StorageConflict(_) | Error::InvalidTransition(_) => Self {
                status: StatusCode::CONFLICT,
                code: "PAYMENT_CONFLICT",
                message: error.to_string(),
            },
            Error::DependencyUnavailable(_) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "DEPENDENCY_UNAVAILABLE",
                message: "a required service is temporarily unavailable".to_owned(),
            },
            _ => {
                tracing::error!(%error, "API operation failed");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "INTERNAL_ERROR",
                    message: "the operation could not be completed".to_owned(),
                }
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

impl ApiController {
    pub(crate) fn management_router(
        config: &crate::config::ManagementConfig,
        metrics: crate::apm::AgentVaultMetrics,
        readiness_probes: Vec<Arc<dyn crate::apm::ReadinessProbe>>,
    ) -> Router {
        use crate::apm::{ManagementEndpoints, ManagementState};
        let state = ManagementState {
            metrics,
            readiness_probes,
            probe_timeout: config.health.probe_timeout,
        };
        let mut routes = Router::new()
            .route(
                &config.health.liveness_path,
                get(|| async { ManagementEndpoints::liveness() }),
            )
            .route(&config.health.readiness_path, get(ManagementEndpoints::readiness))
            .route(
                "/debug/runtime",
                get(|| async { ManagementEndpoints::runtime_profile() }),
            );
        if config.metrics.enabled {
            routes = routes.route(
                &config.metrics.path,
                get(|state: State<ManagementState>| async move { ManagementEndpoints::render_metrics(state) }),
            );
        }
        let routes = routes.with_state(state);
        if config.context_path == "/" {
            routes
        } else {
            Router::new().nest(&config.context_path, routes)
        }
    }
}
