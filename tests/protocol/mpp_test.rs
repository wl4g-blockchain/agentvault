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
        offer_id: None,
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

fn offer() -> PaymentPrice {
    PaymentPrice {
        id: "offer-1".to_owned(),
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
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let parsed = adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
            &challenge.payload,
            now,
            Some(challenge.expires_at),
        )
        .await
        .unwrap();
    assert_eq!(parsed.amount, offer().amount);
    assert_eq!(parsed.asset, offer().asset);
    assert_eq!(parsed.nonce, challenge.nonce);
    assert_eq!(parsed.challenge_hash, challenge.challenge_hash);
    assert_eq!(
        MppWire::official_challenge(&challenge).unwrap().header.as_deref(),
        Some("Payment-Authorization")
    );

    let second = adapter()
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    assert_ne!(second.challenge_hash, challenge.challenge_hash);
}

#[tokio::test]
async fn http_provider_preserves_bearer_with_the_advertised_credential_header() {
    let now = 2_000_000_000;
    let challenge = adapter()
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", now)
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
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let payer = payer_adapter();
    assert!(payer.build_challenge(&offer(), "recipient", now).await.is_err());

    let parsed = payer
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
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
    let resource = offer().resource;
    let issued = adapter()
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", now)
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
    let key = crate::utils::crypto::WalletCrypto::generate_private_key();
    let payer = crate::utils::crypto::WalletCrypto::address(&key).unwrap();
    let signing = payer_adapter
        .prepare_signing(&challenge, &payer, now + 1)
        .await
        .unwrap();
    assert_eq!(signing.digest.len(), 32);
    assert_eq!(signing.payload["kind"], TEMPO_SIGNING_PAYLOAD_KIND);
    assert_eq!(signing.payload["payer"], payer);

    let signature = crate::utils::crypto::WalletCrypto::sign_eip712_digest(&key, &signing.digest).unwrap();
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

    let other_key = crate::utils::crypto::WalletCrypto::generate_private_key();
    let wrong_signature = crate::utils::crypto::WalletCrypto::sign_eip712_digest(&other_key, &signing.digest).unwrap();
    assert!(payer_adapter
        .complete_signing(&challenge, signing.clone(), &wrong_signature)
        .await
        .is_err());

    let mut modified = signing;
    modified.payload["transaction"]["gas"] = json!(999_999_u64);
    let modified_signature = crate::utils::crypto::WalletCrypto::sign_eip712_digest(&key, &modified.digest).unwrap();
    assert!(payer_adapter
        .complete_signing(&challenge, modified, &modified_signature)
        .await
        .is_err());
}

#[tokio::test]
async fn payer_selects_allowed_payment_challenge_and_ignores_bearer() {
    let now = 2_000_000_000;
    let issued = adapter()
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", now)
        .await
        .unwrap();
    let payment = issued.payload["www_authenticate"].as_str().unwrap();
    let header = format!("Bearer realm=\"login\", {payment}");
    let parsed = payer_adapter()
        .parse_challenge(
            "payer-scenario",
            &offer().resource,
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
            &offer().resource,
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
        .build_challenge(&offer(), recipient, 2_000_000_000)
        .await
        .unwrap();
    let second = adapter
        .build_challenge(&offer(), recipient, 2_000_000_000)
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
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", 2_000_000_000)
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
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", 2_000_000_000)
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

const LIVE_TEMPO_RPC_URL: &str = "http://127.0.0.1:28546";
const LIVE_TEMPO_CHAIN_ID: u64 = 1_337;
const LIVE_TEMPO_PATH_USD: &str = "0x20c0000000000000000000000000000000000000";
const LIVE_TEMPO_DEV_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LiveTempoSettlementRequest {
    protocol: String,
    run_id: String,
    payment_payload: Value,
    payment_requirements: Value,
}

#[derive(Clone)]
struct LiveTempoFacilitator {
    method: mpp::server::TempoChargeMethod<mpp::server::TempoProvider>,
    provider: mpp::server::TempoProvider,
    asset: AssetIdentity,
    drop_settle_response: std::sync::Arc<std::sync::atomic::AtomicBool>,
    broadcasts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

struct ValidatedLiveCharge {
    credential: mpp::PaymentCredential,
    charge: mpp::ChargeRequest,
    transaction_hash: B256,
    payer: String,
    amount: Amount,
    recipient: String,
}

impl LiveTempoFacilitator {
    async fn execute(
        &self,
        request: &LiveTempoSettlementRequest,
        settle: bool,
    ) -> crate::model::SettlementProviderResult {
        match self.execute_checked(request, settle).await {
            Ok(result) => result,
            Err(reason) => crate::model::SettlementProviderResult {
                valid: false,
                settled: false,
                payer: None,
                transaction: None,
                settlement_ref: None,
                effect_ref: None,
                invalid_reason: Some(reason),
                asset: None,
                amount: None,
                recipient: None,
                confirmations: None,
                extensions: BTreeMap::new(),
            },
        }
    }

    async fn execute_checked(
        &self,
        request: &LiveTempoSettlementRequest,
        settle: bool,
    ) -> std::result::Result<crate::model::SettlementProviderResult, String> {
        use alloy::{network::ReceiptResponse as _, providers::Provider as _};
        use mpp::ChargeMethod as _;

        let validated = self.validate_charge(request).await?;
        let expected_hash = validated.transaction_hash;
        let mut receipt = self
            .provider
            .get_transaction_receipt(expected_hash)
            .await
            .map_err(|error| format!("query Tempo receipt: {error}"))?;

        if settle && receipt.is_none() {
            let protocol_receipt = self
                .method
                .broadcast(&validated.credential, &validated.charge)
                .await
                .map_err(|error| error.to_string())?;
            let broadcast_hash = protocol_receipt
                .reference
                .parse::<B256>()
                .map_err(|error| format!("invalid Tempo receipt hash: {error}"))?;
            if broadcast_hash != expected_hash {
                return Err("facilitator broadcast returned a different transaction".to_owned());
            }
            self.broadcasts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            for _ in 0..40 {
                receipt = self
                    .provider
                    .get_transaction_receipt(expected_hash)
                    .await
                    .map_err(|error| format!("query broadcast Tempo receipt: {error}"))?;
                if receipt.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        let mut extensions = BTreeMap::new();
        let (settled, confirmations) = if let Some(receipt) = receipt {
            if !receipt.status() || format!("{:#x}", receipt.from()) != validated.payer {
                return Err("Tempo receipt status or payer is invalid".to_owned());
            }
            let (gas_limit, fee) = live_tempo_gas_evidence(
                &self.provider,
                expected_hash,
                receipt.gas_used(),
                receipt.effective_gas_price(),
            )
            .await?;
            let gas_cost_attodollars = U256::from(receipt.gas_used()) * U256::from(receipt.effective_gas_price());
            extensions.insert("gas_limit".to_owned(), json!(gas_limit));
            extensions.insert("gas_used".to_owned(), json!(receipt.gas_used()));
            extensions.insert(
                "effective_gas_price".to_owned(),
                json!(receipt.effective_gas_price().to_string()),
            );
            extensions.insert("gas_fee".to_owned(), json!(fee.to_string()));
            extensions.insert(
                "gas_cost_attodollars".to_owned(),
                json!(gas_cost_attodollars.to_string()),
            );
            extensions.insert("fee_payer".to_owned(), json!(format!("{:#x}", receipt.fee_payer)));
            extensions.insert(
                "fee_token".to_owned(),
                json!(receipt.fee_token.map(|token| format!("{token:#x}"))),
            );
            let current_block = self
                .provider
                .get_block_number()
                .await
                .map_err(|error| format!("query Tempo block number: {error}"))?;
            let receipt_block = receipt
                .block_number()
                .ok_or_else(|| "confirmed Tempo receipt omitted its block".to_owned())?;
            (
                true,
                Some(current_block.saturating_sub(receipt_block).saturating_add(1)),
            )
        } else {
            (false, None)
        };

        Ok(crate::model::SettlementProviderResult {
            valid: true,
            settled,
            payer: Some(validated.payer),
            transaction: settled.then(|| format!("{expected_hash:#x}")),
            settlement_ref: None,
            effect_ref: settled.then(|| "tempo:tip20-transfer:0".to_owned()),
            invalid_reason: None,
            asset: Some(self.asset.clone()),
            amount: Some(validated.amount),
            recipient: Some(validated.recipient),
            confirmations,
            extensions,
        })
    }

    async fn validate_charge(
        &self,
        request: &LiveTempoSettlementRequest,
    ) -> std::result::Result<ValidatedLiveCharge, String> {
        use mpp::ChargeMethod as _;

        if request.protocol != MPP_PROTOCOL || request.run_id.trim().is_empty() {
            return Err("facilitator request identity is invalid".to_owned());
        }
        let challenge_header = request
            .payment_requirements
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| "facilitator request omitted the MPP challenge".to_owned())?;
        let challenge = mpp::parse_www_authenticate(challenge_header).map_err(|error| error.to_string())?;
        let authorization = request
            .payment_payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| "facilitator request omitted the MPP credential".to_owned())?;
        let credential = mpp::parse_authorization(authorization).map_err(|error| error.to_string())?;
        if serde_json::to_value(&credential.challenge).map_err(|error| error.to_string())?
            != serde_json::to_value(challenge.to_echo()).map_err(|error| error.to_string())?
        {
            return Err("facilitator challenge echo mismatch".to_owned());
        }
        let charge: mpp::ChargeRequest = challenge.request.decode().map_err(|error| error.to_string())?;
        let validation = self
            .method
            .validate(&credential, &charge)
            .await
            .map_err(|error| error.to_string())?;
        let payload = credential.charge_payload().map_err(|error| error.to_string())?;
        let transaction = payload
            .signed_tx()
            .ok_or_else(|| "facilitator accepts only Tempo transaction credentials".to_owned())?;
        let transaction_bytes = transaction
            .parse::<alloy_primitives::Bytes>()
            .map_err(|error| format!("invalid Tempo transaction bytes: {error}"))?;
        let payer = validation
            .source
            .as_deref()
            .and_then(|source| source.rsplit(':').next())
            .ok_or_else(|| "validated Tempo credential omitted the payer".to_owned())?
            .to_ascii_lowercase();
        let amount = charge
            .amount
            .parse::<u128>()
            .map(Amount::new)
            .map_err(|error| format!("invalid validated Tempo amount: {error}"))?;
        let recipient = charge
            .recipient
            .as_deref()
            .ok_or_else(|| "validated Tempo charge omitted the recipient".to_owned())?
            .to_ascii_lowercase();
        Ok(ValidatedLiveCharge {
            credential,
            charge,
            transaction_hash: alloy_primitives::keccak256(&transaction_bytes),
            payer,
            amount,
            recipient,
        })
    }
}

async fn live_tempo_verify(
    axum::extract::State(facilitator): axum::extract::State<LiveTempoFacilitator>,
    axum::Json(request): axum::Json<LiveTempoSettlementRequest>,
) -> axum::Json<crate::model::SettlementProviderResult> {
    let result = facilitator.execute(&request, false).await;
    println!(
        "{}",
        json!({"event": "agentvault.e2e.mpp.verify", "run_id": request.run_id,
            "valid": result.valid, "settled": result.settled,
            "invalid_reason": result.invalid_reason})
    );
    axum::Json(result)
}

async fn live_tempo_settle(
    axum::extract::State(facilitator): axum::extract::State<LiveTempoFacilitator>,
    axum::Json(request): axum::Json<LiveTempoSettlementRequest>,
) -> (
    axum::http::StatusCode,
    axum::Json<crate::model::SettlementProviderResult>,
) {
    let result = facilitator.execute(&request, true).await;
    println!(
        "{}",
        json!({"event": "agentvault.e2e.mpp.settle", "run_id": request.run_id,
            "valid": result.valid, "settled": result.settled,
            "transaction": result.transaction})
    );
    if result.settled
        && facilitator
            .drop_settle_response
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(result));
    }
    (axum::http::StatusCode::OK, axum::Json(result))
}

async fn live_tempo_balance(provider: &mpp::server::TempoProvider, token: Address, account: Address) -> U256 {
    use alloy::providers::Provider as _;

    let input = ITIP20::balanceOfCall::new((account,)).abi_encode();
    let result = provider
        .call(
            alloy::rpc::types::TransactionRequest::default()
                .to(token)
                .input(alloy::rpc::types::TransactionInput::new(Bytes::from(input)))
                .into(),
        )
        .await
        .expect("Tempo balanceOf call failed");
    U256::from_be_slice(&result)
}

async fn fund_live_tempo_payer(provider: &mpp::server::TempoProvider, token: Address, payer: Address) {
    use alloy::{network::ReceiptResponse as _, providers::Provider as _};

    let dev_key = hex::decode(LIVE_TEMPO_DEV_PRIVATE_KEY).expect("dev key hex");
    let dev = crate::utils::crypto::WalletCrypto::address(&dev_key)
        .expect("derive Tempo dev account")
        .parse::<Address>()
        .expect("Tempo dev account address");
    let chain_id = provider.get_chain_id().await.expect("query Tempo chain ID");
    let nonce = provider
        .get_transaction_count(dev)
        .await
        .expect("query Tempo dev nonce");
    let gas_price = provider.get_gas_price().await.expect("query Tempo gas price");
    let funding_amount = U256::from(10_000_000_000_u64);
    let transaction = TempoTransaction {
        chain_id,
        nonce,
        gas_limit: 5_000_000,
        max_fee_per_gas: gas_price,
        max_priority_fee_per_gas: gas_price,
        calls: vec![Call {
            to: TxKind::Call(token),
            value: U256::ZERO,
            input: Bytes::from(ITIP20::transferCall::new((payer, funding_amount)).abi_encode()),
        }],
        ..TempoTransaction::default()
    };
    let signature =
        crate::utils::crypto::WalletCrypto::sign_eip712_digest(&dev_key, transaction.signature_hash().as_slice())
            .expect("sign Tempo payer funding transaction");
    let signature = Signature::from_raw(&signature).expect("decode Tempo payer funding signature");
    let raw = format!(
        "0x{}",
        hex::encode(transaction.into_signed(signature.into()).encoded_2718())
    );
    let transaction_hash: B256 = provider
        .raw_request("eth_sendRawTransaction".into(), (raw,))
        .await
        .expect("broadcast Tempo payer funding transaction");
    for _ in 0..40 {
        if let Some(receipt) = provider
            .get_transaction_receipt(transaction_hash)
            .await
            .expect("query Tempo payer funding receipt")
        {
            assert!(receipt.status(), "Tempo payer funding reverted");
            assert_eq!(live_tempo_balance(provider, token, payer).await, funding_amount);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("Tempo payer funding was not confirmed");
}

struct LiveTempoAccounts {
    token: Address,
    payer: Address,
    recipient: Address,
}

struct PreparedLiveTempoPayment {
    accounts: LiveTempoAccounts,
    recipient: String,
    asset: AssetIdentity,
    amount: Amount,
    payer_protocol: MppAdapter,
    settlement: ProtocolSettlement,
}

async fn prepare_live_tempo_payment(provider: &mpp::server::TempoProvider) -> PreparedLiveTempoPayment {
    let token = LIVE_TEMPO_PATH_USD.parse::<Address>().expect("pathUSD address");
    let payer_key = crate::utils::crypto::WalletCrypto::generate_private_key();
    let payer = crate::utils::crypto::WalletCrypto::address(&payer_key).expect("derive isolated payer");
    let payer_address = payer.parse::<Address>().expect("payer address");
    fund_live_tempo_payer(provider, token, payer_address).await;
    let recipient_key = crate::utils::crypto::WalletCrypto::generate_private_key();
    let recipient = crate::utils::crypto::WalletCrypto::address(&recipient_key).expect("derive recipient");
    let recipient_address = recipient.parse::<Address>().expect("recipient address");
    let asset = AssetIdentity {
        network: format!("eip155:{LIVE_TEMPO_CHAIN_ID}"),
        asset: "pathUSD".to_owned(),
        token_contract: Some(LIVE_TEMPO_PATH_USD.to_owned()),
        decimals: 6,
    };
    let configured_asset = MppAsset {
        network: asset.network.clone(),
        currency: LIVE_TEMPO_PATH_USD.to_owned(),
        symbol: asset.asset.clone(),
        token_contract: asset.token_contract.clone(),
        decimals: asset.decimals,
    };
    let issuer_adapter = MppAdapter::new_payee(
        vec![configured_asset.clone()],
        "marketpulse.local",
        "tempo-e2e-challenge-secret-32-bytes",
        60,
    )
    .expect("create MPP payee adapter");
    let payer_protocol = MppAdapter::new_payer(vec![configured_asset], 60).expect("create MPP payer adapter");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let offer = PaymentPrice {
        id: "offer-mpp-tempo-live".to_owned(),
        scenario_id: "scenario-mpp-tempo-live".to_owned(),
        policy_id: "policy-mpp-tempo-live".to_owned(),
        resource: "https://marketpulse.local/mcp/realtime".to_owned(),
        protocols: vec![MPP_PROTOCOL.to_owned()],
        asset: asset.clone(),
        amount: Amount::new(1_000_000),
        wallet_id: "wallet-mpp-tempo-payee".to_owned(),
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    let issued = issuer_adapter
        .build_challenge(&offer, &recipient, now)
        .await
        .expect("issue MPP charge challenge");
    let challenge = payer_protocol
        .parse_challenge(
            "scenario-mpp-tempo-live",
            &offer.resource,
            &issued.payload,
            now,
            Some(issued.expires_at),
        )
        .await
        .expect("payer parses MPP response header");
    let signing = payer_protocol
        .prepare_signing(&challenge, &payer, now + 1)
        .await
        .expect("Vault prepares intent-bound Tempo signing plan");
    let signature = crate::utils::crypto::WalletCrypto::sign_eip712_digest(&payer_key, &signing.digest)
        .expect("Vault signs the Tempo transaction digest");
    let credential = payer_protocol
        .complete_signing(&challenge, signing, &signature)
        .await
        .expect("Vault creates the official MPP credential");
    let validation = issuer_adapter
        .verify_credential(&issued, &credential, now + 2)
        .await
        .expect("payee validates the signed credential binding");
    assert_eq!(validation.payer.as_deref(), Some(payer.as_str()));
    assert!(validation.requires_external_settlement);
    let settlement = issuer_adapter
        .settle_payment(&issued, &credential)
        .await
        .expect("payee maps the credential to facilitator DTOs");
    PreparedLiveTempoPayment {
        accounts: LiveTempoAccounts {
            token,
            payer: payer_address,
            recipient: recipient_address,
        },
        recipient,
        asset,
        amount: offer.amount,
        payer_protocol,
        settlement,
    }
}

async fn start_live_tempo_facilitator(
    provider: &mpp::server::TempoProvider,
    asset: &AssetIdentity,
) -> (
    crate::payment::HttpSettlementProvider,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use axum::routing::post;

    let broadcasts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let facilitator = LiveTempoFacilitator {
        method: mpp::server::TempoChargeMethod::new(provider.clone())
            .with_store(std::sync::Arc::new(mpp::MemoryStore::new())),
        provider: provider.clone(),
        asset: asset.clone(),
        drop_settle_response: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        broadcasts: broadcasts.clone(),
    };
    let app = axum::Router::new()
        .route("/verify", post(live_tempo_verify))
        .route("/settle", post(live_tempo_settle))
        .with_state(facilitator);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind MPP facilitator");
    let base_url = format!("http://{}", listener.local_addr().expect("facilitator address"));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("facilitator server") });
    let external = crate::payment::HttpSettlementProvider::new(
        &crate::payment::SettlementProviderConfig {
            base_url,
            allow_insecure_loopback: true,
            ..crate::payment::SettlementProviderConfig::default()
        },
        reqwest::header::HeaderMap::new(),
    )
    .expect("create production HTTP settlement provider");
    (external, broadcasts, server)
}

async fn live_tempo_gas_evidence(
    provider: &mpp::server::TempoProvider,
    transaction_hash: B256,
    gas_used: u64,
    effective_gas_price: u128,
) -> std::result::Result<(u64, U256), String> {
    use alloy::{consensus::Transaction as _, providers::Provider as _};

    let transaction = provider
        .get_transaction_by_hash(transaction_hash)
        .await
        .map_err(|error| format!("query confirmed Tempo transaction: {error}"))?
        .ok_or_else(|| "confirmed Tempo transaction is missing".to_owned())?;
    let gas_limit = transaction.gas_limit();
    if gas_used > gas_limit {
        return Err("Tempo receipt gas usage exceeds the signed transaction cap".to_owned());
    }
    let fee = tempo_primitives::transaction::calc_gas_balance_spending(gas_used, effective_gas_price);
    Ok((gas_limit, fee))
}

async fn assert_live_tempo_accounting(
    provider: &mpp::server::TempoProvider,
    accounts: &LiveTempoAccounts,
    transaction: &str,
    amount: Amount,
    payer_before: U256,
    recipient_before: U256,
) -> (U256, U256, U256, u64, u64) {
    use alloy::{network::ReceiptResponse as _, providers::Provider as _};

    let transaction_hash = transaction.parse::<B256>().expect("transaction hash");
    let receipt = provider
        .get_transaction_receipt(transaction_hash)
        .await
        .expect("query confirmed receipt")
        .expect("confirmed Tempo receipt");
    assert!(receipt.status());
    assert_eq!(receipt.from(), accounts.payer);
    assert_eq!(receipt.fee_payer, accounts.payer);
    assert_eq!(receipt.fee_token, Some(accounts.token));
    let payer_after = live_tempo_balance(provider, accounts.token, accounts.payer).await;
    let recipient_after = live_tempo_balance(provider, accounts.token, accounts.recipient).await;
    let amount = U256::from(amount.value());
    let gas_used = receipt.gas_used();
    let (gas_limit, gas_fee) =
        live_tempo_gas_evidence(provider, transaction_hash, gas_used, receipt.effective_gas_price())
            .await
            .expect("derive Tempo fee-token debit from confirmed transaction");
    assert_eq!(recipient_after - recipient_before, amount);
    assert_eq!(payer_before - payer_after, amount + gas_fee);
    assert_eq!(payer_before + recipient_before, payer_after + recipient_after + gas_fee);
    (payer_after, recipient_after, gas_fee, gas_limit, gas_used)
}

struct LiveTempoEvidence {
    transaction: String,
    amount: Amount,
    gas_fee: U256,
    payer_before: U256,
    payer_after: U256,
    recipient_before: U256,
    recipient_after: U256,
    broadcasts: usize,
    gas_limit: u64,
    gas_used: u64,
}

impl LiveTempoEvidence {
    fn validate_and_emit(&self, payer_protocol: &MppAdapter, run_id: &str) {
        let receipt_header = mpp::Receipt::success(TEMPO_METHOD, self.transaction.clone())
            .to_header()
            .expect("MPP receipt header");
        let merchant_evidence = payer_protocol
            .decode_merchant_evidence(&receipt_header)
            .expect("payer decodes MPP merchant evidence");
        assert!(merchant_evidence.accepted);
        assert_eq!(
            merchant_evidence.transaction.as_deref(),
            Some(self.transaction.as_str())
        );
        println!(
            "MPP_TEMPO_EVIDENCE={}",
            json!({
                "run_id": run_id,
                "protocol": MPP_PROTOCOL,
                "method": TEMPO_METHOD,
                "chain_id": LIVE_TEMPO_CHAIN_ID,
                "transaction": self.transaction,
                "amount": self.amount.to_string(),
                "gas_fee": self.gas_fee.to_string(),
                "gas_limit": self.gas_limit,
                "gas_used": self.gas_used,
                "payer_before": self.payer_before.to_string(),
                "payer_after": self.payer_after.to_string(),
                "recipient_before": self.recipient_before.to_string(),
                "recipient_after": self.recipient_after.to_string(),
                "broadcasts": self.broadcasts,
                "recovered_after_lost_response": true,
            })
        );
    }
}

#[tokio::test]
#[ignore = "requires the pinned Tempo localnet E2E profile"]
async fn live_tempo_charge_survives_lost_settle_response_and_conserves_token_with_gas() {
    use crate::model::SettlementOperation;
    use crate::model::SettlementProviderRequest;
    use crate::payment::ExternalSettlementProvider;
    use alloy::providers::Provider as _;

    let rpc_url = std::env::var("TEMPO_RPC_URL").unwrap_or_else(|_| LIVE_TEMPO_RPC_URL.to_owned());
    let provider = mpp::server::tempo_provider(&rpc_url).expect("create Tempo provider");
    assert_eq!(
        provider.get_chain_id().await.expect("query Tempo chain ID"),
        LIVE_TEMPO_CHAIN_ID,
        "the E2E endpoint is not the pinned Tempo localnet"
    );
    let PreparedLiveTempoPayment {
        accounts,
        recipient,
        asset,
        amount,
        payer_protocol,
        settlement,
    } = prepare_live_tempo_payment(&provider).await;
    let (external, broadcasts, server) = start_live_tempo_facilitator(&provider, &asset).await;
    let request = SettlementProviderRequest {
        protocol: settlement.protocol,
        run_id: "run-mpp-tempo-live".to_owned(),
        payment_payload: settlement.payment_payload,
        payment_requirements: settlement.payment_requirements,
    };

    let payer_before = live_tempo_balance(&provider, accounts.token, accounts.payer).await;
    let recipient_before = live_tempo_balance(&provider, accounts.token, accounts.recipient).await;
    let verified = external
        .execute(SettlementOperation::Verify, &request)
        .await
        .expect("offline facilitator verify");
    assert!(
        verified.valid && !verified.settled,
        "offline facilitator verification failed: {:?}",
        verified.invalid_reason
    );
    assert_eq!(
        (
            live_tempo_balance(&provider, accounts.token, accounts.payer).await,
            live_tempo_balance(&provider, accounts.token, accounts.recipient).await,
        ),
        (payer_before, recipient_before)
    );

    let lost_response = external.execute(SettlementOperation::Settle, &request).await;
    assert!(lost_response.is_err(), "the E2E must simulate a lost settle response");
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);

    let recovered = external
        .execute(SettlementOperation::Verify, &request)
        .await
        .expect("recover settlement through read-only verify");
    assert!(recovered.valid && recovered.settled);
    assert_eq!(recovered.amount, Some(amount));
    assert_eq!(recovered.asset.as_ref(), Some(&asset));
    assert_eq!(recovered.recipient.as_deref(), Some(recipient.as_str()));
    let transaction = recovered.transaction.expect("confirmed transaction");
    let (payer_after, recipient_after, gas_fee, gas_limit, gas_used) = assert_live_tempo_accounting(
        &provider,
        &accounts,
        &transaction,
        amount,
        payer_before,
        recipient_before,
    )
    .await;

    let duplicate = external
        .execute(SettlementOperation::Settle, &request)
        .await
        .expect("idempotent settlement retry");
    assert_eq!(duplicate.transaction.as_deref(), Some(transaction.as_str()));
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        (
            live_tempo_balance(&provider, accounts.token, accounts.payer).await,
            live_tempo_balance(&provider, accounts.token, accounts.recipient).await,
        ),
        (payer_after, recipient_after)
    );

    LiveTempoEvidence {
        transaction,
        amount,
        gas_fee,
        payer_before,
        payer_after,
        recipient_before,
        recipient_after,
        broadcasts: broadcasts.load(std::sync::atomic::Ordering::SeqCst),
        gas_limit,
        gas_used,
    }
    .validate_and_emit(&payer_protocol, &request.run_id);
    server.abort();
    let _ = server.await;
}

#[derive(Clone)]
struct LiveTempoBalanceSource {
    provider: mpp::server::TempoProvider,
    token: Address,
}

#[async_trait::async_trait]
impl crate::payment::WalletBalanceSource for LiveTempoBalanceSource {
    async fn load(
        &self,
        wallet: &crate::model::Wallet,
        asset: &AssetIdentity,
    ) -> crate::Result<crate::model::WalletBalanceSnapshot> {
        if asset.network != format!("eip155:{LIVE_TEMPO_CHAIN_ID}")
            || asset.token_contract.as_deref() != Some(LIVE_TEMPO_PATH_USD)
        {
            return Err(crate::Error::Settlement(
                "Tempo balance source received an unconfigured asset".to_owned(),
            ));
        }
        let account = wallet
            .address
            .parse::<Address>()
            .map_err(|_| crate::Error::Settlement("Tempo balance wallet is invalid".to_owned()))?;
        let available = live_tempo_balance(&self.provider, self.token, account).await;
        let available = u128::try_from(available)
            .map_err(|_| crate::Error::Settlement("Tempo balance exceeds Amount".to_owned()))?;
        Ok(crate::model::WalletBalanceSnapshot {
            asset_available: Amount::new(available),
            // Tempo charges gas in the configured fee token. The native-gas
            // threshold is therefore intentionally zero for this source.
            native_available: Amount::ZERO,
            minimum_native_required: Amount::ZERO,
        })
    }
}

struct LiveTempoClock(i64);

impl crate::utils::clock::UnixClock for LiveTempoClock {
    fn now(&self) -> crate::Result<i64> {
        Ok(self.0)
    }
}

fn live_tempo_storage_config(database: &std::path::Path, kek_env: &str) -> crate::config::AppConfig {
    let mut config = crate::config::AppConfig::default();
    config.storage.provider = crate::config::StorageProvider::Sqlite;
    config.storage.sqlite.database_file = database.to_string_lossy().into_owned();
    config.agentvault.key_management.active_key_id = "live-tempo-kek-v1".to_owned();
    config.agentvault.key_management.keys = BTreeMap::from([(
        "live-tempo-kek-v1".to_owned(),
        crate::config::SecretRefConfig {
            file: String::new(),
            env: kek_env.to_owned(),
        },
    )]);
    config
}

async fn import_live_tempo_storage(
    config: &crate::config::AppConfig,
    manifest_path: &std::path::Path,
    manifest: &Value,
) -> std::sync::Arc<crate::storage::sqlite::SqliteStorage> {
    std::fs::write(
        manifest_path,
        crate::payment::ResourceImporter::encode_test_resource_documents(manifest),
    )
    .expect("write live Tempo resource documents");
    crate::payment::ResourceImporter::import_from_paths(config, &[manifest_path.to_path_buf()])
        .await
        .expect("import live Tempo Vault resources");
    std::sync::Arc::new(
        crate::storage::sqlite::SqliteStorage::connect(&config.storage.sqlite)
            .await
            .expect("open live Tempo Vault storage"),
    )
}

#[allow(clippy::similar_names, clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires the pinned Tempo localnet E2E profile"]
async fn live_tempo_vault_handlers_recover_once_and_reconcile_both_ledgers() {
    use crate::apm::AgentVaultMetrics;
    use crate::handler::ScenarioHandler;
    use crate::job::ReconciliationJob;
    use crate::model::wallet::WalletSelectionStrategy;
    use crate::model::IssuePaymentChallenge;
    use crate::model::PaymentAuthorizationResult;
    use crate::model::PaymentEventType;
    use crate::model::PaymentStatus;
    use crate::model::RecordOutboundPaymentResponse;
    use crate::model::SettleIncomingPayment;
    use crate::model::SignAuthorizedPayment;
    use crate::model::VerifyIncomingPayment;
    use crate::payment::EvmEvidenceSource;
    use crate::payment::EvmReceiptRegistry;
    use crate::payment::EvmRpcReceiptConfig;
    use crate::payment::EvmRpcReceiptSource;
    use crate::payment::ExternalSettlementProvider;
    use crate::payment::MerchantEvidenceRecorder;
    use crate::payment::OutboundEvidenceDependencies;
    use crate::payment::OutboundSigner;
    use crate::payment::OutboundSigningDependencies;
    use crate::payment::PaymentAuthorizer;
    use crate::payment::PaymentEvidenceRouter;
    use crate::payment::ReconciliationConfig;
    use crate::payment::ReconciliationEvidenceSource;
    use crate::payment::SettlementCoordinator;
    use crate::payment::SettlementDependencies;
    use crate::payment::SettlementProviderRegistry;
    use crate::protocol::PaymentProtocolFactory;
    use crate::storage::wallet::sqlite::SqliteWalletKeyRepository;
    use crate::storage::EventStorage;
    use crate::storage::LedgerQuery;
    use crate::storage::LedgerStorage;
    use crate::storage::PageRequest;
    use crate::storage::PaymentStorage;
    use crate::utils::secure_material::Aes256GcmMaterialProtector;
    use crate::utils::secure_material::SensitiveMaterialProtector;
    use alloy::providers::Provider as _;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use std::{sync::Arc, time::Duration};

    let rpc_url = std::env::var("TEMPO_RPC_URL").unwrap_or_else(|_| LIVE_TEMPO_RPC_URL.to_owned());
    let provider = mpp::server::tempo_provider(&rpc_url).expect("create Tempo provider");
    assert_eq!(
        provider.get_chain_id().await.expect("query Tempo chain ID"),
        LIVE_TEMPO_CHAIN_ID
    );

    let token = LIVE_TEMPO_PATH_USD.parse::<Address>().expect("pathUSD address");
    let payer_key = crate::utils::crypto::WalletCrypto::generate_private_key();
    let payer = crate::utils::crypto::WalletCrypto::address(&payer_key).expect("derive isolated payer");
    let payer_address = payer.parse::<Address>().expect("payer address");
    fund_live_tempo_payer(&provider, token, payer_address).await;
    let recipient_key = crate::utils::crypto::WalletCrypto::generate_private_key();
    let recipient = crate::utils::crypto::WalletCrypto::address(&recipient_key).expect("derive recipient");
    let recipient_address = recipient.parse::<Address>().expect("recipient address");
    let asset = AssetIdentity {
        network: format!("eip155:{LIVE_TEMPO_CHAIN_ID}"),
        asset: "pathUSD".to_owned(),
        token_contract: Some(LIVE_TEMPO_PATH_USD.to_owned()),
        decimals: 6,
    };
    let configured_asset = MppAsset {
        network: asset.network.clone(),
        currency: LIVE_TEMPO_PATH_USD.to_owned(),
        symbol: asset.asset.clone(),
        token_contract: asset.token_contract.clone(),
        decimals: asset.decimals,
    };

    let temporary = tempfile::TempDir::new().expect("create live Tempo Vault directory");
    let suffix = uuid::Uuid::new_v4().simple().to_string().to_ascii_uppercase();
    let payer_key_env = format!("AGENTVAULT_LIVE_TEMPO_PAYER_KEY_{suffix}");
    let kek_env = format!("AGENTVAULT_LIVE_TEMPO_KEK_{suffix}");
    std::env::set_var(&payer_key_env, hex::encode(&payer_key));
    std::env::set_var(&kek_env, URL_SAFE_NO_PAD.encode([7_u8; 32]));
    let payer_config = live_tempo_storage_config(&temporary.path().join("payer.sqlite3"), &kek_env);
    let payee_config = live_tempo_storage_config(&temporary.path().join("payee.sqlite3"), &kek_env);
    let budget = json!({"asset": "pathUSD", "decimals": 6});
    let policy_asset = serde_json::to_value(&asset).expect("asset JSON");
    let resource = "https://marketpulse.local/mcp/realtime";
    let scenario_id = "scenario-mpp-tempo-handler-live";
    let payer_manifest = json!({
        "version": 1,
        "clients": [{"id": "payer-client", "name": "Tempo payer", "auth_principal": "principal:tempo-payer"}],
        "wallets": [{
            "id": "payer-wallet", "custody": "local", "private_key_env": payer_key_env,
            "budget_unit": budget, "max_per_payment": "10000000", "max_per_day": "100000000"
        }],
        "scenarios": [{"id": scenario_id, "client_id": "payer-client", "name": "Tempo handler E2E"}],
        "policies": [{
            "id": "payer-policy", "scenario_id": scenario_id, "wallet_ids": ["payer-wallet"],
            "protocols": ["mpp"], "networks": [asset.network], "assets": [policy_asset],
            "budget_unit": budget, "allowed_recipients": [recipient],
            "allowed_origins": ["https://marketpulse.local"],
            "outbound_limits": {"max_per_payment": "10000000", "max_per_day": "100000000", "approval_enabled": false}
        }]
    });
    let payee_manifest = json!({
        "version": 1,
        "clients": [{"id": "payee-client", "name": "Tempo payee", "auth_principal": "principal:tempo-payee"}],
        "wallets": [{
            "id": "payee-wallet", "custody": "external", "algorithm": "secp256k1", "address": recipient,
            "budget_unit": budget, "max_per_payment": "10000000", "max_per_day": "100000000"
        }],
        "scenarios": [{"id": scenario_id, "client_id": "payee-client", "name": "Tempo handler E2E"}],
        "policies": [{
            "id": "payee-policy", "scenario_id": scenario_id, "wallet_ids": ["payee-wallet"],
            "protocols": ["mpp"], "networks": [asset.network], "assets": [policy_asset],
            "budget_unit": budget, "allowed_recipients": [recipient],
            "allowed_origins": ["https://marketpulse.local"],
            "inbound_limits": {"max_per_payment": "10000000", "max_per_day": "100000000", "approval_enabled": false}
        }],
        "offers": [{
            "id": "offer-mpp-tempo-handler-live", "scenario_id": scenario_id, "policy_id": "payee-policy",
            "resource": resource, "protocols": ["mpp"], "asset": policy_asset,
            "amount": "1000000", "wallet_id": "payee-wallet"
        }]
    });
    let payer_storage = import_live_tempo_storage(
        &payer_config,
        &temporary.path().join("payer-manifest.json"),
        &payer_manifest,
    )
    .await;
    let payee_storage = import_live_tempo_storage(
        &payee_config,
        &temporary.path().join("payee-manifest.json"),
        &payee_manifest,
    )
    .await;

    let payer_protocols = Arc::new(
        PaymentProtocolFactory::new()
            .register(Arc::new(
                MppAdapter::new_payer(vec![configured_asset.clone()], 60).expect("payer MPP adapter"),
            ))
            .build()
            .expect("payer protocol registry"),
    );
    let payee_protocols = Arc::new(
        PaymentProtocolFactory::new()
            .register(Arc::new(
                MppAdapter::new_payee(
                    vec![configured_asset],
                    "marketpulse.local",
                    "tempo-handler-e2e-challenge-secret-32-bytes",
                    60,
                )
                .expect("payee MPP adapter"),
            ))
            .build()
            .expect("payee protocol registry"),
    );
    let payer_ports = crate::storage::IStorage::ports(payer_storage.clone());
    let payee_ports = crate::storage::IStorage::ports(payee_storage.clone());
    let payer_scenarios = Arc::new(ScenarioHandler::new(
        payer_ports.clients.clone(),
        payer_ports.scenarios.clone(),
    ));
    let payee_scenarios = Arc::new(ScenarioHandler::new(
        payee_ports.clients.clone(),
        payee_ports.scenarios.clone(),
    ));
    let protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
        Aes256GcmMaterialProtector::new(
            "live-tempo-recovery-v1",
            [("live-tempo-recovery-v1".to_owned(), [9_u8; 32])],
        )
        .expect("recovery protector"),
    );

    let payer_handler = PaymentAuthorizer::new(
        payer_scenarios.clone(),
        payer_ports.policies.clone(),
        payer_ports.payments.clone(),
        payer_protocols.clone(),
        30,
        WalletSelectionStrategy::LeastSpent,
        AgentVaultMetrics::default(),
    )
    .expect("payer handler");
    let signer: Arc<dyn crate::storage::WalletPaymentSigner> = Arc::new(
        SqliteWalletKeyRepository::from_config((*payer_storage).clone(), &payer_config.agentvault.key_management)
            .expect("payer custody signer"),
    );
    let signing_handler = OutboundSigner::new(OutboundSigningDependencies {
        storage: payer_ports.signing.clone(),
        protocols: payer_protocols.clone(),
        balances: Arc::new(LiveTempoBalanceSource {
            provider: provider.clone(),
            token,
        }),
        signer,
        material_protector: protector.clone(),
    });

    let (external, broadcasts, server) = start_live_tempo_facilitator(&provider, &asset).await;
    let external: Arc<dyn ExternalSettlementProvider> = Arc::new(external);
    let providers = Arc::new(
        SettlementProviderRegistry::new([(
            MPP_PROTOCOL.to_owned(),
            "mpp-tempo-localnet".to_owned(),
            BTreeMap::from([(asset.network.clone(), 1)]),
            external,
        )])
        .expect("MPP settlement provider registry"),
    );
    let empty_sources: Vec<(String, Arc<dyn EvmEvidenceSource>)> = Vec::new();
    let payee_handler = Arc::new(
        SettlementCoordinator::new(
            SettlementDependencies {
                scenarios: payee_scenarios,
                offers: payee_ports.offers.clone(),
                policies: payee_ports.policies.clone(),
                wallets: payee_ports.wallets.clone(),
                payments: payee_ports.payments.clone(),
                events: payee_ports.events.clone(),
                protocols: payee_protocols,
                providers,
                material_protector: protector.clone(),
                authorizations: Arc::new(EvmReceiptRegistry::new(empty_sources).expect("empty receipt registry")),
            },
            1,
        )
        .expect("payee settlement handler"),
    );

    // Import timestamps are the key activation boundary. Advance the
    // request clock by one second so the test exercises an active key without
    // relying on wall-clock scheduling between the two operations.
    let now = time::OffsetDateTime::now_utc().unix_timestamp().saturating_add(1);
    let run_id = "run-mpp-tempo-handler-live";
    let request_hash = "ab".repeat(32);
    let issued = payee_handler
        .issue_challenge(
            "principal:tempo-payee",
            &IssuePaymentChallenge {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                offer_id: "offer-mpp-tempo-handler-live".to_owned(),
                resource: resource.to_owned(),
                origin: "https://marketpulse.local".to_owned(),
                protocol: MPP_PROTOCOL.to_owned(),
                request_hash: request_hash.clone(),
            },
            now,
        )
        .await
        .expect("payee issues durable MPP challenge");
    let payer_protocol = payer_protocols.get(MPP_PROTOCOL).expect("payer MPP protocol");
    let challenge = payer_protocol
        .parse_challenge(scenario_id, resource, &issued.payload, now, Some(issued.expires_at))
        .await
        .expect("payer parses MPP challenge");
    let authorization = payer_handler
        .authorize(
            "principal:tempo-payer",
            &crate::model::AuthorizePayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                challenge: challenge.clone(),
            },
            now.saturating_add(1),
        )
        .await
        .expect("payer authorizes MPP intent");
    let intent_id = match authorization {
        PaymentAuthorizationResult::Authorized { intent_id, .. } => intent_id,
        other => panic!("MPP handler E2E was not authorized: {other:?}"),
    };
    let signed = signing_handler
        .sign(
            "principal:tempo-payer",
            &SignAuthorizedPayment {
                intent_id: intent_id.clone(),
                challenge,
            },
            now.saturating_add(2),
        )
        .await
        .expect("Vault custody signs MPP charge");
    assert_eq!(signed.intent.run_id, run_id);
    assert_eq!(signed.intent.status, PaymentStatus::Signed);

    let verification = payee_handler
        .verify_payment(
            "principal:tempo-payee",
            &VerifyIncomingPayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                challenge_hash: issued.challenge_hash.clone(),
                request_hash,
                credential: signed.credential,
            },
            now.saturating_add(3),
        )
        .await
        .expect("payee verifies MPP credential through facilitator");
    assert!(verification.valid);
    let verification_ref = verification.verification_ref.expect("verification reference");
    let settle = SettleIncomingPayment {
        scenario_id: scenario_id.to_owned(),
        run_id: run_id.to_owned(),
        intent_id: verification.intent.id.clone(),
        verification_ref,
    };
    let payer_before = live_tempo_balance(&provider, token, payer_address).await;
    let recipient_before = live_tempo_balance(&provider, token, recipient_address).await;
    assert!(
        payee_handler
            .settle_payment("principal:tempo-payee", &settle, now.saturating_add(4))
            .await
            .is_err(),
        "the first handler settle must observe the injected lost response"
    );
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let settled = payee_handler
        .settle_payment("principal:tempo-payee", &settle, now.saturating_add(5))
        .await
        .expect("payee recovers settlement through read-only verify");
    let replay = payee_handler
        .settle_payment("principal:tempo-payee", &settle, now.saturating_add(6))
        .await
        .expect("settled payee retry is idempotent");
    assert_eq!(replay.receipt, settled.receipt);
    assert_eq!(settled.intent.run_id, run_id);
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let transaction = settled.receipt.tx_hash.clone().expect("settled Tempo transaction");
    let (payer_after, recipient_after, gas_fee, gas_limit, gas_used) = assert_live_tempo_accounting(
        &provider,
        &LiveTempoAccounts {
            token,
            payer: payer_address,
            recipient: recipient_address,
        },
        &transaction,
        settled.intent.amount,
        payer_before,
        recipient_before,
    )
    .await;

    let receipt_header = mpp::Receipt::success(TEMPO_METHOD, transaction.clone())
        .to_header()
        .expect("MPP receipt header");
    let receipt_source: Arc<dyn EvmEvidenceSource> = Arc::new(
        EvmRpcReceiptSource::new(
            &asset.network,
            EvmRpcReceiptConfig {
                endpoint: rpc_url,
                secret_headers: BTreeMap::new(),
                timeout: Duration::from_secs(10),
                max_response_bytes: 1_048_576,
                minimum_confirmations: 1,
                allow_insecure_loopback: true,
            },
        )
        .expect("Tempo receipt source"),
    );
    let receipt_registry =
        Arc::new(EvmReceiptRegistry::new([(asset.network.clone(), receipt_source)]).expect("Tempo receipt registry"));
    let outbound = Arc::new(MerchantEvidenceRecorder::new(OutboundEvidenceDependencies {
        scenarios: payer_scenarios,
        payments: payer_ports.payments.clone(),
        events: payer_ports.events.clone(),
        protocols: payer_protocols,
        material_protector: protector,
        receipts: receipt_registry,
    }));
    let reported = outbound
        .record_merchant_response(
            "principal:tempo-payer",
            &RecordOutboundPaymentResponse {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                intent_id: intent_id.clone(),
                protocol: MPP_PROTOCOL.to_owned(),
                header_name: "payment-receipt".to_owned(),
                header_value: receipt_header,
            },
            now.saturating_add(7),
        )
        .await
        .expect("payer records untrusted MPP merchant receipt");
    assert!(reported.accepted);
    assert_eq!(reported.intent.status, PaymentStatus::PendingSettlement);

    let provider_evidence: Arc<dyn ReconciliationEvidenceSource> = payee_handler;
    let router: Arc<dyn ReconciliationEvidenceSource> =
        Arc::new(PaymentEvidenceRouter::new(provider_evidence, outbound));
    let job = ReconciliationJob::with_clock(
        payer_ports.payments.clone(),
        router,
        Arc::new(LiveTempoClock(now.saturating_add(20))),
        ReconciliationConfig {
            worker_id: "mpp-tempo-handler-live".to_owned(),
            batch_size: 10,
            lease_duration: Duration::from_secs(10),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(8),
        },
    )
    .expect("payer reconciliation job");
    let reconciliation = job.run_once().await.expect("reconcile payer MPP receipt");
    assert_eq!(reconciliation.settled, 1);
    let payer_intent = PaymentStorage::get(payer_ports.payments.as_ref(), &intent_id)
        .await
        .expect("load payer intent")
        .expect("payer intent exists");
    assert_eq!(payer_intent.status, PaymentStatus::Settled);
    assert_eq!(payer_intent.run_id, run_id);
    assert_eq!(payer_intent.tx_hash.as_deref(), Some(transaction.as_str()));

    let payer_ledger = LedgerStorage::list(
        payer_ports.ledger.as_ref(),
        &LedgerQuery {
            intent_id: Some(intent_id.clone()),
            ..LedgerQuery::default()
        },
    )
    .await
    .expect("payer ledger");
    let payee_ledger = LedgerStorage::list(
        payee_ports.ledger.as_ref(),
        &LedgerQuery {
            intent_id: Some(settled.intent.id.clone()),
            ..LedgerQuery::default()
        },
    )
    .await
    .expect("payee ledger");
    assert_eq!(payer_ledger.total, 1);
    assert_eq!(payee_ledger.total, 1);
    assert_eq!(payer_ledger.items[0].tx_hash.as_deref(), Some(transaction.as_str()));
    assert_eq!(payee_ledger.items[0].tx_hash.as_deref(), Some(transaction.as_str()));
    let payee_events = EventStorage::list_for_intent(
        payee_ports.events.as_ref(),
        &settled.intent.id,
        PageRequest { offset: 0, limit: 100 },
    )
    .await
    .expect("payee events");
    assert_eq!(
        payee_events
            .items
            .iter()
            .filter(|event| event.event_type == PaymentEventType::SettlementRequested)
            .count(),
        1
    );

    println!(
        "MPP_TEMPO_EVIDENCE={}",
        json!({
            "run_id": run_id,
            "protocol": MPP_PROTOCOL,
            "method": TEMPO_METHOD,
            "chain_id": LIVE_TEMPO_CHAIN_ID,
            "transaction": transaction,
            "amount": settled.intent.amount.to_string(),
            "gas_fee": gas_fee.to_string(),
            "gas_limit": gas_limit,
            "gas_used": gas_used,
            "payer_before": payer_before.to_string(),
            "payer_after": payer_after.to_string(),
            "recipient_before": recipient_before.to_string(),
            "recipient_after": recipient_after.to_string(),
            "broadcasts": broadcasts.load(std::sync::atomic::Ordering::SeqCst),
            "recovered_after_lost_response": true,
            "vault_handler_roundtrip": true,
            "encrypted_custody_signing": true,
            "outbound_receipt_reconciled": true,
            "payer_ledger_entries": payer_ledger.total,
            "payee_ledger_entries": payee_ledger.total,
            "settlement_requested_events": 1,
        })
    );
    server.abort();
    let _ = server.await;
    std::env::remove_var(payer_key_env);
    std::env::remove_var(kek_env);
}
