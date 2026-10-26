use super::*;
#[test]
fn expired_non_settlement_releases_reserved_funds_but_not_consumed_settlement() {
    assert_eq!(
        SettlementFacts::reservation_after_non_settlement(PaymentStatus::Expired, ReservationStatus::Reserved),
        ReservationStatus::Released
    );
    assert_eq!(
        SettlementFacts::reservation_after_non_settlement(PaymentStatus::Expired, ReservationStatus::Consumed),
        ReservationStatus::Consumed
    );
}
