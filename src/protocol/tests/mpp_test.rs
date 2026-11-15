use super::*;

fn adapter() -> MppAdapter {
    MppAdapter::new_payee(
        vec![MppAsset {
            network: "eip155:42431".to_owned(),
            currency: "0x1111111111111111111111111111111111111111".to_owned(),
            symbol: "USDC".to_owned(),
            token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
            decimals: 6,
        }],
        "merchant.example",
        "0123456789abcdef0123456789abcdef",
        60,
    )
    .unwrap()
}

fn payer_adapter() -> MppAdapter {
    MppAdapter::new_payer(
        vec![MppAsset {
            network: "eip155:42431".to_owned(),
            currency: "0x1111111111111111111111111111111111111111".to_owned(),
            symbol: "USDC".to_owned(),
            token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
            decimals: 6,
        }],
        60,
    )
    .unwrap()
}

fn adapter_with_two_assets() -> MppAdapter {
    MppAdapter::new_payee(
        vec![
            MppAsset {
                network: "eip155:42431".to_owned(),
                currency: "0x1111111111111111111111111111111111111111".to_owned(),
                symbol: "USDC".to_owned(),
                token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
                decimals: 6,
            },
            MppAsset {
                network: "eip155:1".to_owned(),
                currency: "0x3333333333333333333333333333333333333333".to_owned(),
                symbol: "USDC".to_owned(),
                token_contract: Some("0x3333333333333333333333333333333333333333".to_owned()),
                decimals: 6,
            },
        ],
        "merchant.example",
        "0123456789abcdef0123456789abcdef",
        60,
    )
    .unwrap()
}

fn second_asset() -> AssetIdentity {
    AssetIdentity {
        network: "eip155:1".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x3333333333333333333333333333333333333333".to_owned()),
        decimals: 6,
    }
}

fn replace_charge_request(adapter: &MppAdapter, challenge: &mut PaymentChallenge, request: &ChargeRequest) {
    let original = MppWire::official_challenge(challenge).unwrap();
    let request = Base64UrlJson::from_typed(request).unwrap();
    let replacement = mpp::PaymentChallenge::with_secret_key_full(
        adapter.payee.as_ref().expect("payee adapter").challenge_secret.as_str(),
        &original.realm,
        original.method.as_str(),
        original.intent.as_str(),
        request,
        original.expires.as_deref(),
        original.digest.as_deref(),
        original.description.as_deref(),
        original.opaque.clone(),
        original.header.as_deref(),
    );
    let header = format_www_authenticate(&replacement).unwrap();
    challenge.nonce = replacement.id;
    challenge.challenge_hash = MppWire::challenge_hash(&challenge.resource, &header);
    challenge.payload = json!({"www_authenticate": header});
}

fn credential_for(challenge: &PaymentChallenge) -> PaymentCredential {
    let official = MppWire::official_challenge(challenge).unwrap();
    let credential = mpp::PaymentCredential::new(
        official.to_echo(),
        mpp::PaymentPayload::hash(format!("0x{}", "ab".repeat(32))),
    );
    PaymentCredential {
        protocol: MPP_PROTOCOL.to_owned(),
        challenge_hash: challenge.challenge_hash.clone(),
        credential_hash: "untrusted".to_owned(),
        payload: json!({
            "authorization": format_authorization(&credential).unwrap(),
        }),
    }
}

fn matching_intent(challenge: &PaymentChallenge) -> PaymentIntent {
    PaymentIntent {
        id: "intent-1".to_owned(),
        scenario_id: challenge.scenario_id.clone(),
        run_id: "run-1".to_owned(),
        wallet_id: Some("wallet-1".to_owned()),
        policy_id: Some("policy-1".to_owned()),
        price_id: None,
        direction: crate::model::PaymentDirection::Outbound,
        protocol: MPP_PROTOCOL.to_owned(),
        asset: challenge.asset.clone(),
        recipient: challenge.recipient.clone(),
        amount: challenge.amount,
        settled_amount: Amount::ZERO,
        resource: challenge.resource.clone(),
        challenge_hash: Some(challenge.challenge_hash.clone()),
        request_hash: "request-1".to_owned(),
        status: PaymentStatus::Created,
        reservation_status: crate::model::ReservationStatus::Unreserved,
        reserved_amount: Amount::ZERO,
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at: 1,
        updated_at: 1,
        expires_at: Some(challenge.expires_at),
        settled_at: None,
    }
}

async fn assert_integrity_rejected(adapter: &MppAdapter, challenge: &PaymentChallenge) {
    assert!(adapter
        .validate_payment(challenge, &matching_intent(challenge))
        .await
        .is_err());
    let credential = PaymentCredential {
        protocol: MPP_PROTOCOL.to_owned(),
        challenge_hash: challenge.challenge_hash.clone(),
        credential_hash: "untrusted".to_owned(),
        payload: json!({}),
    };
    assert!(adapter.settle_payment(challenge, &credential).await.is_err());
}

fn price() -> PaymentPrice {
    PaymentPrice {
        id: "price-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        policy_id: "policy-1".to_owned(),
        resource: "https://merchant.example/tool".to_owned(),
        protocols: vec![MPP_PROTOCOL.to_owned()],
        asset: AssetIdentity {
            network: "eip155:42431".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
            decimals: 6,
        },
        amount: Amount::new(1_000_000),
        wallet_id: "payee".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    }
}

#[tokio::test]
async fn official_sdk_round_trips_charge_challenge() {
    let now = 2_000_000_000;
    let challenge = adapter()
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let parsed = adapter()
        .parse_challenge(
            "payer-scenario",
            &price().resource,
            &challenge.payload,
            now,
            Some(challenge.expires_at),
        )
        .await
        .unwrap();
    assert_eq!(parsed.amount, price().amount);
    assert_eq!(parsed.asset, price().asset);
    assert_eq!(parsed.nonce, challenge.nonce);
    assert_eq!(parsed.challenge_hash, challenge.challenge_hash);
    assert_eq!(
        MppWire::official_challenge(&challenge).unwrap().header.as_deref(),
        Some("Payment-Authorization")
    );

    let second = adapter()
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    assert_ne!(second.challenge_hash, challenge.challenge_hash);
}

#[tokio::test]
async fn http_provider_preserves_bearer_with_the_advertised_credential_header() {
    let now = 2_000_000_000;
    let challenge = adapter()
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let credential = credential_for(&challenge);
    let delivery = payer_adapter().encode_http_credential(&credential).unwrap();
    assert_eq!(delivery.header_name, "payment-authorization");

    let mut official = MppWire::official_challenge(&challenge).unwrap();
    official.header = None;
    let default = mpp::PaymentCredential::new(
        official.to_echo(),
        mpp::PaymentPayload::hash(format!("0x{}", "ab".repeat(32))),
    );
    let default = PaymentCredential {
        protocol: MPP_PROTOCOL.to_owned(),
        challenge_hash: "challenge".to_owned(),
        credential_hash: "credential".to_owned(),
        payload: json!({"authorization": format_authorization(&default).unwrap()}),
    };
    assert_eq!(
        payer_adapter().encode_http_credential(&default).unwrap().header_name,
        "authorization"
    );
}

#[test]
fn http_provider_decodes_tempo_charge_receipts() {
    let transaction = format!("0x{}", "11".repeat(32));
    let receipt = mpp::Receipt::success(TEMPO_METHOD, transaction.clone())
        .to_header()
        .unwrap();
    let evidence = payer_adapter().decode_merchant_evidence(&receipt).unwrap();
    assert!(evidence.accepted);
    assert_eq!(evidence.transaction.as_deref(), Some(transaction.as_str()));
    assert_eq!(evidence.network, None);
    assert_eq!(evidence.payer, None);
}

#[tokio::test]
async fn payer_adapter_consumes_remote_challenges_but_cannot_issue_them() {
    let now = 2_000_000_000;
    let challenge = adapter()
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let payer = payer_adapter();
    assert!(payer.build_challenge(&price(), "recipient", now).await.is_err());

    let parsed = payer
        .parse_challenge(
            "payer-scenario",
            &price().resource,
            &challenge.payload,
            now,
            Some(challenge.expires_at),
        )
        .await
        .unwrap();
    assert_eq!(parsed.challenge_hash, challenge.challenge_hash);
}

#[tokio::test]
async fn payer_builds_an_official_tempo_transaction_credential() {
    let now = 2_000_000_000;
    let resource = price().resource;
    let issued = adapter()
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let payer_adapter = payer_adapter();
    let challenge = payer_adapter
        .parse_challenge(
            "payer-scenario",
            &resource,
            &issued.payload,
            now,
            Some(issued.expires_at),
        )
        .await
        .unwrap();
    let key = crate::model::wallet::WalletCrypto::generate_private_key();
    let payer = crate::model::wallet::WalletCrypto::address(&key).unwrap();
    let signing = payer_adapter
        .prepare_signing(&challenge, &payer, now + 1)
        .await
        .unwrap();
    assert_eq!(signing.digest.len(), 32);
    assert_eq!(signing.payload["kind"], TEMPO_SIGNING_PAYLOAD_KIND);
    assert_eq!(signing.payload["payer"], payer);

    let signature = crate::model::wallet::WalletCrypto::sign_eip712_digest(&key, &signing.digest).unwrap();
    let credential = payer_adapter
        .complete_signing(&challenge, signing.clone(), &signature)
        .await
        .unwrap();
    let authorization = credential.payload["authorization"].as_str().unwrap();
    let official = parse_authorization(authorization).unwrap();
    assert!(official
        .source
        .as_deref()
        .is_some_and(|source| source.eq_ignore_ascii_case(&format!("did:pkh:eip155:42431:{payer}"))));
    let payload = official.charge_payload().unwrap();
    assert_eq!(payload.payload_type(), mpp::PayloadType::Transaction);
    assert!(payload.data().starts_with("0x76"));
    assert_eq!(
        credential.credential_hash,
        payer_adapter.credential_fingerprint(&challenge, &credential).unwrap()
    );
    assert!(payer_adapter
        .verify_credential(&challenge, &credential, now + 2)
        .await
        .is_ok());

    let other_key = crate::model::wallet::WalletCrypto::generate_private_key();
    let wrong_signature = crate::model::wallet::WalletCrypto::sign_eip712_digest(&other_key, &signing.digest).unwrap();
    assert!(payer_adapter
        .complete_signing(&challenge, signing.clone(), &wrong_signature)
        .await
        .is_err());

    let mut modified = signing;
    modified.payload["transaction"]["gas"] = json!(999_999_u64);
    let modified_signature = crate::model::wallet::WalletCrypto::sign_eip712_digest(&key, &modified.digest).unwrap();
    assert!(payer_adapter
        .complete_signing(&challenge, modified, &modified_signature)
        .await
        .is_err());
}

#[tokio::test]
async fn payer_selects_allowed_payment_challenge_and_ignores_bearer() {
    let now = 2_000_000_000;
    let issued = adapter()
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let payment = issued.payload["www_authenticate"].as_str().unwrap();
    let header = format!("Bearer realm=\"login\", {payment}");
    let parsed = payer_adapter()
        .parse_challenge(
            "payer-scenario",
            &price().resource,
            &json!({"www_authenticate": header}),
            now,
            Some(issued.expires_at),
        )
        .await
        .unwrap();
    assert_eq!(parsed.challenge_hash, issued.challenge_hash);

    assert!(payer_adapter()
        .parse_challenge(
            "payer-scenario",
            &price().resource,
            &json!({"www_authenticate": "Bearer realm=\"login\""}),
            now,
            None,
        )
        .await
        .is_err());
}

#[tokio::test]
async fn credential_echo_cannot_be_moved_to_another_payee_challenge() {
    let adapter = adapter();
    let recipient = "0x2222222222222222222222222222222222222222";
    let first = adapter
        .build_challenge(&price(), recipient, 2_000_000_000)
        .await
        .unwrap();
    let second = adapter
        .build_challenge(&price(), recipient, 2_000_000_000)
        .await
        .unwrap();
    assert_ne!(first.nonce, second.nonce);

    let mut stolen = credential_for(&first);
    stolen.challenge_hash = second.challenge_hash.clone();
    assert!(adapter
        .verify_credential(&second, &stolen, 2_000_000_001)
        .await
        .is_err());
    assert!(adapter.credential_fingerprint(&second, &stolen).is_err());
    assert!(adapter.settle_payment(&second, &stolen).await.is_err());
}

#[tokio::test]
async fn every_entrypoint_rejects_forged_outer_or_official_charge() {
    let adapter = adapter_with_two_assets();
    let challenge = adapter
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", 2_000_000_000)
        .await
        .unwrap();

    let mut outer_allowed_asset = challenge.clone();
    outer_allowed_asset.asset = second_asset();
    assert_integrity_rejected(&adapter, &outer_allowed_asset).await;
    let signed_payload = serde_json::to_value(mpp::PaymentPayload::hash(format!("0x{}", "ab".repeat(32)))).unwrap();
    assert!(adapter
        .build_credential(&outer_allowed_asset, signed_payload)
        .await
        .is_err());
    let credential = credential_for(&challenge);
    assert!(adapter
        .verify_credential(&outer_allowed_asset, &credential, 2_000_000_001)
        .await
        .is_err());
    assert!(adapter
        .credential_fingerprint(&outer_allowed_asset, &credential)
        .is_err());

    let original = MppWire::official_challenge(&challenge).unwrap();
    let original_request: ChargeRequest = original.request.decode().unwrap();

    let mut inner_other_asset = challenge.clone();
    let mut request = original_request.clone();
    request.currency = "0x3333333333333333333333333333333333333333".to_owned();
    request.method_details = Some(json!({"chainId": "1"}));
    replace_charge_request(&adapter, &mut inner_other_asset, &request);
    assert_integrity_rejected(&adapter, &inner_other_asset).await;

    let mut inner_other_amount = challenge.clone();
    let mut request = original_request.clone();
    request.amount = "2000000".to_owned();
    replace_charge_request(&adapter, &mut inner_other_amount, &request);
    assert_integrity_rejected(&adapter, &inner_other_amount).await;

    let mut inner_other_resource = challenge.clone();
    let mut request = original_request.clone();
    request.mppx_scope = Some(json!({"resource": "https://merchant.example/other"}));
    replace_charge_request(&adapter, &mut inner_other_resource, &request);
    assert_integrity_rejected(&adapter, &inner_other_resource).await;

    let mut missing_resource_scope = challenge.clone();
    let mut request = original_request;
    request.mppx_scope = None;
    replace_charge_request(&adapter, &mut missing_resource_scope, &request);
    assert_integrity_rejected(&adapter, &missing_resource_scope).await;

    let mut split_recipient = challenge.clone();
    let mut request = MppWire::official_challenge(&challenge)
        .unwrap()
        .request
        .decode::<ChargeRequest>()
        .unwrap();
    request.method_details = Some(json!({
        "chainId": "42431",
        "splits": [{
            "recipient": "0x4444444444444444444444444444444444444444",
            "amount": "999999"
        }]
    }));
    replace_charge_request(&adapter, &mut split_recipient, &request);
    assert_integrity_rejected(&adapter, &split_recipient).await;
    assert!(adapter
        .parse_challenge(
            "scenario-1",
            &split_recipient.resource,
            &split_recipient.payload,
            2_000_000_000,
            None,
        )
        .await
        .is_err());

    let mut forged_expiry = challenge.clone();
    forged_expiry.expires_at += 1;
    assert_integrity_rejected(&adapter, &forged_expiry).await;

    let mut forged_nonce = challenge.clone();
    forged_nonce.nonce = "other-id".to_owned();
    assert_integrity_rejected(&adapter, &forged_nonce).await;

    let mut forged_hash = challenge;
    forged_hash.challenge_hash = "00".repeat(32);
    assert_integrity_rejected(&adapter, &forged_hash).await;
}

#[tokio::test]
async fn replay_fingerprint_normalizes_official_payment_reference() {
    let adapter = adapter();
    let challenge = adapter
        .build_challenge(&price(), "0x2222222222222222222222222222222222222222", 2_000_000_000)
        .await
        .unwrap();
    let official = MppWire::official_challenge(&challenge).unwrap();
    let upper = mpp::PaymentCredential::new(
        official.to_echo(),
        mpp::PaymentPayload::hash(format!("0x{}", "AB".repeat(32))),
    );
    let lower = mpp::PaymentCredential::new(
        official.to_echo(),
        mpp::PaymentPayload::hash(format!("0x{}", "ab".repeat(32))),
    );
    let to_domain = |credential: &mpp::PaymentCredential| PaymentCredential {
        protocol: MPP_PROTOCOL.to_owned(),
        challenge_hash: challenge.challenge_hash.clone(),
        credential_hash: "untrusted".to_owned(),
        payload: json!({
            "authorization": format_authorization(credential).unwrap(),
        }),
    };

    assert_eq!(
        adapter.credential_fingerprint(&challenge, &to_domain(&upper)).unwrap(),
        adapter.credential_fingerprint(&challenge, &to_domain(&lower)).unwrap()
    );
}

mod tempo {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mpp/tempo_test.rs"));
}
