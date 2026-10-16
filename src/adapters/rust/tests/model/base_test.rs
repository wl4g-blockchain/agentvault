use super::*;

#[test]
fn base_models_enforce_stable_idempotency_and_redact_credentials() {
    let key = IdempotencyKey::for_operation("run-1", "realtime").expect("stable key");
    assert_eq!(key.as_str(), "run-1:realtime");
    assert!(IdempotencyKey::new("unsafe/key").is_err());

    let credential = PaymentCredential {
        protocol: "x402".to_owned(),
        challenge_hash: "challenge-secret".to_owned(),
        credential_hash: "credential-secret".to_owned(),
        payload: serde_json::json!({"signature": "signature-secret"}),
        http: Some(HttpCredential {
            bound_origin: "https://merchant.test".to_owned(),
            header_name: "PAYMENT-SIGNATURE".to_owned(),
            header_value: "header-secret".to_owned(),
        }),
    };
    let debug = format!("{credential:?}");
    assert!(!debug.contains("challenge-secret"));
    assert!(!debug.contains("credential-secret"));
    assert!(!debug.contains("signature-secret"));
    assert!(!debug.contains("header-secret"));
}
