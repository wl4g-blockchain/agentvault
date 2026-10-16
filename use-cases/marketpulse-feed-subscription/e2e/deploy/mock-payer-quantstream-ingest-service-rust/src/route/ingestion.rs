use axum::{routing::get, Router};

use crate::handler::ingestion::IngestionHandler;

pub struct IngestionRouter {
    handler: IngestionHandler,
}

impl IngestionRouter {
    #[must_use]
    pub const fn new(handler: IngestionHandler) -> Self {
        Self { handler }
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/healthz", get(IngestionHandler::health))
            .route("/metrics", get(IngestionHandler::metrics))
            .with_state(self.handler)
    }
}

#[cfg(test)]
#[path = "../../tests/route/ingestion_test.rs"]
mod tests;
