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
        &challenge.nonce,
    )
    .unwrap();
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
    let nonce = "11".repeat(32);
    let first_hash = x402_challenge_hash(resource, 160, &first, &nonce).unwrap();
    assert_eq!(
        first_hash,
        x402_challenge_hash(resource, 160, &reordered, &nonce).unwrap()
    );

    let mut tampered = reordered;
    tampered["accepts"][0]["amount"] = Value::String("1000001".to_owned());
    assert_ne!(
        first_hash,
        x402_challenge_hash(resource, 160, &tampered, &nonce).unwrap()
    );
    assert_ne!(
        first_hash,
        x402_challenge_hash(resource, 160, &first, &"22".repeat(32)).unwrap()
    );
    assert!(x402_challenge_hash(resource, 160, &first, "not-a-nonce").is_err());
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
async fn http_provider_accepts_header_only_and_rejects_conflicting_body() {
    let adapter = adapter();
    let challenge = adapter
        .build_challenge(&offer(), "0x2222222222222222222222222222222222222222", 100)
        .await
        .unwrap();
    let wire = challenge.payload["payment_required"].clone();
    let encoded = Base64Bytes::encode(serde_json::to_vec(&wire).unwrap()).to_string();
    let headers = BTreeMap::from([("PAYMENT-REQUIRED".to_owned(), encoded)]);

    assert_eq!(
        adapter.decode_http_challenge(&offer().resource, &headers, "").unwrap(),
        Some(wire.clone())
    );
    assert_eq!(
        adapter
            .decode_http_challenge(&offer().resource, &headers, &serde_json::to_string(&wire).unwrap())
            .unwrap(),
        Some(wire.clone())
    );

    let mut conflicting = wire;
    conflicting["accepts"][0]["amount"] = Value::String("1000001".to_owned());
    assert!(matches!(
        adapter.decode_http_challenge(
            &offer().resource,
            &headers,
            &serde_json::to_string(&conflicting).unwrap()
        ),
        Err(Error::PaymentConflict(_))
    ));
}

#[test]
fn http_provider_decodes_only_official_x402_merchant_evidence() {
    let success = Base64Bytes::encode(
        serde_json::to_vec(&json!({
            "success": true,
            "transaction": format!("0x{}", "11".repeat(32)),
            "network": "eip155:8453",
            "payer": format!("0x{}", "22".repeat(20)),
        }))
        .unwrap(),
    )
    .to_string();
    let evidence = adapter().decode_merchant_evidence(&success).unwrap();
    assert!(evidence.accepted);
    assert_eq!(evidence.network.as_deref(), Some("eip155:8453"));

    let unofficial = Base64Bytes::encode(
        serde_json::to_vec(&json!({
            "valid": true,
            "transactionHash": format!("0x{}", "11".repeat(32)),
            "network": "eip155:8453",
        }))
        .unwrap(),
    )
    .to_string();
    assert!(adapter().decode_merchant_evidence(&unofficial).is_err());
}

#[tokio::test]
async fn challenge_binds_server_owned_offer_fields() {
    let recipient = "0x2222222222222222222222222222222222222222";
    let challenge = adapter().build_challenge(&offer(), recipient, 100).await.unwrap();
    assert_eq!(challenge.recipient, recipient);
    assert_eq!(challenge.amount, Amount::new(1_000_000));
    assert_eq!(challenge.offer_id.as_deref(), Some("offer-1"));
    assert_eq!(challenge.nonce.len(), 64);
    assert_ne!(challenge.nonce, challenge.challenge_hash);

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
    assert_ne!(parsed.challenge_hash, challenge.challenge_hash);

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
    assert_eq!(replay.nonce, parsed.nonce);
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
    assert_eq!(
        second.payload["payment_required"],
        challenge.payload["payment_required"]
    );
    assert_ne!(second.nonce, challenge.nonce);
    assert_ne!(second.challenge_hash, challenge.challenge_hash);

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

#[derive(Clone)]
struct LiveSolanaFixture {
    rpc_url: String,
    chain_reference: String,
    payer_keypair: String,
    facilitator_keypair: String,
    payer_token_account: String,
    recipient_token_account: String,
    recipient: String,
    mint: String,
}

impl LiveSolanaFixture {
    fn from_env() -> Self {
        let required = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"));
        Self {
            rpc_url: required("SOLANA_RPC_URL"),
            chain_reference: required("SOLANA_CHAIN_REFERENCE"),
            payer_keypair: required("SOLANA_PAYER_KEYPAIR_JSON"),
            facilitator_keypair: required("SOLANA_FACILITATOR_KEYPAIR_JSON"),
            payer_token_account: required("SOLANA_PAYER_TOKEN_ACCOUNT"),
            recipient_token_account: required("SOLANA_RECIPIENT_TOKEN_ACCOUNT"),
            recipient: required("SOLANA_RECIPIENT_ADDRESS"),
            mint: required("SOLANA_TOKEN_MINT"),
        }
    }

    fn keypair(encoded: &str) -> solana_keypair::Keypair {
        let bytes: Vec<u8> = serde_json::from_str(encoded).expect("Solana keypair JSON");
        solana_keypair::Keypair::try_from(bytes.as_slice()).expect("valid Solana keypair")
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        let response = reqwest::Client::new()
            .post(&self.rpc_url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .expect("Solana RPC request")
            .error_for_status()
            .expect("Solana RPC status");
        let wire: Value = response.json().await.expect("Solana RPC JSON");
        assert!(wire.get("error").is_none(), "Solana RPC {method} failed: {wire}");
        wire.get("result").cloned().expect("Solana RPC result")
    }

    async fn token_balance(&self, account: &str) -> u64 {
        self.rpc("getTokenAccountBalance", json!([account, {"commitment": "confirmed"}]))
            .await["value"]["amount"]
            .as_str()
            .expect("SPL token amount")
            .parse()
            .expect("SPL token u64")
    }

    async fn native_balance(&self, address: &str) -> u64 {
        self.rpc("getBalance", json!([address, {"commitment": "confirmed"}]))
            .await["value"]
            .as_u64()
            .expect("lamport balance")
    }

    async fn transaction_fee(&self, signature: &str) -> u64 {
        self.rpc(
            "getTransaction",
            json!([signature, {"encoding": "json", "commitment": "confirmed", "maxSupportedTransactionVersion": 0}]),
        )
        .await["meta"]["fee"]
            .as_u64()
            .expect("transaction fee")
    }
}

struct LiveSolanaClock(std::sync::atomic::AtomicI64);

impl LiveSolanaClock {
    fn new(now: i64) -> Self {
        Self(std::sync::atomic::AtomicI64::new(now))
    }

    fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
    }
}

impl crate::handler::UnixClock for LiveSolanaClock {
    fn now(&self) -> crate::Result<i64> {
        Ok(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}

fn live_solana_storage_config(database: &std::path::Path, kek_env: &str) -> crate::config::AppConfig {
    let mut config = crate::config::AppConfig::default();
    config.storage.provider = crate::config::StorageProvider::Sqlite;
    config.storage.sqlite.database_file = database.to_string_lossy().into_owned();
    config.agentvault.key_management.active_key_id = "live-solana-kek-v1".to_owned();
    config.agentvault.key_management.keys = BTreeMap::from([(
        "live-solana-kek-v1".to_owned(),
        crate::config::SecretRefConfig {
            file: String::new(),
            env: kek_env.to_owned(),
        },
    )]);
    config
}

async fn import_live_solana_storage(
    config: &crate::config::AppConfig,
    manifest_path: &std::path::Path,
    manifest: &Value,
) -> std::sync::Arc<crate::storage::sqlite::SqliteStorage> {
    std::fs::write(manifest_path, crate::handler::encode_test_resource_documents(manifest))
        .expect("write live Solana resource documents");
    crate::handler::import_from_paths(config, &[manifest_path.to_path_buf()])
        .await
        .expect("import live Solana Vault resources");
    std::sync::Arc::new(
        crate::storage::sqlite::SqliteStorage::connect(&config.storage.sqlite)
            .await
            .expect("open live Solana Vault storage"),
    )
}

type LiveOfficialSolanaFacilitator =
    x402_chain_solana::v2_solana_exact::V2SolanaExactFacilitator<x402_chain_solana::chain::SolanaChainProvider>;

#[derive(Clone)]
struct LiveSolanaFacilitatorState {
    facilitator: std::sync::Arc<LiveOfficialSolanaFacilitator>,
    network: String,
    broadcasts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn live_solana_proto_request(value: &Value) -> x402_types::proto::VerifyRequest {
    serde_json::value::RawValue::from_string(serde_json::to_string(value).expect("facilitator request JSON"))
        .expect("facilitator raw request")
        .into()
}

async fn live_solana_verify_endpoint(
    axum::extract::State(state): axum::extract::State<LiveSolanaFacilitatorState>,
    axum::Json(request): axum::Json<Value>,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    use x402_types::scheme::X402SchemeFacilitator as _;

    match state.facilitator.verify(&live_solana_proto_request(&request)).await {
        Ok(response) => (axum::http::StatusCode::OK, axum::Json(response.0)),
        Err(_) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"isValid": false, "invalidReason": "verification_failed"})),
        ),
    }
}

async fn live_solana_settle_endpoint(
    axum::extract::State(state): axum::extract::State<LiveSolanaFacilitatorState>,
    axum::Json(request): axum::Json<Value>,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    use x402_types::scheme::X402SchemeFacilitator as _;

    match state.facilitator.settle(&live_solana_proto_request(&request)).await {
        Ok(response) => {
            state.broadcasts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (axum::http::StatusCode::OK, axum::Json(response.0))
        }
        Err(_) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "success": false,
                "errorReason": "settlement_failed",
                "network": state.network,
            })),
        ),
    }
}

async fn start_live_solana_facilitator(
    fixture: &LiveSolanaFixture,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use std::{str::FromStr as _, sync::Arc};

    use axum::{routing::post, Router};
    use x402_chain_solana::{
        chain::{SolanaChainProvider, SolanaChainReference},
        v2_solana_exact::{V2SolanaExactFacilitator, V2SolanaExactFacilitatorConfig},
    };

    let chain = SolanaChainReference::from_str(&fixture.chain_reference).expect("Solana chain reference");
    let provider = SolanaChainProvider::new(
        LiveSolanaFixture::keypair(&fixture.facilitator_keypair),
        fixture.rpc_url.clone(),
        None,
        chain,
        400_000,
        1_000_000,
    )
    .await
    .expect("official Solana facilitator provider");
    let broadcasts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = LiveSolanaFacilitatorState {
        facilitator: Arc::new(V2SolanaExactFacilitator::new(
            provider,
            V2SolanaExactFacilitatorConfig::default(),
        )),
        network: format!("solana:{}", fixture.chain_reference),
        broadcasts: broadcasts.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local Solana facilitator");
    let address = listener.local_addr().expect("local facilitator address");
    let application = Router::new()
        .route("/verify", post(live_solana_verify_endpoint))
        .route("/settle", post(live_solana_settle_endpoint))
        .with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("serve local Solana facilitator");
    });
    (format!("http://{address}/"), broadcasts, server)
}

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the pinned local Solana validator fixture"]
async fn live_solana_x402_v2_official_facilitator_conserves_spl_and_lamports() {
    use std::{str::FromStr as _, time::Duration};

    use solana_signer::Signer as _;
    use x402_chain_solana::{
        chain::{Address as SolanaAddress, SolanaChainProvider, SolanaChainReference},
        v1_solana_exact::types::SupportedPaymentKindExtra,
        v2_solana_exact::{
            PaymentPayload as SolanaPaymentPayload, PaymentRequirements as SolanaPaymentRequirements,
            V2SolanaExactFacilitator, V2SolanaExactFacilitatorConfig, VerifyRequest as SolanaVerifyRequest,
        },
    };
    use x402_types::{
        proto::{self, v1::SettleResponse as OfficialSettleResponse, v2::X402Version2},
        scheme::X402SchemeFacilitator as _,
    };

    let fixture = LiveSolanaFixture::from_env();
    let genesis = fixture.rpc("getGenesisHash", json!([])).await;
    let genesis = genesis.as_str().expect("Solana genesis hash");
    assert_eq!(&genesis[..32], fixture.chain_reference);

    let payer = LiveSolanaFixture::keypair(&fixture.payer_keypair);
    let payer_address = payer.pubkey().to_string();
    let facilitator_keypair = LiveSolanaFixture::keypair(&fixture.facilitator_keypair);
    let facilitator_address = facilitator_keypair.pubkey().to_string();
    let network = format!("solana:{}", fixture.chain_reference);
    let amount = 1_000_000_u64;
    let configured_asset = X402Asset {
        network: network.clone(),
        contract: fixture.mint.clone(),
        symbol: "USDC".to_owned(),
        decimals: 6,
    };
    let adapter = X402Adapter::new_with_solana(
        vec![configured_asset.clone()],
        60,
        vec![X402SolanaConfig {
            network: network.clone(),
            fee_payer: facilitator_address.clone(),
            rpc_url: fixture.rpc_url.clone(),
            rpc_timeout: Duration::from_secs(10),
            rpc_max_response_bytes: 1_048_576,
            rpc_headers: BTreeMap::new(),
            allow_insecure_loopback: true,
        }],
    )
    .expect("Vault x402 Solana adapter");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let challenge = adapter
        .build_challenge(
            &PaymentOffer {
                id: "offer-x402-solana-live".to_owned(),
                scenario_id: "scenario-x402-solana-live".to_owned(),
                policy_id: "policy-x402-solana-live".to_owned(),
                resource: "https://marketpulse.local/solana".to_owned(),
                protocols: vec![X402_PROTOCOL.to_owned()],
                asset: AssetIdentity {
                    network: network.clone(),
                    asset: "USDC".to_owned(),
                    token_contract: Some(fixture.mint.clone()),
                    decimals: 6,
                },
                amount: Amount::new(u128::from(amount)),
                wallet_id: "payee-wallet".to_owned(),
                enabled: true,
                created_at: now,
                updated_at: now,
            },
            &fixture.recipient,
            now,
        )
        .await
        .expect("Vault builds official Solana x402 challenge");

    let payer_before = fixture.token_balance(&fixture.payer_token_account).await;
    let recipient_before = fixture.token_balance(&fixture.recipient_token_account).await;
    let payer_native_before = fixture.native_balance(&payer_address).await;
    let facilitator_native_before = fixture.native_balance(&facilitator_address).await;

    let signing = adapter
        .prepare_signing(&challenge, &payer_address, now.saturating_add(1))
        .await
        .expect("Vault prepares official Solana x402 transaction");
    let payer_key_bytes: Vec<u8> = serde_json::from_str(&fixture.payer_keypair).expect("payer keypair JSON");
    let signature = crate::utils::crypto::sign_solana_message(&payer_key_bytes[..32], &signing.digest)
        .expect("Vault custody primitive signs intent-bound Solana message");
    let credential = adapter
        .complete_signing(&challenge, signing, &signature)
        .await
        .expect("Vault completes official Solana x402 payload");
    let settlement = adapter
        .settle_payment(&challenge, &credential)
        .await
        .expect("Vault maps credential to facilitator request");
    let payment_payload: SolanaPaymentPayload =
        serde_json::from_value(settlement.payment_payload).expect("official Solana payment payload");
    let requirements: SolanaPaymentRequirements =
        serde_json::from_value(settlement.payment_requirements).expect("official Solana requirements");
    let _: SupportedPaymentKindExtra = requirements.extra.clone();
    let _: SolanaAddress = requirements.pay_to.clone();
    let client_simulation = fixture
        .rpc(
            "simulateTransaction",
            json!([
                payment_payload.payload.transaction,
                {
                    "encoding": "base64",
                    "sigVerify": false,
                    "replaceRecentBlockhash": true,
                    "commitment": "confirmed"
                }
            ]),
        )
        .await;
    assert_eq!(
        client_simulation["value"]["err"],
        Value::Null,
        "official client transaction simulation failed: {}",
        client_simulation["value"]["logs"]
    );
    let request = SolanaVerifyRequest {
        x402_version: X402Version2,
        payment_payload,
        payment_requirements: requirements,
    };
    let request = proto::VerifyRequest::try_from(&request).expect("official facilitator request");

    let chain_reference = SolanaChainReference::from_str(&fixture.chain_reference).expect("Solana CAIP reference");
    let provider = SolanaChainProvider::new(
        facilitator_keypair,
        fixture.rpc_url.clone(),
        None,
        chain_reference,
        400_000,
        1_000_000,
    )
    .await
    .expect("official Solana chain provider");
    let facilitator = V2SolanaExactFacilitator::new(provider, V2SolanaExactFacilitatorConfig::default());
    let verified = facilitator.verify(&request).await.expect("official facilitator verify");
    assert_eq!(verified.0["isValid"], true);
    assert_eq!(verified.0["payer"], payer_address);

    assert_eq!(fixture.token_balance(&fixture.payer_token_account).await, payer_before);
    assert_eq!(
        fixture.token_balance(&fixture.recipient_token_account).await,
        recipient_before
    );

    let settled = facilitator.settle(&request).await.expect("official facilitator settle");
    let settlement: OfficialSettleResponse = serde_json::from_value(settled.0).expect("official settle response");
    let (settled_payer, transaction, settled_network) = match settlement {
        OfficialSettleResponse::Success {
            payer,
            transaction,
            network,
        } => (payer, transaction, network),
        OfficialSettleResponse::Error { reason, .. } => panic!("Solana settlement failed: {reason}"),
    };
    assert_eq!(settled_payer, payer_address);
    assert_eq!(settled_network, network);

    let payer_after = fixture.token_balance(&fixture.payer_token_account).await;
    let recipient_after = fixture.token_balance(&fixture.recipient_token_account).await;
    let payer_native_after = fixture.native_balance(&payer_address).await;
    let facilitator_native_after = fixture.native_balance(&facilitator_address).await;
    let fee = fixture.transaction_fee(&transaction).await;
    assert_eq!(payer_before - payer_after, amount);
    assert_eq!(recipient_after - recipient_before, amount);
    assert_eq!(payer_before + recipient_before, payer_after + recipient_after);
    assert_eq!(
        payer_native_before, payer_native_after,
        "facilitator must remain the Solana fee payer"
    );
    assert_eq!(facilitator_native_before - facilitator_native_after, fee);
    assert!(fee > 0);

    println!(
        "X402_SOLANA_EVIDENCE={}",
        json!({
            "version": 2,
            "scheme": "exact",
            "network": settled_network,
            "transaction": transaction,
            "payer": settled_payer,
            "recipient": fixture.recipient,
            "mint": fixture.mint,
            "amount": amount,
            "payer_token_before": payer_before,
            "payer_token_after": payer_after,
            "recipient_token_before": recipient_before,
            "recipient_token_after": recipient_after,
            "payer_native_before": payer_native_before,
            "payer_native_after": payer_native_after,
            "facilitator_native_before": facilitator_native_before,
            "facilitator_native_after": facilitator_native_after,
            "transaction_fee": fee,
            "verify_before_settle": true,
            "broadcast_count": 1,
            "vault_protocol_adapter": true,
            "vault_custody_signing": true,
        })
    );
}

#[allow(clippy::similar_names, clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the pinned local Solana validator fixture"]
async fn live_solana_x402_vault_handlers_reconcile_both_ledgers() {
    use std::{sync::Arc, time::Duration};

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use reqwest::header::HeaderMap;
    use solana_signer::Signer as _;
    use x402_types::{proto::v2::SettleResponse, util::b64::Base64Bytes};

    use crate::{
        apm::AgentVaultMetrics,
        handler::{
            IssuePaymentChallenge, OutboundEvidenceDependencies, OutboundEvidenceHandler, OutboundSigningDependencies,
            OutboundSigningHandler, PaymentEvidenceRouter, PaymentHandler, ReceiptRegistry, ReceiptSource,
            ReconciliationConfig, ReconciliationEvidenceSource, ReconciliationJob, RecordOutboundPaymentResponse,
            ScenarioHandler, SettleIncomingPayment, SettlementDependencies, SettlementHandler,
            SettlementProviderConfig, SettlementProviderRegistry, SignAuthorizedPayment, SolanaRpcBalanceSource,
            SolanaRpcReceiptConfig, SolanaRpcReceiptSource, VerifyIncomingPayment, X402FacilitatorProvider,
        },
        model::{wallet::WalletSelectionStrategy, PaymentAuthorizationResult, PaymentEventType, PaymentStatus},
        protocol::PaymentProtocolFactory,
        storage::{
            sqlite::SqliteWalletKeyRepository, EventStorage, LedgerQuery, LedgerStorage, PageRequest, PaymentStorage,
        },
        utils::secure_material::{Aes256GcmMaterialProtector, SensitiveMaterialProtector},
    };

    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let fixture = LiveSolanaFixture::from_env();
    let payer_keypair = LiveSolanaFixture::keypair(&fixture.payer_keypair);
    let payer = payer_keypair.pubkey().to_string();
    let facilitator = LiveSolanaFixture::keypair(&fixture.facilitator_keypair)
        .pubkey()
        .to_string();
    let network = format!("solana:{}", fixture.chain_reference);
    let asset = AssetIdentity {
        network: network.clone(),
        asset: "USDC".to_owned(),
        token_contract: Some(fixture.mint.clone()),
        decimals: 6,
    };
    let configured_asset = X402Asset {
        network: network.clone(),
        contract: fixture.mint.clone(),
        symbol: "USDC".to_owned(),
        decimals: 6,
    };
    let solana_runtime = || X402SolanaConfig {
        network: network.clone(),
        fee_payer: facilitator.clone(),
        rpc_url: fixture.rpc_url.clone(),
        rpc_timeout: Duration::from_secs(10),
        rpc_max_response_bytes: 1_048_576,
        rpc_headers: BTreeMap::new(),
        allow_insecure_loopback: true,
    };

    let temporary = tempfile::TempDir::new().expect("create live Solana Vault directory");
    let suffix = uuid::Uuid::new_v4().simple().to_string().to_ascii_uppercase();
    let payer_key_env = format!("AGENTVAULT_LIVE_SOLANA_PAYER_KEY_{suffix}");
    let kek_env = format!("AGENTVAULT_LIVE_SOLANA_KEK_{suffix}");
    let payer_key_bytes: Vec<u8> = serde_json::from_str(&fixture.payer_keypair).expect("payer keypair JSON");
    std::env::set_var(&payer_key_env, hex::encode(&payer_key_bytes[..32]));
    std::env::set_var(&kek_env, URL_SAFE_NO_PAD.encode([11_u8; 32]));
    let payer_config = live_solana_storage_config(&temporary.path().join("payer.sqlite3"), &kek_env);
    let payee_config = live_solana_storage_config(&temporary.path().join("payee.sqlite3"), &kek_env);
    let budget = json!({"asset": "USDC", "decimals": 6});
    let policy_asset = serde_json::to_value(&asset).expect("asset JSON");
    let resource = "https://marketpulse.local/mcp/solana-feed";
    let scenario_id = "scenario-x402-solana-handler-live";
    let payer_manifest = json!({
        "version": 1,
        "clients": [{"id": "payer-client", "name": "Solana payer", "auth_principal": "principal:solana-payer"}],
        "wallets": [{
            "id": "payer-wallet", "custody": "local", "algorithm": "ed25519",
            "private_key_env": payer_key_env, "budget_unit": budget,
            "max_per_payment": "10000000", "max_per_day": "100000000"
        }],
        "scenarios": [{"id": scenario_id, "client_id": "payer-client", "name": "Solana handler E2E"}],
        "policies": [{
            "id": "payer-policy", "scenario_id": scenario_id, "wallet_ids": ["payer-wallet"],
            "protocols": ["x402"], "networks": [network], "assets": [policy_asset],
            "budget_unit": budget, "allowed_recipients": [fixture.recipient],
            "allowed_origins": ["https://marketpulse.local"],
            "outbound_limits": {"max_per_payment": "10000000", "max_per_day": "100000000", "approval_enabled": false}
        }]
    });
    let payee_manifest = json!({
        "version": 1,
        "clients": [{"id": "payee-client", "name": "Solana payee", "auth_principal": "principal:solana-payee"}],
        "wallets": [{
            "id": "payee-wallet", "custody": "external", "algorithm": "ed25519",
            "address": fixture.recipient, "budget_unit": budget,
            "max_per_payment": "10000000", "max_per_day": "100000000"
        }],
        "scenarios": [{"id": scenario_id, "client_id": "payee-client", "name": "Solana handler E2E"}],
        "policies": [{
            "id": "payee-policy", "scenario_id": scenario_id, "wallet_ids": ["payee-wallet"],
            "protocols": ["x402"], "networks": [network], "assets": [policy_asset],
            "budget_unit": budget, "allowed_recipients": [fixture.recipient],
            "allowed_origins": ["https://marketpulse.local"],
            "inbound_limits": {"max_per_payment": "10000000", "max_per_day": "100000000", "approval_enabled": false}
        }],
        "offers": [{
            "id": "offer-x402-solana-handler-live", "scenario_id": scenario_id,
            "policy_id": "payee-policy", "resource": resource, "protocols": ["x402"],
            "asset": policy_asset, "amount": "1000000", "wallet_id": "payee-wallet"
        }]
    });
    let payer_storage = import_live_solana_storage(
        &payer_config,
        &temporary.path().join("payer-manifest.json"),
        &payer_manifest,
    )
    .await;
    let payee_storage = import_live_solana_storage(
        &payee_config,
        &temporary.path().join("payee-manifest.json"),
        &payee_manifest,
    )
    .await;

    let payer_protocols = Arc::new(
        PaymentProtocolFactory::new()
            .register(Arc::new(
                X402Adapter::new_with_solana(vec![configured_asset.clone()], 60, vec![solana_runtime()])
                    .expect("payer x402 Solana adapter"),
            ))
            .build()
            .expect("payer protocol registry"),
    );
    let payee_protocols = Arc::new(
        PaymentProtocolFactory::new()
            .register(Arc::new(
                X402Adapter::new_with_solana(vec![configured_asset.clone()], 60, vec![solana_runtime()])
                    .expect("payee x402 Solana adapter"),
            ))
            .build()
            .expect("payee protocol registry"),
    );
    let payer_scenarios = Arc::new(ScenarioHandler::new(payer_storage.clone(), payer_storage.clone()));
    let payee_scenarios = Arc::new(ScenarioHandler::new(payee_storage.clone(), payee_storage.clone()));
    let protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
        Aes256GcmMaterialProtector::new(
            "live-solana-recovery-v1",
            [("live-solana-recovery-v1".to_owned(), [13_u8; 32])],
        )
        .expect("recovery protector"),
    );
    let payer_handler = PaymentHandler::new(
        payer_scenarios.clone(),
        payer_storage.clone(),
        payer_storage.clone(),
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
    let balance_source = Arc::new(
        SolanaRpcBalanceSource::new(
            &network,
            &fixture.rpc_url,
            &BTreeMap::new(),
            Duration::from_secs(10),
            1_048_576,
            Amount::ZERO,
            true,
        )
        .expect("Solana balance source"),
    );
    let signing_handler = OutboundSigningHandler::new(OutboundSigningDependencies {
        storage: payer_storage.clone(),
        protocols: payer_protocols.clone(),
        balances: balance_source,
        signer,
        material_protector: protector.clone(),
    });

    let (facilitator_url, broadcasts, server) = start_live_solana_facilitator(&fixture).await;
    let facilitator_provider: Arc<dyn crate::handler::ExternalSettlementProvider> = Arc::new(
        X402FacilitatorProvider::new(
            &SettlementProviderConfig {
                base_url: facilitator_url,
                verify_path: "verify".to_owned(),
                settle_path: "settle".to_owned(),
                timeout: Duration::from_secs(10),
                max_response_bytes: 1_048_576,
                allow_insecure_loopback: true,
            },
            HeaderMap::new(),
            vec![configured_asset],
        )
        .expect("HTTP x402 Solana facilitator provider"),
    );
    let providers = Arc::new(
        SettlementProviderRegistry::new([(
            X402_PROTOCOL.to_owned(),
            "x402-solana-localnet".to_owned(),
            BTreeMap::from([(network.clone(), 1)]),
            facilitator_provider,
        )])
        .expect("x402 settlement provider registry"),
    );
    let receipt_source: Arc<dyn ReceiptSource> = Arc::new(
        SolanaRpcReceiptSource::new(
            &network,
            &SolanaRpcReceiptConfig {
                endpoint: fixture.rpc_url.clone(),
                secret_headers: BTreeMap::new(),
                timeout: Duration::from_secs(10),
                max_response_bytes: 1_048_576,
                minimum_confirmations: 1,
                allow_insecure_loopback: true,
            },
        )
        .expect("Solana receipt source"),
    );
    let receipt_registry = Arc::new(
        ReceiptRegistry::from_sources(
            [(network.clone(), receipt_source)],
            std::iter::empty::<(String, Arc<dyn crate::handler::EvmAuthorizationSource>)>(),
        )
        .expect("mixed-chain receipt registry"),
    );
    let payee_handler = Arc::new(
        SettlementHandler::new(
            SettlementDependencies {
                scenarios: payee_scenarios,
                offers: payee_storage.clone(),
                policies: payee_storage.clone(),
                wallets: payee_storage.clone(),
                payments: payee_storage.clone(),
                events: payee_storage.clone(),
                protocols: payee_protocols,
                providers,
                material_protector: protector.clone(),
                authorizations: receipt_registry.clone(),
            },
            1,
        )
        .expect("payee settlement handler"),
    );

    let now = time::OffsetDateTime::now_utc().unix_timestamp().saturating_add(1);
    let run_id = "run-x402-solana-handler-live";
    let request_hash = "cd".repeat(32);
    let issued = payee_handler
        .issue_challenge(
            "principal:solana-payee",
            &IssuePaymentChallenge {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                offer_id: "offer-x402-solana-handler-live".to_owned(),
                resource: resource.to_owned(),
                origin: "https://marketpulse.local".to_owned(),
                protocol: X402_PROTOCOL.to_owned(),
                request_hash: request_hash.clone(),
            },
            now,
        )
        .await
        .expect("payee issues durable x402 Solana challenge");
    let challenge = payer_protocols
        .get(X402_PROTOCOL)
        .expect("payer x402 protocol")
        .parse_challenge(
            scenario_id,
            resource,
            &issued.payload["payment_required"],
            now,
            Some(issued.expires_at),
        )
        .await
        .expect("payer parses x402 Solana challenge");
    let authorization = payer_handler
        .authorize(
            "principal:solana-payer",
            &crate::handler::AuthorizePayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                challenge: challenge.clone(),
            },
            now.saturating_add(1),
        )
        .await
        .expect("payer authorizes x402 Solana intent");
    let intent_id = match authorization {
        PaymentAuthorizationResult::Authorized { intent_id, .. } => intent_id,
        other => panic!("x402 Solana handler E2E was not authorized: {other:?}"),
    };
    let signed = signing_handler
        .sign(
            "principal:solana-payer",
            &SignAuthorizedPayment {
                intent_id: intent_id.clone(),
                challenge,
            },
            now.saturating_add(2),
        )
        .await
        .expect("Vault custody signs x402 Solana payment");
    assert_eq!(signed.intent.status, PaymentStatus::Signed);
    assert_eq!(signed.intent.run_id, run_id);

    let verification = payee_handler
        .verify_payment(
            "principal:solana-payee",
            &VerifyIncomingPayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                challenge_hash: issued.challenge_hash,
                request_hash,
                credential: signed.credential,
            },
            now.saturating_add(3),
        )
        .await
        .expect("payee verifies x402 Solana credential through facilitator");
    assert!(verification.valid);
    let payer_before = fixture.token_balance(&fixture.payer_token_account).await;
    let recipient_before = fixture.token_balance(&fixture.recipient_token_account).await;
    let payer_native_before = fixture.native_balance(&payer).await;
    let facilitator_native_before = fixture.native_balance(&facilitator).await;
    let settled = payee_handler
        .settle_payment(
            "principal:solana-payee",
            &SettleIncomingPayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                intent_id: verification.intent.id.clone(),
                verification_ref: verification.verification_ref.expect("verification reference"),
            },
            now.saturating_add(4),
        )
        .await
        .expect("payee settles x402 Solana through facilitator");
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let transaction = settled.receipt.tx_hash.clone().expect("Solana settlement transaction");
    let payer_after = fixture.token_balance(&fixture.payer_token_account).await;
    let recipient_after = fixture.token_balance(&fixture.recipient_token_account).await;
    let payer_native_after = fixture.native_balance(&payer).await;
    let facilitator_native_after = fixture.native_balance(&facilitator).await;
    let transaction_fee = fixture.transaction_fee(&transaction).await;
    assert_eq!(payer_before - payer_after, 1_000_000);
    assert_eq!(recipient_after - recipient_before, 1_000_000);
    assert_eq!(payer_before + recipient_before, payer_after + recipient_after);
    assert_eq!(payer_native_before, payer_native_after);
    assert_eq!(facilitator_native_before - facilitator_native_after, transaction_fee);

    let merchant_response = SettleResponse::Success {
        payer: payer.clone(),
        transaction: transaction.clone(),
        network: network.clone(),
    };
    let response_header =
        Base64Bytes::encode(serde_json::to_vec(&merchant_response).expect("settle response JSON")).to_string();
    let outbound = Arc::new(OutboundEvidenceHandler::new(OutboundEvidenceDependencies {
        scenarios: payer_scenarios,
        payments: payer_storage.clone(),
        events: payer_storage.clone(),
        protocols: payer_protocols,
        material_protector: protector,
        receipts: receipt_registry,
    }));
    let reported = outbound
        .record_merchant_response(
            "principal:solana-payer",
            &RecordOutboundPaymentResponse {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                intent_id: intent_id.clone(),
                protocol: X402_PROTOCOL.to_owned(),
                header_name: "payment-response".to_owned(),
                header_value: response_header,
            },
            now.saturating_add(5),
        )
        .await
        .expect("payer stores untrusted x402 merchant response");
    assert!(reported.accepted);
    assert_eq!(reported.intent.status, PaymentStatus::PendingSettlement);

    let provider_evidence: Arc<dyn ReconciliationEvidenceSource> = payee_handler;
    let evidence: Arc<dyn ReconciliationEvidenceSource> =
        Arc::new(PaymentEvidenceRouter::new(provider_evidence, outbound));
    let reconciliation_clock = Arc::new(LiveSolanaClock::new(now.saturating_add(20)));
    let job = ReconciliationJob::with_clock(
        payer_storage.clone(),
        evidence,
        reconciliation_clock.clone(),
        ReconciliationConfig {
            worker_id: "x402-solana-handler-live".to_owned(),
            batch_size: 10,
            lease_duration: Duration::from_secs(10),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(8),
        },
    )
    .expect("payer reconciliation job");
    let mut reconciled = job.run_once().await.expect("reconcile x402 Solana receipt");
    for _ in 0..10 {
        if reconciled.settled == 1 {
            break;
        }
        reconciliation_clock.advance(2);
        tokio::time::sleep(Duration::from_millis(250)).await;
        reconciled = job.run_once().await.expect("retry x402 Solana receipt reconciliation");
    }
    assert_eq!(
        reconciled.settled, 1,
        "unexpected reconciliation result: {reconciled:?}"
    );
    let payer_intent = PaymentStorage::get(payer_storage.as_ref(), &intent_id)
        .await
        .expect("load payer intent")
        .expect("payer intent exists");
    assert_eq!(payer_intent.status, PaymentStatus::Settled);
    assert_eq!(payer_intent.run_id, run_id);
    assert_eq!(payer_intent.tx_hash.as_deref(), Some(transaction.as_str()));

    let payer_ledger = LedgerStorage::list(
        payer_storage.as_ref(),
        &LedgerQuery {
            intent_id: Some(intent_id.clone()),
            ..LedgerQuery::default()
        },
    )
    .await
    .expect("payer ledger");
    let payee_ledger = LedgerStorage::list(
        payee_storage.as_ref(),
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
    let payer_events = EventStorage::list_for_intent(
        payer_storage.as_ref(),
        &intent_id,
        PageRequest { offset: 0, limit: 100 },
    )
    .await
    .expect("payer events");
    let payee_events = EventStorage::list_for_intent(
        payee_storage.as_ref(),
        &settled.intent.id,
        PageRequest { offset: 0, limit: 100 },
    )
    .await
    .expect("payee events");
    assert!(payer_events
        .items
        .iter()
        .any(|event| event.event_type == PaymentEventType::Signed));
    assert!(payer_events
        .items
        .iter()
        .any(|event| event.event_type == PaymentEventType::Reconciled));
    assert_eq!(
        payee_events
            .items
            .iter()
            .filter(|event| event.event_type == PaymentEventType::SettlementRequested)
            .count(),
        1
    );

    println!(
        "X402_SOLANA_EVIDENCE={}",
        json!({
            "version": 2,
            "scheme": "exact",
            "network": network,
            "transaction": transaction,
            "payer": payer,
            "recipient": fixture.recipient,
            "mint": fixture.mint,
            "amount": 1_000_000,
            "payer_token_before": payer_before,
            "payer_token_after": payer_after,
            "recipient_token_before": recipient_before,
            "recipient_token_after": recipient_after,
            "payer_native_before": payer_native_before,
            "payer_native_after": payer_native_after,
            "facilitator_native_before": facilitator_native_before,
            "facilitator_native_after": facilitator_native_after,
            "transaction_fee": transaction_fee,
            "verify_before_settle": true,
            "broadcast_count": broadcasts.load(std::sync::atomic::Ordering::SeqCst),
            "vault_protocol_adapter": true,
            "vault_custody_signing": true,
            "vault_handler_roundtrip": true,
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
