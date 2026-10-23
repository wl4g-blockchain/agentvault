use super::*;

#[test]
fn admin_pages_are_bounded_and_default_to_ten_rows() {
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
fn admin_payment_and_ledger_queries_preserve_financial_filters() {
    let payments = PaymentListQuery {
        scenario_id: Some("scenario-1".to_owned()),
        direction: Some(PaymentDirection::Outbound),
        status: Some(PaymentStatus::Settled),
        page: AdminPageQuery {
            offset: Some(25),
            limit: Some(50),
        },
        ..PaymentListQuery::default()
    }
    .into_domain()
    .expect("payment query");
    assert_eq!(payments.scenario_id.as_deref(), Some("scenario-1"));
    assert_eq!(payments.direction, Some(PaymentDirection::Outbound));
    assert_eq!(payments.status, Some(PaymentStatus::Settled));
    assert_eq!(payments.page, PageRequest { offset: 25, limit: 50 });

    let ledger = LedgerListQuery {
        wallet_id: Some("wallet-1".to_owned()),
        asset: Some("USDC".to_owned()),
        page: AdminPageQuery {
            offset: Some(10),
            limit: Some(20),
        },
        ..LedgerListQuery::default()
    }
    .into_domain()
    .expect("ledger query");
    assert_eq!(ledger.wallet_id.as_deref(), Some("wallet-1"));
    assert_eq!(ledger.asset.as_deref(), Some("USDC"));
    assert_eq!(ledger.page, PageRequest { offset: 10, limit: 20 });
}

#[test]
fn scenario_creation_normalizes_multi_wallet_policy_membership() {
    let request = CreateScenarioRequest {
        id: "scenario-1".to_owned(),
        client_id: "client-1".to_owned(),
        name: "Market data payments".to_owned(),
        policy: InitialPolicyRequest {
            id: "policy-1".to_owned(),
            wallet_ids: vec!["wallet-b".to_owned(), "wallet-a".to_owned(), "wallet-b".to_owned()],
            protocols: vec!["x402".to_owned()],
            networks: vec!["eip155:31337".to_owned()],
            assets: vec![asset()],
            budget_unit: asset().budget_unit(),
            allowed_recipients: Vec::new(),
            allowed_origins: Vec::new(),
            inbound_limits: None,
            outbound_limits: Some(crate::model::PaymentPolicyLimits {
                max_per_payment: Amount::new(10),
                max_per_day: Amount::new(100),
                approval_threshold: None,
                approval_enabled: true,
                approval_overage: None,
            }),
            enabled: true,
        },
    };
    let (scenario, policy) = request.into_domain(123);
    assert_eq!(scenario.created_at, 123);
    assert_eq!(policy.scenario_id, scenario.id);
    assert_eq!(policy.wallet_ids, ["wallet-a", "wallet-b"]);
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

#[test]
fn outbound_evidence_supports_legacy_x402_and_protocol_owned_headers() {
    let context = OperationContextDto {
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
    };
    let legacy = ReportOutboundResponseRequest {
        context: context.clone(),
        payment_response: Some("YWJjZA==".to_owned()),
        protocol: None,
        evidence: None,
    }
    .evidence()
    .expect("legacy x402 evidence");
    assert_eq!(legacy.protocol, "x402");
    assert_eq!(legacy.header_name, "payment-response");

    let mpp = ReportOutboundResponseRequest {
        context: context.clone(),
        payment_response: None,
        protocol: Some("mpp".to_owned()),
        evidence: Some(ProtocolEvidenceDto {
            header_name: "payment-receipt".to_owned(),
            header_value: "receipt".to_owned(),
        }),
    }
    .evidence()
    .expect("MPP evidence");
    assert_eq!(mpp.protocol, "mpp");
    assert_eq!(mpp.header_name, "payment-receipt");

    assert!(ReportOutboundResponseRequest {
        context,
        payment_response: Some("legacy".to_owned()),
        protocol: Some("x402".to_owned()),
        evidence: Some(ProtocolEvidenceDto {
            header_name: "payment-response".to_owned(),
            header_value: "modern".to_owned(),
        }),
    }
    .evidence()
    .is_err());
}

#[test]
fn authorization_request_rejects_caller_selected_wallet_and_payment_projection() {
    let base = json!({
        "scenario_id": "scenario-1",
        "run_id": "run-1",
        "challenge": {
            "origin": "https://merchant.example",
            "resource": "https://merchant.example/mcp",
            "content_type": "application/json",
            "body": "{}"
        }
    });
    assert!(serde_json::from_value::<AuthorizePaymentRequest>(base.clone()).is_ok());
    for field in [
        "client_id",
        "wallet_id",
        "policy_id",
        "asset",
        "amount",
        "recipient",
        "protocol",
    ] {
        let mut request = base.clone();
        request[field] = json!("caller-selected");
        assert!(
            serde_json::from_value::<AuthorizePaymentRequest>(request).is_err(),
            "caller-controlled field {field} must be rejected"
        );
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

    let default = ApprovalQuery {
        scenario_id: "scenario-1".to_owned(),
        status: Some("pending".to_owned()),
        offset: None,
        limit: None,
    };
    assert_eq!(
        approval_page_request(&default).expect("default page"),
        PageRequest::default()
    );
}

#[test]
fn http_credential_debug_and_delivery_never_expose_nested_secrets() {
    let credential = PaymentCredential {
        protocol: "x402".to_owned(),
        challenge_hash: "challenge-secret".to_owned(),
        credential_hash: "credential-secret".to_owned(),
        payload: json!({"signature":"signature-secret"}),
    };
    let adapter = crate::protocol::x402::X402Adapter::new(
        vec![crate::protocol::x402::X402Asset {
            network: "eip155:31337".to_owned(),
            contract: "0x1111111111111111111111111111111111111111".to_owned(),
            symbol: "USDC".to_owned(),
            decimals: 6,
        }],
        60,
    )
    .expect("x402 adapter");
    let dto =
        PaymentCredentialDto::for_http(credential, "https://merchant.example", &adapter).expect("HTTP credential");
    let debug = format!("{dto:?}");
    assert!(!debug.contains("challenge-secret"));
    assert!(!debug.contains("credential-secret"));
    assert!(!debug.contains("signature-secret"));
    assert!(!debug.contains(&dto.http.as_ref().expect("delivery").header_value));

    let encoded = dto.http.expect("delivery").header_value;
    let decoded = x402_types::util::b64::Base64Bytes::from(encoded.as_bytes())
        .decode()
        .expect("standard base64");
    assert_eq!(
        serde_json::from_slice::<Value>(&decoded).unwrap(),
        json!({"signature":"signature-secret"})
    );
    assert!(!String::from_utf8(decoded).unwrap().contains("challenge-secret"));
}
