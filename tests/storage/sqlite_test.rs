use crate::config::SqliteStorageConfig;
use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::PaymentChallenge;
use crate::model::PaymentClient;
use crate::model::PaymentDirection;
use crate::model::PaymentEvent;
use crate::model::PaymentEventType;
use crate::model::PaymentIntent;
use crate::model::PaymentPolicy;
use crate::model::PaymentScenario;
use crate::model::PaymentStatus;
use crate::model::ReservationStatus;
use crate::protocol::ProtocolSigningRequest;
use crate::storage::payment::sqlite::PaymentSqliteRepository;
use crate::storage::scenario::sqlite::ScenarioSqliteRepository;
use crate::storage::sqlite::SqliteStorage;
use crate::storage::wallet::sqlite::SqliteWalletKeyRepository;
use crate::storage::wallet::sqlite::WalletSqliteRepository;
use crate::storage::AuthorizationAction;
use crate::storage::AuthorizationGuard;
use crate::storage::AuthorizationRecord;
use crate::storage::AuthorizationWrite;
use crate::storage::EncryptedWalletKeyVersion;
use crate::storage::IdempotentWrite;
use crate::storage::IntentBoundPaymentSignatureRequest;
use crate::storage::PaymentStorage as _;
use crate::storage::PaymentWallet;
use crate::storage::WalletPaymentSignature;
use crate::storage::WalletPaymentSigner as _;
use crate::storage::WalletStorage as _;
use crate::Error;
use k256::ecdsa::RecoveryId;
use k256::ecdsa::Signature;
use k256::ecdsa::VerifyingKey;
use sha3::Digest as _;
use sha3::Keccak256;
use std::time::Duration;
use tempfile::TempDir;

pub(crate) async fn storage() -> (TempDir, SqliteStorage) {
    let directory = TempDir::new().unwrap();
    let config = SqliteStorageConfig {
        database_file: directory.path().join("agentvault.sqlite3").display().to_string(),
        max_connections: 4,
        busy_timeout: Duration::from_secs(5),
    };
    let storage = SqliteStorage::connect(&config).await.unwrap();
    (directory, storage)
}

#[tokio::test]
async fn migrates_schema_and_enforces_foreign_keys_on_every_connection() {
    let (_directory, storage) = storage().await;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(storage.pool())
        .await
        .unwrap();
    assert_eq!(version, 2);

    let mut connections = Vec::new();
    for _ in 0..4 {
        connections.push(storage.pool().acquire().await.unwrap());
    }
    for mut connection in connections {
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(enabled, 1);
    }
}

pub(crate) async fn assert_invalid_wallet_rolls_back(
    storage: &SqliteStorage,
    scenario: &PaymentScenario,
    policy: &PaymentPolicy,
) {
    let invalid_scenario = PaymentScenario {
        id: "scenario-rollback".to_owned(),
        created_at: 3,
        updated_at: 3,
        ..scenario.clone()
    };
    let invalid_policy = PaymentPolicy {
        id: "policy-rollback".to_owned(),
        scenario_id: invalid_scenario.id.clone(),
        wallet_ids: vec!["wallet-does-not-exist".to_owned()],
        created_at: 3,
        updated_at: 3,
        ..policy.clone()
    };
    assert!(crate::storage::ScenarioStorage::register_with_policy(
        &ScenarioSqliteRepository::new(storage.clone()),
        &invalid_scenario,
        &invalid_policy
    )
    .await
    .is_err());
    assert!(crate::storage::ScenarioStorage::get(
        &ScenarioSqliteRepository::new(storage.clone()),
        &invalid_scenario.id
    )
    .await
    .unwrap()
    .is_none());
}

pub(crate) async fn sign_fixture(
    repository: &SqliteWalletKeyRepository,
    wallet: &PaymentWallet,
    key: &EncryptedWalletKeyVersion,
    digest: &[u8; 32],
    id: &str,
) -> crate::Result<WalletPaymentSignature> {
    let asset = AssetIdentity {
        network: "eip155:8453".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    };
    let intent = intent(
        id,
        &format!("key-{id}"),
        "scenario-signing-test",
        &wallet.wallet.id,
        "policy-signing-test",
        &asset,
        "0x2222222222222222222222222222222222222222",
    );
    let challenge = signing_challenge(&intent);
    let protocol_request = ProtocolSigningRequest {
        digest: digest.to_vec(),
        payload: serde_json::json!({"test": "intent-bound"}),
    };
    repository
        .sign_payment(IntentBoundPaymentSignatureRequest::new(
            &intent,
            &challenge,
            wallet,
            key,
            &protocol_request,
        ))
        .await
}

pub(crate) async fn reserve_authoritatively(
    storage: &SqliteStorage,
    client: &PaymentClient,
    scenario: &PaymentScenario,
    policy: &PaymentPolicy,
    intent: &PaymentIntent,
) -> crate::Result<IdempotentWrite<AuthorizationRecord>> {
    let wallet = WalletSqliteRepository::new(storage.clone())
        .get_payment_wallet(intent.wallet_id.as_deref().unwrap())
        .await?
        .ok_or_else(|| Error::NotFound("test payment wallet".to_owned()))?;
    let challenge = signing_challenge(intent);
    let event = PaymentEvent {
        id: format!("event-challenge-{}", intent.id),
        intent_id: intent.id.clone(),
        event_type: PaymentEventType::ChallengeReceived,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({
            "challenge_hash": challenge.challenge_hash,
            "challenge": challenge,
        }),
        created_at: intent.created_at,
    };
    let guard = AuthorizationGuard {
        principal: client.auth_principal.clone(),
        client: client.clone(),
        scenario: scenario.clone(),
        policy: policy.clone(),
        expected_wallet_updated_at: wallet.updated_at,
        expected_wallet_key_version: wallet.active_key_version,
        origin: "https://feed.example".to_owned(),
        day_started_at: intent.created_at - intent.created_at.rem_euclid(86_400),
        evaluated_at: intent.created_at,
    };
    PaymentSqliteRepository::new(storage.clone())
        .authorize(AuthorizationWrite {
            intent,
            approval: None,
            action: AuthorizationAction::Reserve,
            guard: &guard,
            events: std::slice::from_ref(&event),
            outbox: &[],
        })
        .await
}

pub(crate) fn signing_challenge(intent: &PaymentIntent) -> PaymentChallenge {
    PaymentChallenge {
        scenario_id: intent.scenario_id.clone(),
        offer_id: None,
        resource: intent.resource.clone(),
        protocol: intent.protocol.clone(),
        asset: intent.asset.clone(),
        amount: intent.amount,
        recipient: intent.recipient.clone(),
        nonce: format!("nonce-{}", intent.id),
        challenge_hash: intent.challenge_hash.clone().unwrap(),
        expires_at: intent.expires_at.unwrap(),
        payload: serde_json::json!({"test": "intent-bound"}),
    }
}

pub(crate) fn recover_address(bytes: &[u8], digest: &[u8; 32]) -> String {
    let signature = Signature::try_from(&bytes[..64]).unwrap();
    let recovery = RecoveryId::try_from(bytes[64].checked_sub(27).unwrap()).unwrap();
    let verifying_key = VerifyingKey::recover_from_prehash(digest, &signature, recovery).unwrap();
    let encoded = verifying_key.to_encoded_point(false);
    let hash = Keccak256::digest(&encoded.as_bytes()[1..]);
    format!("0x{}", hex::encode(&hash[12..]))
}

pub(crate) fn intent(
    id: &str,
    run_id: &str,
    scenario_id: &str,
    wallet_id: &str,
    policy_id: &str,
    asset: &AssetIdentity,
    recipient: &str,
) -> PaymentIntent {
    PaymentIntent {
        id: id.to_owned(),
        scenario_id: scenario_id.to_owned(),
        run_id: run_id.to_owned(),
        wallet_id: Some(wallet_id.to_owned()),
        policy_id: Some(policy_id.to_owned()),
        offer_id: None,
        direction: PaymentDirection::Outbound,
        protocol: "x402".to_owned(),
        asset: asset.clone(),
        recipient: recipient.to_owned(),
        amount: Amount::new(60),
        settled_amount: Amount::ZERO,
        resource: "https://feed.example/mcp/realtime".to_owned(),
        challenge_hash: Some(format!("challenge-{id}")),
        request_hash: format!("request-{id}"),
        status: PaymentStatus::Authorized,
        reservation_status: ReservationStatus::Reserved,
        reserved_amount: Amount::new(60),
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at: 100,
        updated_at: 100,
        expires_at: Some(1_000),
        settled_at: None,
    }
}

#[tokio::test]
async fn entity_macros_preserve_values_and_reject_unsafe_mutations() {
    let (_directory, database) = storage().await;
    crate::storage::schema_tests::entity_crud_contract!(
        &database, SqliteStorage,
        "CREATE TABLE repository_macro_contract (id TEXT PRIMARY KEY, label TEXT NOT NULL, enabled BOOLEAN NOT NULL CHECK(enabled IN (0,1)), count INTEGER NOT NULL, optional_text TEXT, payload TEXT NOT NULL, amount TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)"
    );
}

#[tokio::test]
async fn base_accepts_unrelated_schema_and_rolls_back_cancelled_work() {
    let directory = TempDir::new().unwrap();
    let config = SqliteStorageConfig {
        database_file: directory.path().join("catalog.sqlite3").display().to_string(),
        max_connections: 2,
        busy_timeout: Duration::from_secs(5),
    };
    let database = SqliteStorage::from_config(&config).unwrap();
    crate::storage::schema_tests::independent_catalog_contract!(
        database, SqliteStorage, immediate,
        "CREATE TABLE repository_catalog_version (version INTEGER NOT NULL); INSERT INTO repository_catalog_version VALUES (7); CREATE TABLE repository_catalog_book (id TEXT PRIMARY KEY, title TEXT NOT NULL, copies INTEGER NOT NULL, available BOOLEAN NOT NULL, borrowed_at INTEGER)"
    );
}
