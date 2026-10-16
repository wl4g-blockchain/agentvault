use super::*;

#[test]
fn admin_pages_are_bounded_and_default_to_one_hundred_rows() {
    assert_eq!(AdminPageQuery::default().page().unwrap(), PageRequest::default());
    assert!(AdminPageQuery {
        offset: None,
        limit: Some(0),
    }
    .page()
    .is_err());
    assert!(AdminPageQuery {
        offset: None,
        limit: Some(PAGE_SIZE + 1),
    }
    .page()
    .is_err());
}

#[test]
fn advertised_expiry_accepts_only_canonical_unix_seconds() {
    assert_eq!(parse_advertised_expiry(None).unwrap(), None);
    assert_eq!(
        parse_advertised_expiry(Some("1791922093")).unwrap(),
        Some(1_791_922_093)
    );
    for invalid in ["", "0x12", "-1", "+1", "01", "18446744073709551615"] {
        assert!(matches!(
            parse_advertised_expiry(Some(invalid)),
            Err(Error::InvalidRequest(_))
        ));
    }
}
use crate::model::LedgerEntryType;

fn asset() -> AssetIdentity {
    AssetIdentity {
        network: "eip155:31337".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    }
}

fn entry(
    id: &str,
    entry_type: LedgerEntryType,
    direction: PaymentDirection,
    amount: u128,
    reversal_of: Option<&str>,
) -> PaymentLedgerEntry {
    PaymentLedgerEntry {
        id: id.to_owned(),
        intent_id: Some("intent-1".to_owned()),
        scenario_id: Some("scenario-1".to_owned()),
        wallet_id: "wallet-1".to_owned(),
        entry_type,
        direction,
        asset: asset(),
        amount: Amount::new(amount),
        posting_key: format!("posting-{id}"),
        tx_hash: Some(format!("0x{id}")),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: reversal_of.map(str::to_owned),
        effective_at: 10,
        created_at: 10,
    }
}

#[test]
fn balance_reversals_negate_the_original_direction_independent_of_order() {
    let outbound = entry(
        "outbound",
        LedgerEntryType::Payment,
        PaymentDirection::Outbound,
        10,
        None,
    );
    let reversal = entry(
        "reversal",
        LedgerEntryType::Reversal,
        PaymentDirection::Inbound,
        10,
        Some("outbound"),
    );
    let inbound = entry("inbound", LedgerEntryType::Payment, PaymentDirection::Inbound, 7, None);
    let totals = confirmed_totals(&[reversal, inbound, outbound]).expect("valid totals");
    assert_eq!(totals, (Amount::new(7), Amount::ZERO));
}

#[test]
fn approval_page_query_is_bounded_and_preserves_offset() {
    let query = ApprovalQuery {
        scenario_id: "scenario-1".to_owned(),
        status: Some("pending".to_owned()),
        offset: Some(125),
        limit: Some(25),
    };
    assert_eq!(
        approval_page_request(&query).expect("bounded page"),
        PageRequest { offset: 125, limit: 25 }
    );

    let invalid = ApprovalQuery {
        limit: Some(PAGE_SIZE + 1),
        ..query
    };
    assert!(matches!(approval_page_request(&invalid), Err(Error::InvalidRequest(_))));
}

#[test]
fn http_credential_debug_and_delivery_never_expose_nested_secrets() {
    let credential = PaymentCredential {
        protocol: "x402".to_owned(),
        challenge_hash: "challenge-secret".to_owned(),
        credential_hash: "credential-secret".to_owned(),
        payload: json!({"signature":"signature-secret"}),
    };
    let dto = PaymentCredentialDto::for_http(credential, "https://merchant.example").expect("HTTP credential");
    let debug = format!("{dto:?}");
    assert!(!debug.contains("challenge-secret"));
    assert!(!debug.contains("credential-secret"));
    assert!(!debug.contains("signature-secret"));
    assert!(!debug.contains(&dto.http.as_ref().expect("delivery").header_value));

    let encoded = dto.http.expect("delivery").header_value;
    let decoded = Base64Bytes::from(encoded.as_bytes()).decode().expect("standard base64");
    assert_eq!(
        serde_json::from_slice::<Value>(&decoded).unwrap(),
        json!({"signature":"signature-secret"})
    );
    assert!(!String::from_utf8(decoded).unwrap().contains("challenge-secret"));
}
