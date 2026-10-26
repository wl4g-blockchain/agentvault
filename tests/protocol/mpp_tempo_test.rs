use super::*;

#[test]
fn signing_payload_composes_the_upstream_transaction_and_rejects_unknown_fields() {
    let payload = TempoSigningPayload {
        kind: TEMPO_SIGNING_PAYLOAD_KIND.into(),
        payer: "0x1111111111111111111111111111111111111111".into(),
        transaction: TempoTransaction::default(),
    };
    let mut wire = serde_json::to_value(payload).unwrap();
    wire["untrusted_digest"] = json!("arbitrary");
    assert!(serde_json::from_value::<TempoSigningPayload>(wire).is_err());
}
