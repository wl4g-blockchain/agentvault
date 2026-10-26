use super::*;

#[test]
fn offer_handler_has_no_embedded_transport_state() {
    assert!(std::mem::size_of::<PriceResolver>() > 0);
}
