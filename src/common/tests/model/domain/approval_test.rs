use super::*;

#[test]
fn api_serialization_never_contains_the_sealed_jwt() {
    let approval = ApprovalRequest {
        id: "approval-1".to_owned(),
        intent_id: "intent-1".to_owned(),
        run_id: "run-1".to_owned(),
        requested_amount: Amount::new(100),
        approved_amount: Some(Amount::new(100)),
        requested_by: "requester".to_owned(),
        approved_by: Some("approver".to_owned()),
        status: ApprovalStatus::Approved,
        intent_hash: "intent-hash".to_owned(),
        grant_jti: Some("grant-1".to_owned()),
        grant_claims_hash: Some("claims-hash".to_owned()),
        sealed_grant_token: Some(SealedSensitiveMaterial {
            version: 1,
            key_id: "recovery-v1".to_owned(),
            nonce: "secret-nonce".to_owned(),
            ciphertext: "secret-ciphertext".to_owned(),
        }),
        expires_at: 200,
        consumed_at: None,
        created_at: 100,
        approved_at: Some(101),
    };

    let encoded = serde_json::to_string(&approval).unwrap();
    assert!(!encoded.contains("secret-nonce"));
    assert!(!encoded.contains("secret-ciphertext"));
    assert!(!encoded.contains("sealed_grant_token"));
}
