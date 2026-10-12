use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::{
    api::{ApiError, ApiState},
    model::wallet::{CreateWalletRequest, Wallet, WalletList},
};

pub async fn create(
    State(state): State<ApiState>,
    Json(request): Json<CreateWalletRequest>,
) -> Result<(StatusCode, Json<Wallet>), ApiError> {
    let wallet = state.wallet.create(&request.id)?;
    Ok((StatusCode::CREATED, Json(wallet)))
}

pub async fn list(State(state): State<ApiState>) -> Result<Json<WalletList>, ApiError> {
    let items = state.wallet.list()?;
    let total = items.len();
    Ok(Json(WalletList { items, total }))
}

pub async fn get(State(state): State<ApiState>, Path(id): Path<String>) -> Result<Json<Wallet>, ApiError> {
    Ok(Json(state.wallet.get(&id)?))
}

pub async fn rotate(State(state): State<ApiState>, Path(id): Path<String>) -> Result<Json<Wallet>, ApiError> {
    Ok(Json(state.wallet.rotate(&id)?))
}

pub async fn delete(State(state): State<ApiState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    state.wallet.delete(&id)?;
    Ok(StatusCode::NO_CONTENT)
}
