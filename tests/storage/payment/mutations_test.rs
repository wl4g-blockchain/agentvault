use crate::{model::PaymentStatus, storage::payment::PaymentStorageRules};
#[test]
fn terminal_payment_facts_cannot_be_reopened_for_another_signature() {
    for terminal in [PaymentStatus::Settled, PaymentStatus::Failed, PaymentStatus::Expired] {
        assert!(!PaymentStorageRules::allowed_status_transition(
            terminal,
            PaymentStatus::Authorized
        ));
        assert!(!PaymentStorageRules::allowed_status_transition(
            terminal,
            PaymentStatus::Signed
        ));
    }
}
