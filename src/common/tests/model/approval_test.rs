use super::*;

#[test]
fn jwt_claims_bind_every_required_authorization_field() {
    let claims = ApprovalGrantClaims {
        iss: "agentvault".to_owned(),
        aud: "agentvault:approval-grant".to_owned(),
        jti: "grant-1".to_owned(),
        approval_id: "approval-1".to_owned(),
        intent_id: "intent-1".to_owned(),
        intent_hash: "intent-hash".to_owned(),
        amount: Amount::new(100),
        approver: "principal:admin".to_owned(),
        iat: 100,
        exp: 200,
    };
    assert!(claims.validate().is_ok());

    let mut invalid = claims;
    invalid.intent_hash.clear();
    assert_eq!(invalid.validate(), Err("intent_hash"));
}
