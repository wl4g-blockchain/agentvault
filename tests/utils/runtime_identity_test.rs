use super::*;

#[test]
fn runtime_ids_are_bounded_and_unique_per_instance() {
    let prefix = "agentvault".repeat(32);
    let first = unique_runtime_id(&prefix);
    let second = unique_runtime_id(&prefix);

    assert_ne!(first, second);
    assert!(first.len() <= MAX_RUNTIME_ID_BYTES);
    assert!(first.starts_with("agentvault"));
}
