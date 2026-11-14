use super::*;

#[test]
fn approval_wire_values_are_stable_snake_case() {
    assert_eq!(serde_json::to_string(&ApprovalStatus::Pending).unwrap(), "\"pending\"");
    assert_eq!(
        serde_json::to_string(&ApprovalDecisionKind::Approve).unwrap(),
        "\"approve\""
    );
    assert_eq!(
        serde_json::from_str::<ApprovalStatus>("\"consumed\"").unwrap(),
        ApprovalStatus::Consumed
    );
}
