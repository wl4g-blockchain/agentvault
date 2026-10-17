use super::*;
use crate::utils::secure_material::Aes256GcmMaterialProtector;

fn pending_approval() -> ApprovalRequest {
    ApprovalRequest {
        id: "approval-1".to_owned(),
        intent_id: "intent-1".to_owned(),
        run_id: "run-1".to_owned(),
        requested_amount: Amount::new(10),
        approved_amount: None,
        requested_by: "requester".to_owned(),
        approved_by: None,
        status: ApprovalStatus::Pending,
        intent_hash: "intent-hash".to_owned(),
        grant_jti: None,
        grant_claims_hash: None,
        sealed_grant_token: None,
        expires_at: 1_000,
        consumed_at: None,
        created_at: 100,
        approved_at: None,
    }
}

fn decision() -> ApprovalDecision {
    ApprovalDecision {
        approval_id: "approval-1".to_owned(),
        intent_id: "intent-1".to_owned(),
        decision: ApprovalDecisionKind::Approve,
        approved_amount: Some(Amount::new(10)),
        approved_by: "approver".to_owned(),
        reason: None,
        decided_at: 200,
    }
}

fn protector() -> Arc<dyn SensitiveMaterialProtector> {
    Arc::new(Aes256GcmMaterialProtector::new("recovery-v1", [("recovery-v1".to_owned(), [9_u8; 32])]).unwrap())
}

fn persisted_approval(grant: &ApprovalGrant) -> ApprovalRequest {
    let mut approval = pending_approval();
    approval.status = ApprovalStatus::Approved;
    approval.approved_amount = Some(grant.claims.amount);
    approval.approved_by = Some(grant.claims.approver.clone());
    approval.grant_jti = Some(grant.claims.jti.clone());
    approval.grant_claims_hash = Some(grant.claims_hash.clone());
    approval.sealed_grant_token = Some(grant.sealed_token.clone());
    approval.expires_at = grant.claims.exp;
    approval.approved_at = Some(grant.claims.iat);
    approval
}

#[test]
fn signed_jwt_round_trips_through_aead_and_rotated_key_ring() {
    let issuer = JwtApprovalGrantTokenService::new(
        "agentvault",
        "agentvault:approval-grant",
        "jwt-v1",
        [("jwt-v1".to_owned(), [1_u8; 32])],
        protector(),
    )
    .unwrap();
    let grant = issuer.issue(&pending_approval(), &decision(), 300).unwrap();
    assert!(!grant.sealed_token.ciphertext.contains("eyJ"));

    let rotated = JwtApprovalGrantTokenService::new(
        "agentvault",
        "agentvault:approval-grant",
        "jwt-v2",
        [("jwt-v1".to_owned(), [1_u8; 32]), ("jwt-v2".to_owned(), [2_u8; 32])],
        protector(),
    )
    .unwrap();
    let verified = rotated.verify(&persisted_approval(&grant), 250).unwrap();
    assert_eq!(verified.claims.jti, grant.claims.jti);
    assert_eq!(verified.claims.amount, Amount::new(10));
}

#[test]
fn tampered_envelope_or_binding_fails_closed() {
    let service = JwtApprovalGrantTokenService::new(
        "agentvault",
        "agentvault:approval-grant",
        "jwt-v1",
        [("jwt-v1".to_owned(), [1_u8; 32])],
        protector(),
    )
    .unwrap();
    let grant = service.issue(&pending_approval(), &decision(), 300).unwrap();
    let mut approval = persisted_approval(&grant);
    approval.intent_hash = "other-intent-hash".to_owned();
    assert!(service.verify(&approval, 250).is_err());

    let mut approval = persisted_approval(&grant);
    approval.sealed_grant_token.as_mut().unwrap().ciphertext.push('A');
    assert!(service.verify(&approval, 250).is_err());
}

#[test]
fn expired_or_wrong_audience_jwt_fails_closed() {
    let service = JwtApprovalGrantTokenService::new(
        "agentvault",
        "agentvault:approval-grant",
        "jwt-v1",
        [("jwt-v1".to_owned(), [1_u8; 32])],
        protector(),
    )
    .unwrap();
    let grant = service.issue(&pending_approval(), &decision(), 300).unwrap();
    let approval = persisted_approval(&grant);
    assert!(service.verify(&approval, 300).is_err());

    let wrong_audience = JwtApprovalGrantTokenService::new(
        "agentvault",
        "agentvault:other-purpose",
        "jwt-v1",
        [("jwt-v1".to_owned(), [1_u8; 32])],
        protector(),
    )
    .unwrap();
    assert!(wrong_audience.verify(&approval, 250).is_err());
}

#[test]
fn grant_expiry_never_uses_the_longer_request_ttl() {
    assert_eq!(grant_expiration(1_000, 100, 300), 400);
    assert_eq!(grant_expiration(200, 100, 300), 200);
}
