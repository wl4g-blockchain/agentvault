use crate::{
    handler::PayeeApi,
    model::payment_api::{
        CreateChallengeRequest, PaymentVerification, ResolvePriceRequest, SettlePaymentRequest, SettledPaymentResponse,
        VerifyPaymentRequest,
    },
    model::{PaymentChallenge, PaymentPrice},
};
use agentvault_common::{
    util::api_error::ApiError, util::clock::SystemUnixClock, util::clock::UnixClock,
    util::principal_context::CallerPrincipal,
};
use axum::{extract::State, routing::post, Json, Router};
use std::sync::Arc;
pub struct PayeeRoutes;
impl PayeeRoutes {
    pub fn router(api: Arc<PayeeApi>) -> Router {
        Router::new()
            .route("/prices/resolve", post(Self::resolve_price))
            .route("/payee/challenges", post(Self::create_challenge))
            .route("/payee/verify", post(Self::verify_payment))
            .route("/payee/settle", post(Self::settle_payment))
            .with_state(api)
    }
    async fn resolve_price(
        State(api): State<Arc<PayeeApi>>,
        principal: CallerPrincipal,
        Json(request): Json<ResolvePriceRequest>,
    ) -> std::result::Result<Json<PaymentPrice>, ApiError> {
        Ok(Json(api.resolve_price(principal.as_str(), &request).await?))
    }
    async fn create_challenge(
        State(api): State<Arc<PayeeApi>>,
        principal: CallerPrincipal,
        Json(request): Json<CreateChallengeRequest>,
    ) -> std::result::Result<Json<PaymentChallenge>, ApiError> {
        Ok(Json(
            api.create_challenge(principal.as_str(), &request, SystemUnixClock.now()?)
                .await?,
        ))
    }
    async fn verify_payment(
        State(api): State<Arc<PayeeApi>>,
        principal: CallerPrincipal,
        Json(request): Json<VerifyPaymentRequest>,
    ) -> std::result::Result<Json<PaymentVerification>, ApiError> {
        Ok(Json(
            api.verify_payment(principal.as_str(), &request, SystemUnixClock.now()?)
                .await?,
        ))
    }
    async fn settle_payment(
        State(api): State<Arc<PayeeApi>>,
        principal: CallerPrincipal,
        Json(request): Json<SettlePaymentRequest>,
    ) -> std::result::Result<Json<SettledPaymentResponse>, ApiError> {
        Ok(Json(
            api.settle_payment(principal.as_str(), &request, SystemUnixClock.now()?)
                .await?,
        ))
    }
}
