use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::PaymentChallenge;
use crate::model::PaymentClient;
use crate::model::PaymentDirection;
use crate::model::PaymentEvent;
use crate::model::PaymentEventType;
use crate::model::PaymentIntent;
use crate::model::PaymentPolicy;
use crate::model::PaymentPrice;
use crate::model::PaymentScenario;
use crate::model::PaymentStatus;
use crate::model::ReservationStatus;
use crate::protocol::ProtocolSigningRequest;
use crate::storage::payment::postgres::PaymentPostgresRepository;
use crate::storage::postgres::PostgresRepository;
use crate::storage::wallet::postgres::PgWalletKeyRepository;
use crate::storage::wallet::postgres::WalletPostgresRepository;
use crate::storage::AuthorizationAction;
use crate::storage::AuthorizationGuard;
use crate::storage::AuthorizationRecord;
use crate::storage::AuthorizationWrite;
use crate::storage::ChallengeIssuanceGuard;
use crate::storage::ChallengeWrite;
use crate::storage::EncryptedWalletKeyVersion;
use crate::storage::IIntentRepository as _;
use crate::storage::IWalletRepository as _;
use crate::storage::IdempotentWrite;
use crate::storage::IntentBoundPaymentSignatureRequest;
use crate::storage::PaymentWallet;
use crate::storage::WalletPaymentSignature;
use crate::storage::WalletPaymentSigner as _;
use crate::Error;
use k256::ecdsa::RecoveryId;
use k256::ecdsa::Signature;
use k256::ecdsa::VerifyingKey;
use sha3::Digest as _;
use sha3::Keccak256;
use std::time::Duration;
use uuid::Uuid;

const TEST_URL_ENV: &str = "AGENTVAULT_TEST_POSTGRES_URL";

#[test]
fn rejects_jdbc_urls_before_creating_a_pool() {
    let result = PostgresRepository::from_config(
        "jdbc:postgresql://127.0.0.1:5432/agentvault",
        8,
        Duration::from_secs(10),
    );

    assert!(matches!(
        result,
        Err(Error::Config(message))
            if message == "PostgreSQL storage URL must use postgres:// or postgresql://; JDBC URLs require a JVM driver"
    ));
}

pub(crate) async fn storage() -> PostgresRepository {
    let url = std::env::var(TEST_URL_ENV).expect("set AGENTVAULT_TEST_POSTGRES_URL to a dedicated test database");
    PostgresRepository::connect(&url, 8, Duration::from_secs(10))
        .await
        .expect("connect dedicated PostgreSQL test database")
}

pub(crate) fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

pub(crate) fn unique_key(prefix: &str) -> [u8; 32] {
    Keccak256::digest(unique(prefix).as_bytes()).into()
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_expiring_inbound_challenge(
    storage: &PostgresRepository,
    client: &PaymentClient,
    scenario: &PaymentScenario,
    policy: &PaymentPolicy,
    price: &PaymentPrice,
    wallet: &PaymentWallet,
    created_at: i64,
    expires_at: i64,
) -> PaymentIntent {
    let challenge = PaymentChallenge {
        scenario_id: scenario.id.clone(),
        price_id: Some(price.id.clone()),
        resource: price.resource.clone(),
        protocol: "x402".to_owned(),
        asset: price.asset.clone(),
        amount: price.amount,
        recipient: wallet.wallet.address.clone(),
        nonce: unique("pg-expiring-nonce"),
        challenge_hash: unique("pg-expiring-challenge"),
        expires_at,
        payload: serde_json::json!({"scheme": "exact"}),
    };
    let intent = PaymentIntent {
        id: unique("pg-expiring-intent"),
        scenario_id: scenario.id.clone(),
        run_id: unique("pg-expiring-run"),
        wallet_id: Some(wallet.wallet.id.clone()),
        policy_id: Some(policy.id.clone()),
        price_id: Some(price.id.clone()),
        direction: PaymentDirection::Inbound,
        protocol: challenge.protocol.clone(),
        asset: challenge.asset.clone(),
        recipient: challenge.recipient.clone(),
        amount: challenge.amount,
        settled_amount: Amount::ZERO,
        resource: challenge.resource.clone(),
        challenge_hash: Some(challenge.challenge_hash.clone()),
        request_hash: unique("pg-expiring-request"),
        status: PaymentStatus::Created,
        reservation_status: ReservationStatus::Reserved,
        reserved_amount: challenge.amount,
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at,
        updated_at: created_at,
        expires_at: Some(expires_at),
        settled_at: None,
    };
    let event = PaymentEvent {
        id: unique("pg-expiring-created"),
        intent_id: intent.id.clone(),
        event_type: PaymentEventType::ChallengeCreated,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({"challenge": challenge.clone()}),
        created_at,
    };
    let guard = ChallengeIssuanceGuard {
        principal: client.auth_principal.clone(),
        client: client.clone(),
        scenario: scenario.clone(),
        price: price.clone(),
        policy: policy.clone(),
        wallet: wallet.clone(),
        origin: "https://feed.example".to_owned(),
        day_started_at: 0,
        evaluated_at: created_at,
    };
    let persisted = PaymentPostgresRepository::new(storage.clone())
        .record_challenge(ChallengeWrite {
            intent: &intent,
            challenge: &challenge,
            guard: &guard,
            events: std::slice::from_ref(&event),
            outbox: &[],
        })
        .await
        .unwrap();
    match persisted {
        IdempotentWrite::Inserted(intent) => intent,
        IdempotentWrite::Replay(_) => panic!("fresh challenge unexpectedly replayed"),
    }
}

pub(crate) fn failed_event(intent: &PaymentIntent, stage: &str) -> PaymentEvent {
    PaymentEvent {
        id: unique("pg-expired-event"),
        intent_id: intent.id.clone(),
        event_type: PaymentEventType::Failed,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({"stage": stage}),
        created_at: intent.updated_at,
    }
}

pub(crate) async fn sign_fixture(
    repository: &PgWalletKeyRepository,
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
    storage: &PostgresRepository,
    client: &PaymentClient,
    scenario: &PaymentScenario,
    policy: &PaymentPolicy,
    intent: &PaymentIntent,
) -> crate::Result<IdempotentWrite<AuthorizationRecord>> {
    let wallet = WalletPostgresRepository::new(storage.clone())
        .get_payment_wallet(intent.wallet_id.as_deref().unwrap())
        .await?
        .ok_or_else(|| Error::NotFound("test payment wallet".to_owned()))?;
    let challenge = signing_challenge(intent);
    let event = PaymentEvent {
        id: unique("pg-challenge-received"),
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
    PaymentPostgresRepository::new(storage.clone())
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
        price_id: None,
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
        price_id: None,
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
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_URL pointing at a dedicated PostgreSQL database"]
async fn entity_macros_preserve_values_and_reject_unsafe_mutations() {
    let database = storage().await;
    crate::storage::schema_tests::entity_crud_contract!(
        &database, PostgresRepository,
        "CREATE TABLE IF NOT EXISTS repository_macro_contract (id TEXT PRIMARY KEY, label TEXT NOT NULL, enabled BOOLEAN NOT NULL, count BIGINT NOT NULL, optional_text TEXT, payload JSONB NOT NULL, amount pay_uint128 NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL)"
    );
}

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL test database"]
async fn base_accepts_unrelated_schema_and_rolls_back_cancelled_work() {
    let url = std::env::var(TEST_URL_ENV).unwrap();
    let database = PostgresRepository::from_config(&url, 2, Duration::from_secs(5)).unwrap();
    crate::storage::schema_tests::independent_catalog_contract!(
        database, PostgresRepository, transaction,
        "CREATE TABLE repository_catalog_version (version INTEGER NOT NULL); INSERT INTO repository_catalog_version VALUES (7); CREATE TABLE repository_catalog_book (id TEXT PRIMARY KEY, title TEXT NOT NULL, copies BIGINT NOT NULL, available BOOLEAN NOT NULL, borrowed_at BIGINT)"
    );
}
