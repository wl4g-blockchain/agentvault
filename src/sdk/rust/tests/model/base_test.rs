use super::*;

#[test]
fn base_models_enforce_safe_run_identity_and_redact_credentials() {
    assert_eq!(OperationContext::new("scenario-1", "run-1").unwrap().run_id, "run-1");
    assert!(OperationContext::new("scenario-1", "unsafe/run").is_err());

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
