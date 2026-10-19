use axum::{routing::get, Router};

use crate::handler::feed::FeedHandler;

/// Declares only ordinary operational routes; MCP belongs to `MarketPulseServer`.
pub struct FeedRouter {
    handler: FeedHandler,
}

impl FeedRouter {
    #[must_use]
    pub const fn new(handler: FeedHandler) -> Self {
        Self { handler }
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/healthz", get(FeedHandler::health))
            .route("/metrics", get(FeedHandler::metrics))
            .with_state(self.handler)
    }
}

#[cfg(test)]
#[path = "../../tests/route/feed_test.rs"]
mod tests;
