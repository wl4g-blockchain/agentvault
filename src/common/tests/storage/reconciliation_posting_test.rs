use super::*;
#[test]
fn posting_identity_is_stable_without_erasing_material_changes() {
    assert_eq!(
        ReconciliationPosting::stable_id("ledger", "intent-a"),
        ReconciliationPosting::stable_id("ledger", "intent-a")
    );
    assert_ne!(
        ReconciliationPosting::stable_id("ledger", "intent-a"),
        ReconciliationPosting::stable_id("ledger", "intent-b")
    );
}
