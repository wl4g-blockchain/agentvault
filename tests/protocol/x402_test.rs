use super::*;

fn adapter() -> X402Adapter {
    X402Adapter::new(
        vec![X402Asset {
            network: "eip155:8453".to_owned(),
            contract: "0x1111111111111111111111111111111111111111".to_owned(),
            symbol: "USDC".to_owned(),
            decimals: 6,
        }],
        60,
    )
    .unwrap()
}

fn adapter_with_two_assets() -> X402Adapter {
    X402Adapter::new(
        vec![
            X402Asset {
                network: "eip155:8453".to_owned(),
                contract: "0x1111111111111111111111111111111111111111".to_owned(),
                symbol: "USDC".to_owned(),
                decimals: 6,
            },
            X402Asset {
                network: "eip155:1".to_owned(),
                contract: "0x3333333333333333333333333333333333333333".to_owned(),
                symbol: "USDC".to_owned(),
                decimals: 6,
            },
        ],
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

fn rehash(challenge: &mut PaymentChallenge) {
    let hash = x402_challenge_hash(
        &challenge.resource,
        challenge.expires_at,
        &challenge.payload["payment_required"],
    )
    .unwrap();
    challenge.nonce = hash.clone();
    challenge.challenge_hash = hash;
}

#[test]
fn challenge_hash_is_stable_across_object_key_order() {
    let first: Value = serde_json::from_str(
        r#"{"x402Version":2,"resource":{"url":"https://merchant.example/paid","description":"feed"},"accepts":[{"scheme":"exact","network":"eip155:8453","amount":"1000000","asset":"0x1111111111111111111111111111111111111111","payTo":"0x2222222222222222222222222222222222222222","maxTimeoutSeconds":60}]}"#,
    )
    .unwrap();
    let reordered: Value = serde_json::from_str(
        r#"{"accepts":[{"maxTimeoutSeconds":60,"payTo":"0x2222222222222222222222222222222222222222","asset":"0x1111111111111111111111111111111111111111","amount":"1000000","network":"eip155:8453","scheme":"exact"}],"resource":{"description":"feed","url":"https://merchant.example/paid"},"x402Version":2}"#,
    )
    .unwrap();
    let resource = "https://merchant.example/paid";
    let first_hash = x402_challenge_hash(resource, 160, &first).unwrap();
    assert_eq!(first_hash, x402_challenge_hash(resource, 160, &reordered).unwrap());

    let mut tampered = reordered;
    tampered["accepts"][0]["amount"] = Value::String("1000001".to_owned());
    assert_ne!(first_hash, x402_challenge_hash(resource, 160, &tampered).unwrap());
}

fn signed_payload(challenge: &PaymentChallenge) -> Value {
    json!({
        "x402Version": 2,
        "resource": challenge.payload["payment_required"]["resource"].clone(),
        "accepted": challenge.payload["selected"].clone(),
        "payload": {
            "signature": format!("0x{}", "ab".repeat(65)),
            "authorization": {
                "from": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "to": challenge.recipient.clone(),
                "value": challenge.amount.to_string(),
                "validAfter": "0",
                "validBefore": challenge.expires_at.to_string(),
                "nonce": format!("0x{}", "42".repeat(32)),
            }
        }
    })
}

fn matching_intent(challenge: &PaymentChallenge) -> PaymentIntent {
    PaymentIntent {
        id: "intent-1".to_owned(),
        scenario_id: challenge.scenario_id.clone(),
        run_id: "run-1".to_owned(),
        wallet_id: Some("wallet-1".to_owned()),
        policy_id: Some("policy-1".to_owned()),
        offer_id: None,
        direction: crate::model::PaymentDirection::Outbound,
        protocol: X402_PROTOCOL.to_owned(),
        asset: challenge.asset.clone(),
        recipient: challenge.recipient.clone(),
        amount: challenge.amount,
        settled_amount: Amount::ZERO,
        resource: challenge.resource.clone(),
        challenge_hash: Some(challenge.challenge_hash.clone()),
        idempotency_key: "idempotency-1".to_owned(),
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

async fn assert_integrity_rejected(adapter: &X402Adapter, challenge: &PaymentChallenge) {
    assert!(adapter
        .validate_payment(challenge, &matching_intent(challenge))
        .await
        .is_err());
    let credential = PaymentCredential {
        protocol: X402_PROTOCOL.to_owned(),
        challenge_hash: challenge.challenge_hash.clone(),
        credential_hash: "untrusted".to_owned(),
        payload: json!({}),
    };
    assert!(adapter.settle_payment(challenge, &credential).await.is_err());
}

fn offer() -> PaymentOffer {
    PaymentOffer {
        id: "offer-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        policy_id: "policy-1".to_owned(),
        resource: "https://merchant.example/paid".to_owned(),
        protocols: vec![X402_PROTOCOL.to_owned()],
        asset: AssetIdentity {
            network: "eip155:8453".to_owned(),
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
async fn challenge_binds_server_owned_offer_fields() {
    let recipient = "0x2222222222222222222222222222222222222222";
    let challenge = adapter().build_challenge(&offer(), recipient, 100).await.unwrap();
    assert_eq!(challenge.recipient, recipient);
    assert_eq!(challenge.amount, Amount::new(1_000_000));
    assert_eq!(challenge.offer_id.as_deref(), Some("offer-1"));

    let parsed = adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
            &challenge.payload["payment_required"],
            100,
            Some(challenge.expires_at),
        )
        .await
        .unwrap();
    assert_eq!(parsed.recipient, recipient);
    assert_eq!(parsed.challenge_hash, challenge.challenge_hash);

    let replay = adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
            &challenge.payload["payment_required"],
            101,
            Some(challenge.expires_at),
        )
        .await
        .unwrap();
    assert_eq!(replay.challenge_hash, parsed.challenge_hash);
    assert_eq!(replay.expires_at, parsed.expires_at);
    assert!(adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
            &challenge.payload["payment_required"],
            101,
            None,
        )
        .await
        .is_err());

    let second = adapter().build_challenge(&offer(), recipient, 100).await.unwrap();
    assert_eq!(second.challenge_hash, challenge.challenge_hash);

    let mut tampered = challenge.payload["payment_required"].clone();
    tampered["accepts"][0]["payTo"] = Value::String("0x3333333333333333333333333333333333333333".to_owned());
    let tampered = adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
            &tampered,
            100,
            Some(challenge.expires_at),
        )
        .await
        .unwrap();
    assert_ne!(tampered.challenge_hash, challenge.challenge_hash);
}

#[tokio::test]
async fn official_client_nonce_is_accepted_and_globally_fingerprinted() {
    let adapter = adapter();
    let recipient = "0x2222222222222222222222222222222222222222";
    let challenge = adapter.build_challenge(&offer(), recipient, 100).await.unwrap();
    assert!(challenge.payload["payment_required"].get("extensions").is_none());

    let first = adapter
        .build_credential(&challenge, signed_payload(&challenge))
        .await
        .unwrap();
    let mut another_payload = signed_payload(&challenge);
    another_payload["payload"]["authorization"]["nonce"] = Value::String(format!("0x{}", "43".repeat(32)));
    let second = adapter.build_credential(&challenge, another_payload).await.unwrap();
    assert_ne!(
        adapter.credential_fingerprint(&challenge, &first).unwrap(),
        adapter.credential_fingerprint(&challenge, &second).unwrap()
    );

    let mut unbound = first;
    unbound.challenge_hash = "other-challenge".to_owned();
    assert!(adapter.verify_credential(&challenge, &unbound, 101).await.is_err());
}

#[tokio::test]
async fn adapter_builds_and_verifies_an_intent_bound_eoa_signature() {
    let adapter = adapter();
    let challenge = adapter
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", 100)
        .await
        .unwrap();
    let key = crate::utils::crypto::generate_private_key();
    let payer = crate::utils::crypto::address(&key).unwrap();
    let signing = adapter.prepare_signing(&challenge, &payer, 101).await.unwrap();
    assert_eq!(signing.digest.len(), 32);
    assert_eq!(
        signing.payload["payload"]["authorization"]["validAfter"],
        Value::String("0".to_owned())
    );
    assert_eq!(
        signing.payload["payload"]["authorization"]["nonce"]
            .as_str()
            .unwrap()
            .len(),
        66
    );
    let signature = crate::utils::crypto::sign_eip712_digest(&key, &signing.digest).unwrap();
    let credential = adapter.complete_signing(&challenge, signing, &signature).await.unwrap();
    assert_eq!(
        credential.credential_hash,
        adapter.credential_fingerprint(&challenge, &credential).unwrap()
    );
    assert_eq!(verify_eip3009_signature(&credential.payload).unwrap(), payer);
    assert!(adapter.verify_credential(&challenge, &credential, 102).await.is_ok());
}

#[tokio::test]
async fn challenge_canonicalizes_checksum_recipient() {
    let mixed = "0xAbCdEf0123456789aBCDef0123456789AbCDef01";
    let canonical = "0xabcdef0123456789abcdef0123456789abcdef01";
    let challenge = adapter().build_challenge(&offer(), mixed, 100).await.unwrap();
    assert_eq!(challenge.recipient, canonical);
    let wire_recipient = challenge.payload["selected"]["payTo"].as_str().unwrap();
    assert_eq!(
        canonicalize_wallet_address(EVM_EOA_ALGORITHM, wire_recipient).unwrap(),
        canonical
    );

    let parsed = adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
            &json!({
                "x402Version": 2,
                "resource": {"url": offer().resource},
                "accepts": [{
                    "scheme": "exact",
                    "network": "eip155:8453",
                    "amount": "1000000",
                    "asset": "0x1111111111111111111111111111111111111111",
                    "payTo": mixed,
                    "maxTimeoutSeconds": 60,
                    "extra": {"name": "USDC", "version": "2"}
                }]
            }),
            100,
            Some(160),
        )
        .await
        .unwrap();
    assert_eq!(parsed.recipient, canonical);
}

#[tokio::test]
async fn every_entrypoint_rejects_forged_outer_or_stored_wire() {
    let adapter = adapter_with_two_assets();
    let challenge = adapter
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", 100)
        .await
        .unwrap();

    let mut outer_allowed_asset = challenge.clone();
    outer_allowed_asset.asset = second_asset();
    assert_integrity_rejected(&adapter, &outer_allowed_asset).await;
    let payload = signed_payload(&outer_allowed_asset);
    assert!(adapter
        .build_credential(&outer_allowed_asset, payload.clone())
        .await
        .is_err());
    let credential = PaymentCredential {
        protocol: X402_PROTOCOL.to_owned(),
        challenge_hash: outer_allowed_asset.challenge_hash.clone(),
        credential_hash: "untrusted".to_owned(),
        payload,
    };
    assert!(adapter
        .verify_credential(&outer_allowed_asset, &credential, outer_allowed_asset.expires_at - 1,)
        .await
        .is_err());
    assert!(adapter
        .credential_fingerprint(&outer_allowed_asset, &credential)
        .is_err());

    let mut inner_other_asset = challenge.clone();
    inner_other_asset.payload["payment_required"]["accepts"][0]["network"] = Value::String("eip155:1".to_owned());
    inner_other_asset.payload["payment_required"]["accepts"][0]["asset"] =
        Value::String("0x3333333333333333333333333333333333333333".to_owned());
    let selected = inner_other_asset.payload["payment_required"]["accepts"][0].clone();
    inner_other_asset.payload["selected"] = selected;
    rehash(&mut inner_other_asset);
    assert_integrity_rejected(&adapter, &inner_other_asset).await;

    let mut inner_other_amount = challenge.clone();
    inner_other_amount.payload["payment_required"]["accepts"][0]["amount"] = Value::String("2000000".to_owned());
    let selected = inner_other_amount.payload["payment_required"]["accepts"][0].clone();
    inner_other_amount.payload["selected"] = selected;
    rehash(&mut inner_other_amount);
    assert_integrity_rejected(&adapter, &inner_other_amount).await;

    let mut inner_other_resource = challenge.clone();
    inner_other_resource.payload["payment_required"]["resource"]["url"] =
        Value::String("https://merchant.example/other".to_owned());
    rehash(&mut inner_other_resource);
    assert_integrity_rejected(&adapter, &inner_other_resource).await;

    let mut unadvertised_selection = challenge.clone();
    unadvertised_selection.payload["selected"]["amount"] = Value::String("2000000".to_owned());
    assert_integrity_rejected(&adapter, &unadvertised_selection).await;

    let mut forged_nonce = challenge.clone();
    forged_nonce.nonce = "other-id".to_owned();
    assert_integrity_rejected(&adapter, &forged_nonce).await;

    let mut forged_hash = challenge;
    forged_hash.challenge_hash = "00".repeat(32);
    assert_integrity_rejected(&adapter, &forged_hash).await;
}

#[tokio::test]
async fn replay_fingerprint_uses_eip3009_identity_not_json_spelling() {
    let adapter = adapter();
    let challenge = adapter
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", 100)
        .await
        .unwrap();
    let nonce = format!("0x{}", "42".repeat(32));
    let payload = json!({
        "x402Version": 2,
        "resource": challenge.payload["payment_required"]["resource"].clone(),
        "accepted": challenge.payload["selected"].clone(),
        "payload": {
            "signature": format!("0x{}", "ab".repeat(65)),
            "authorization": {
                "from": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "to": challenge.recipient.clone(),
                "value": challenge.amount.to_string(),
                "validAfter": "99",
                "validBefore": "150",
                "nonce": nonce,
            }
        },
    });
    let first = PaymentCredential {
        protocol: X402_PROTOCOL.to_owned(),
        challenge_hash: challenge.challenge_hash.clone(),
        credential_hash: "untrusted-one".to_owned(),
        payload,
    };
    let mut equivalent = first.clone();
    equivalent.credential_hash = "untrusted-two".to_owned();
    equivalent.payload["payload"]["signature"] = Value::String(format!("0x{}", "AB".repeat(65)));
    equivalent.payload["payload"]["authorization"]["from"] =
        Value::String("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned());
    equivalent.payload["payload"]["authorization"]["nonce"] =
        Value::String(format!("0x{}", nonce.trim_start_matches("0x").to_ascii_uppercase()));

    assert_eq!(
        adapter.credential_fingerprint(&challenge, &first).unwrap(),
        adapter.credential_fingerprint(&challenge, &equivalent).unwrap()
    );

    let mut unknown = first;
    unknown.payload["payload"]["unexpected"] = Value::Bool(true);
    assert!(adapter.credential_fingerprint(&challenge, &unknown).is_err());
}
