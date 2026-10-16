#[test]
fn route_module_exports_feed_router() {
    let _ = std::any::TypeId::of::<super::feed::FeedRouter>();
}
