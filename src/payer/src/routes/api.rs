//! Axum extraction and routing for the shared payment API.
use crate::{
    handler::api::PayerApi, model::payment_api::AdminPageQuery, model::payment_api::ApprovalPage,
    model::payment_api::ApprovalQuery, model::payment_api::AuthorizePaymentRequest,
    model::payment_api::AuthorizePaymentResponse, model::payment_api::BalanceQuery,
    model::payment_api::ClientPageQuery, model::payment_api::CreateScenarioRequest,
    model::payment_api::LedgerListQuery, model::payment_api::PaymentBalance, model::payment_api::PaymentListQuery,
    model::payment_api::ReportOutboundResponseRequest, model::payment_api::ReportOutboundResponseResponse,
    model::payment_api::ScenarioPageQuery, model::payment_api::ScenarioQuery,
    model::payment_api::SubmitApprovalDecisionRequest, model::price::CreatePriceRequest,
    model::price::DeletePriceRequest, model::price::UpdatePriceRequest, model::ApprovalRequest, model::PaymentIntent,
    model::PaymentLedgerEntry, model::PaymentPrice, model::PaymentReceipt, storage::IdempotentWrite, storage::Page,
    storage::ScenarioConfiguration, Error,
};
use agentvault_common::util::principal_context::CallerPrincipal;
use axum::{
    extract::Path, extract::Query, extract::State, http::header, http::StatusCode, response::IntoResponse,
    routing::get, routing::post, Json, Router,
};
use std::sync::Arc;
pub struct PayerRoutes;
impl PayerRoutes {
    /// Builds the canonical v1 payment routes. Authentication middleware injects
    /// [`CallerPrincipal`] before any endpoint runs.
    pub fn router(api: Arc<PayerApi>) -> Router {
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
            .route("/approvals/{approval_id}/decision", post(Self::decide_approval));
        routes
            .route("/admin/prices", get(Self::list_prices).post(Self::create_price))
            .route(
                "/admin/prices/{id}",
                get(Self::get_price).put(Self::update_price).delete(Self::delete_price),
            )
            .with_state(api)
    }

    async fn list_clients(
        State(api): State<Arc<PayerApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<AdminPageQuery>,
    ) -> std::result::Result<Json<Page<crate::model::PaymentClient>>, ApiError> {
        Ok(Json(api.clients.list(query.page()?).await?))
    }

    async fn list_scenarios(
        State(api): State<Arc<PayerApi>>,
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
        State(api): State<Arc<PayerApi>>,
        _principal: CallerPrincipal,
        Json(request): Json<CreateScenarioRequest>,
    ) -> std::result::Result<Json<ScenarioConfiguration>, ApiError> {
        let now = PayerApi::unix_time()?;
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
        State(api): State<Arc<PayerApi>>,
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
        State(api): State<Arc<PayerApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<AdminPageQuery>,
    ) -> std::result::Result<Json<Page<crate::model::wallet::Wallet>>, ApiError> {
        Ok(Json(api.wallets.list(query.page()?).await?))
    }

    async fn create_wallet_import_session(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
    ) -> std::result::Result<axum::response::Response, ApiError> {
        let session = api
            .wallet_handler
            .create_import_session(principal.as_str(), PayerApi::unix_time()?)
            .await?;
        Ok(([(header::CACHE_CONTROL, "no-store")], Json(session)).into_response())
    }

    async fn create_wallet(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Json(request): Json<crate::model::CreateEncryptedWallet>,
    ) -> std::result::Result<Json<crate::model::wallet::Wallet>, ApiError> {
        Ok(Json(
            api.wallet_handler
                .create_wallet(principal.as_str(), request, PayerApi::unix_time()?)
                .await?,
        ))
    }

    async fn list_prices(
        State(api): State<Arc<PayerApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<ScenarioPageQuery>,
    ) -> std::result::Result<Json<Page<PaymentPrice>>, ApiError> {
        Ok(Json(
            api.prices
                .list_for_scenario(&query.scenario_id, query.page.page()?)
                .await?,
        ))
    }

    async fn create_price(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Json(request): Json<CreatePriceRequest>,
    ) -> std::result::Result<(StatusCode, Json<PaymentPrice>), ApiError> {
        api.scenarios
            .authorize(principal.as_str(), &request.0.scenario_id)
            .await?;
        let price = request.0.into_domain(PayerApi::unix_time()?);
        Ok((StatusCode::CREATED, Json(api.prices.register(&price).await?)))
    }

    async fn authorized_price(
        api: &PayerApi,
        principal: &str,
        id: &str,
    ) -> std::result::Result<PaymentPrice, ApiError> {
        let price = api
            .prices
            .get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment price {id}")))?;
        api.scenarios.authorize(principal, &price.scenario_id).await?;
        Ok(price)
    }

    async fn get_price(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Path(id): Path<String>,
    ) -> std::result::Result<Json<PaymentPrice>, ApiError> {
        Ok(Json(Self::authorized_price(&api, principal.as_str(), &id).await?))
    }

    async fn update_price(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Path(id): Path<String>,
        Json(request): Json<UpdatePriceRequest>,
    ) -> std::result::Result<Json<PaymentPrice>, ApiError> {
        let stored = Self::authorized_price(&api, principal.as_str(), &id).await?;
        let (price, expected) = request.into_domain(&id, &stored, PayerApi::unix_time()?)?;
        Ok(Json(api.prices.update(&price, expected).await?))
    }

    async fn delete_price(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Path(id): Path<String>,
        Query(request): Query<DeletePriceRequest>,
    ) -> std::result::Result<StatusCode, ApiError> {
        Self::authorized_price(&api, principal.as_str(), &id).await?;
        api.prices.delete(&id, request.expected_updated_at).await?;
        Ok(StatusCode::NO_CONTENT)
    }

    async fn list_payments(
        State(api): State<Arc<PayerApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<PaymentListQuery>,
    ) -> std::result::Result<Json<Page<PaymentIntent>>, ApiError> {
        Ok(Json(api.payments.list(&query.into_domain()?).await?))
    }

    async fn list_ledger(
        State(api): State<Arc<PayerApi>>,
        _principal: CallerPrincipal,
        Query(query): Query<LedgerListQuery>,
    ) -> std::result::Result<Json<Page<PaymentLedgerEntry>>, ApiError> {
        Ok(Json(api.ledger.list(&query.into_domain()?).await?))
    }

    async fn authorize_payment(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Json(request): Json<AuthorizePaymentRequest>,
    ) -> std::result::Result<Json<AuthorizePaymentResponse>, ApiError> {
        Ok(Json(
            api.authorize(principal.as_str(), &request, PayerApi::unix_time()?)
                .await?,
        ))
    }

    async fn get_payment(
        State(api): State<Arc<PayerApi>>,
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
        State(api): State<Arc<PayerApi>>,
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
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Path(intent_id): Path<String>,
        Json(request): Json<ReportOutboundResponseRequest>,
    ) -> std::result::Result<Json<ReportOutboundResponseResponse>, ApiError> {
        Ok(Json(
            api.report_outbound_response(principal.as_str(), &intent_id, &request, PayerApi::unix_time()?)
                .await?,
        ))
    }

    async fn get_balance(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Query(query): Query<BalanceQuery>,
    ) -> std::result::Result<Json<PaymentBalance>, ApiError> {
        Ok(Json(
            api.get_balance(principal.as_str(), &query, PayerApi::unix_time()?)
                .await?,
        ))
    }

    async fn list_approvals(
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Query(query): Query<ApprovalQuery>,
    ) -> std::result::Result<Json<ApprovalPage>, ApiError> {
        if query.status.as_deref().is_some_and(|status| status != "pending") {
            return Err(Error::InvalidRequest("only pending approval queries are supported".to_owned()).into());
        }
        let page = query.page()?;
        Ok(Json(
            api.pending_approvals(principal.as_str(), &query.scenario_id, page, PayerApi::unix_time()?)
                .await?,
        ))
    }

    async fn get_approval(
        State(api): State<Arc<PayerApi>>,
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
        State(api): State<Arc<PayerApi>>,
        principal: CallerPrincipal,
        Path(approval_id): Path<String>,
        Json(request): Json<SubmitApprovalDecisionRequest>,
    ) -> std::result::Result<Json<ApprovalRequest>, ApiError> {
        if approval_id != request.approval_id {
            return Err(Error::PaymentConflict("approval path and body differ".to_owned()).into());
        }
        Ok(Json(
            api.decide_approval(principal.as_str(), &request, PayerApi::unix_time()?)
                .await?,
        ))
    }
}

use agentvault_common::util::api_error::ApiError;
