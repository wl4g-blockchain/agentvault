use crate::storage::payment::PaymentStorageRules;
#[test]
fn unknown_direction_cannot_promote_a_terminal_intent() {
    use crate::model::PaymentStatus;
    assert!(!PaymentStorageRules::allowed_status_transition(
        PaymentStatus::Settled,
        PaymentStatus::Authorized
    ));
    assert!(!PaymentStorageRules::allowed_status_transition(
        PaymentStatus::Failed,
        PaymentStatus::Signed
    ));
}
