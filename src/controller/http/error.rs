use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::Error;

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
            Error::PaymentConflict(_) | Error::InvalidTransition(_) => Self {
                status: StatusCode::CONFLICT,
                code: "PAYMENT_CONFLICT",
                message: error.to_string(),
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

#[cfg(test)]
#[path = "../../../tests/controller/http/error_test.rs"]
mod tests;
