use axum::{
    extract::{Path, Query, State},
    Json,
};
use uuid::Uuid;

use crate::{
    api::{ApiError, ApiState},
    model::sign::{
        PaymentSignHistoryPage, PaymentSignRecord, PaymentSignRequest, PaymentSignResponse, SignHistoryQuery,
        SignTransport,
    },
};

pub async fn execute(
    State(state): State<ApiState>,
    Json(request): Json<PaymentSignRequest>,
) -> Json<PaymentSignResponse> {
    Json(state.sign.execute(&request, SignTransport::Http))
}

pub async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SignHistoryQuery>,
) -> Result<Json<PaymentSignHistoryPage>, ApiError> {
    Ok(Json(state.sign.search_history(&query)?))
}

pub async fn get(
    State(state): State<ApiState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<PaymentSignRecord>, ApiError> {
    state
        .sign
        .get_history(request_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("signing history record"))
}
