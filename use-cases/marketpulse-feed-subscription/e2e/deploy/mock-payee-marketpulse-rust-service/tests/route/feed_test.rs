use super::{FeedHandler, FeedRouter};
use axum::Router;

#[test]
fn feed_router_has_a_single_business_composition_type() {
    fn accepts_constructor(_constructor: fn(FeedHandler) -> FeedRouter) {}
    fn accepts_router(_router: fn(FeedRouter) -> Router) {}
    accepts_constructor(FeedRouter::new);
    accepts_router(FeedRouter::router);
}
