//! `PaymentScenario` persistence-entity invariants.

use super::*;

#[test]
fn scenario_ownership_is_exact_and_required() {
    let scenario = PaymentScenario {
        id: "scenario".to_owned(),
        client_id: "client".to_owned(),
        name: "Market data payments".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    assert!(scenario.belongs_to("client"));
    assert!(!scenario.belongs_to("other"));
    assert!(scenario.validate().is_ok());
}
