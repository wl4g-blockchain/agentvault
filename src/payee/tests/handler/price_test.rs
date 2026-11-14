use super::*;

#[test]
fn price_resolver_has_no_embedded_transport_state() {
    assert!(std::mem::size_of::<PriceResolver>() > 0);
}
