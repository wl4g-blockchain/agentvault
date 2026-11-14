use super::*;

#[test]
fn receipt_observations_distinguish_pending_from_mismatch() {
    assert_ne!(
        ReceiptObservation::Pending,
        ReceiptObservation::Mismatch {
            reason: "wrong transfer".to_owned(),
        }
    );
    assert!(matches!(
        OutboundEvidenceObservation::Unavailable {
            reason: "rpc unavailable".to_owned(),
        },
        OutboundEvidenceObservation::Unavailable { .. }
    ));
}
