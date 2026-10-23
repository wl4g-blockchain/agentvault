use super::*;

#[test]
fn evidence_hash_binds_the_stable_run_id() {
    let first = merchant_evidence_hash(
        "x402",
        "intent-1",
        "run-1",
        "eip155:8453",
        &format!("0x{}", "11".repeat(32)),
        &format!("0x{}", "22".repeat(20)),
    );
    let changed = merchant_evidence_hash(
        "x402",
        "intent-1",
        "different-run",
        "eip155:8453",
        &format!("0x{}", "11".repeat(32)),
        &format!("0x{}", "22".repeat(20)),
    );
    assert_ne!(first, changed);
}

#[test]
fn protocol_evidence_hashes_are_isolated_without_changing_legacy_x402_identity() {
    let transaction = format!("0x{}", "11".repeat(32));
    let payer = format!("0x{}", "22".repeat(20));
    let x402 = merchant_evidence_hash("x402", "intent-1", "run-1", "eip155:8453", &transaction, &payer);
    let mpp = merchant_evidence_hash("mpp", "intent-1", "run-1", "eip155:8453", &transaction, &payer);
    assert_ne!(x402, mpp);
    assert_ne!(
        merchant_event_id("x402", "intent-1"),
        merchant_event_id("mpp", "intent-1")
    );
}
