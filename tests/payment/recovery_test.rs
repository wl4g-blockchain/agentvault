use super::*;
#[test]
fn confirmed_provider_result_cannot_substitute_another_amount() {
    let intent = crate::payment::reconciliation::tests::intent(PaymentStatus::PendingSettlement);
    let mut result = SettlementProviderResult {
        valid: true,
        settled: true,
        payer: Some(format!("0x{}", "33".repeat(20))),
        transaction: Some(format!("0x{}", "11".repeat(32))),
        settlement_ref: Some("settlement".into()),
        effect_ref: Some("log:1".into()),
        invalid_reason: None,
        asset: Some(intent.asset.clone()),
        amount: Some(crate::model::Amount::new(1)),
        recipient: Some(intent.recipient.clone()),
        confirmations: Some(3),
        extensions: std::collections::BTreeMap::default(),
    };
    assert!(
        PaymentEvidenceRouter::provider_result_to_evidence(&intent, 100, "facilitator", 1, result.clone()).is_err()
    );
    result.amount = Some(intent.amount);
    assert!(matches!(
        PaymentEvidenceRouter::provider_result_to_evidence(&intent, 100, "facilitator", 1, result.clone()).unwrap(),
        ReconciliationEvidence::Confirmed(_)
    ));
    result.valid = false;
    assert!(matches!(
        PaymentEvidenceRouter::provider_result_to_evidence(&intent, 100, "facilitator", 1, result).unwrap(),
        ReconciliationEvidence::Unknown { .. }
    ));
}
