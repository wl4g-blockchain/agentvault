use super::*;

#[test]
fn stored_amounts_reject_fractional_negative_and_overflow_values() {
    let maximum = Amount::new(u128::MAX);
    let encoded = PaymentStorageCodec::encode_postgres_amount(maximum).unwrap();
    assert_eq!(
        PaymentStorageCodec::decode_postgres_amount(&encoded, "amount").unwrap(),
        maximum
    );
    assert_eq!(
        PaymentStorageCodec::decode_sqlite_amount(&maximum.to_string(), "amount").unwrap(),
        maximum
    );
    for invalid in ["-1", "1.5", "340282366920938463463374607431768211456"] {
        assert!(PaymentStorageCodec::decode_sqlite_amount(invalid, "amount").is_err());
        assert!(PaymentStorageCodec::decode_postgres_amount(
            &PgAmount(BigDecimal::from_str(invalid).unwrap()),
            "amount"
        )
        .is_err());
    }
}

#[test]
fn stored_states_fail_closed_for_unknown_or_noncanonical_values() {
    assert_eq!(
        PaymentStorageCodec::payment_status("settled").unwrap(),
        PaymentStatus::Settled
    );
    assert_eq!(
        PaymentStorageCodec::reservation_status("consumed").unwrap(),
        ReservationStatus::Consumed
    );
    assert_eq!(PaymentStorageCodec::custody("local").unwrap(), WalletCustody::Local);
    assert_eq!(
        PaymentStorageCodec::key_status("retired").unwrap(),
        WalletKeyStatus::Retired
    );
    for invalid in ["", "SETTLED", " settled", "settled; DELETE FROM pay_intent"] {
        assert!(PaymentStorageCodec::payment_status(invalid).is_err());
        assert!(PaymentStorageCodec::reservation_status(invalid).is_err());
        assert!(PaymentStorageCodec::approval_status(invalid).is_err());
        assert!(PaymentStorageCodec::event_type(invalid).is_err());
        assert!(PaymentStorageCodec::ledger_type(invalid).is_err());
        assert!(PaymentStorageCodec::outbox_status(invalid).is_err());
        assert!(PaymentStorageCodec::custody(invalid).is_err());
        assert!(PaymentStorageCodec::key_status(invalid).is_err());
    }
}
