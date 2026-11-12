#[test]
fn handler_module_exports_feed_handler() {
    let _ = std::any::TypeId::of::<super::feed::FeedHandler>();
}
