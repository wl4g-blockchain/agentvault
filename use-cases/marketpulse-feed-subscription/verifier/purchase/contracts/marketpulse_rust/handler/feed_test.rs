use super::FeedHandler;

#[test]
fn feed_handler_is_cloneable_for_axum_state() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<FeedHandler>();
}

#[test]
fn feed_handler_owns_operational_endpoints() {
    fn accepts_handler<T>(_handler: T) {}
    accepts_handler(FeedHandler::health);
    accepts_handler(FeedHandler::metrics);
}
