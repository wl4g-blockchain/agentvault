use std::{sync::Arc, time::Duration};

use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use sha3::{Digest as _, Keccak256};
use tokio::sync::Barrier;
use uuid::Uuid;

use crate::{
    model::{
        Amount, AssetIdentity, BudgetUnit, LedgerEntryType, PaymentChallenge, PaymentClient, PaymentCredential,
        PaymentDirection, PaymentEvent, PaymentEventType, PaymentIntent, PaymentLedgerEntry, PaymentOffer,
        PaymentPolicy, PaymentPolicyLimits, PaymentScenario, PaymentStatus, ReservationStatus,
    },
    protocol::ProtocolSigningRequest,
    storage::{
        AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, ChallengeIssuanceGuard,
        ChallengeWrite, EncryptedWalletKeyVersion, IdempotentWrite, IntentBoundPaymentSignatureRequest,
        OutboundSigningRead, OutboxMessage, OutboxStatus, OutboxStorage as _, PaymentProgressWrite, PaymentQuery,
        PaymentStorage as _, PaymentWallet, ReconciliationLeaseRequest, SettlementWrite, SigningGuard,
        SigningStorage as _, SigningWrite, VerificationWrite, WalletKeyStatus, WalletPaymentSignature,
        WalletPaymentSigner as _, WalletStorage as _,
    },
    utils::{
        aead::{parse_uuid_nonce, WALLET_KEY_ENCRYPTION_SCHEME},
        secure_material::SealedSensitiveMaterial,
    },
    Error,
};

use super::{key_repository::PgWalletKeyRepository, PostgresStorage};

const TEST_DSN_ENV: &str = "AGENTVAULT_TEST_POSTGRES_DSN";

async fn storage() -> PostgresStorage {
    let dsn = std::env::var(TEST_DSN_ENV).expect("set AGENTVAULT_TEST_POSTGRES_DSN to a dedicated test database");
    PostgresStorage::connect(&dsn, 8, Duration::from_secs(10))
        .await
        .expect("connect dedicated PostgreSQL test database")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

fn unique_key(prefix: &str) -> [u8; 32] {
    Keccak256::digest(unique(prefix).as_bytes()).into()
}

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_DSN pointing at a dedicated PostgreSQL database"]
async fn migrates_schema_and_signs_only_through_intent_bound_active_keys() {
    let storage = storage().await;
    let version: i32 = sqlx::query_scalar("SELECT version FROM pay_schema_version")
        .fetch_one(storage.pool())
        .await
        .unwrap();
    assert_eq!(version, 1);

    let repository = PgWalletKeyRepository::from_resolved_keys(
        storage.clone(),
        "test-kek-v1",
        [("test-kek-v1".to_owned(), [9_u8; 32])],
    )
    .unwrap();
    let wallet_id = unique("pg-key-wallet");
    let first = unique_key("pg-key-v1");
    let first_wallet = repository.create_local(&wallet_id, &first, 10).await.unwrap();
    let first_payment_wallet = storage.get_payment_wallet(&wallet_id).await.unwrap().unwrap();
    let first_key = storage.load_active_key(&wallet_id).await.unwrap().unwrap();
    assert_eq!(first_key.version, 1);
    assert_eq!(first_key.status, WalletKeyStatus::Active);
    assert_eq!(first_key.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    assert!(parse_uuid_nonce(&first_key.wrap_nonce).is_ok());
    assert!(parse_uuid_nonce(&first_key.key_nonce).is_ok());
    let first_digest = [0x41_u8; 32];
    let first_signature = sign_fixture(
        &repository,
        &first_payment_wallet,
        &first_key,
        &first_digest,
        &unique("pg-key-v1"),
    )
    .await
    .unwrap();
    assert_eq!(
        recover_address(&first_signature.bytes, &first_digest),
        first_wallet.address
    );

    let second = unique_key("pg-key-v2");
    let rotated = repository.rotate(&wallet_id, &second, 20).await.unwrap();
    assert_ne!(rotated.address, first_wallet.address);
    let rotated_payment_wallet = storage.get_payment_wallet(&wallet_id).await.unwrap().unwrap();
    let active_key = storage.load_active_key(&wallet_id).await.unwrap().unwrap();
    assert_eq!(active_key.version, 2);
    assert_eq!(active_key.status, WalletKeyStatus::Active);
    assert!(matches!(
        sign_fixture(
            &repository,
            &rotated_payment_wallet,
            &first_key,
            &[0x42_u8; 32],
            &unique("pg-stale-key"),
        )
        .await,
        Err(Error::PaymentConflict(_))
    ));
    let second_digest = [0x43_u8; 32];
    let second_signature = sign_fixture(
        &repository,
        &rotated_payment_wallet,
        &active_key,
        &second_digest,
        &unique("pg-key-v2"),
    )
    .await
    .unwrap();
    assert_eq!(
        recover_address(&second_signature.bytes, &second_digest),
        rotated.address
    );
    let versions = storage.list_key_versions(&wallet_id).await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].status, WalletKeyStatus::Retired);
    assert_eq!(versions[1].status, WalletKeyStatus::Active);
    assert_ne!(versions[0].key_ciphertext, first);
}

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_DSN pointing at a dedicated PostgreSQL database"]
async fn outbox_class_claim_isolated_and_replayed_after_worker_restart() {
    let database = storage().await;
    let approval_id = unique("pg-approval-outbox");
    let payment_id = unique("pg-payment-outbox");
    let foreign_id = unique("pg-foreign-outbox");
    for (id, event_type, destination) in [
        (approval_id.as_str(), "approval_required", "client:client-1.approvals"),
        (payment_id.as_str(), "payment_settled", "scenario:scenario-1.payments"),
        (foreign_id.as_str(), "webhook_event", "webhook:external"),
    ] {
        sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
             correlation_id, run_id, payload, status, attempt_count, available_at, created_at) \
             VALUES ($1, 'payment_intent', 'intent-1', $2, $3, 'intent-1', $1, '{}'::jsonb, 'pending', 0, 10, 10)",
        )
        .bind(id)
        .bind(event_type)
        .bind(destination)
        .execute(database.pool())
        .await
        .unwrap();
    }

    let approval = database
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-approval-worker".to_owned(),
            class: crate::storage::OutboxDeliveryClass::ApprovalRequired,
            now: 10,
            lease_until: 20,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(approval.iter().any(|message| message.id == approval_id));
    assert!(!approval.iter().any(|message| message.id == payment_id));
    let untouched: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = $1")
            .bind(&payment_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(untouched, (0, None));

    drop(database);
    let reopened = storage().await;
    let payment = reopened
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-payment-worker".to_owned(),
            class: crate::storage::OutboxDeliveryClass::PaymentEvents,
            now: 21,
            lease_until: 31,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(payment.iter().any(|message| message.id == payment_id));
    assert!(!payment.iter().any(|message| message.id == approval_id));
    let replay = reopened
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-approval-worker-restarted".to_owned(),
            class: crate::storage::OutboxDeliveryClass::ApprovalRequired,
            now: 21,
            lease_until: 31,
            limit: 100,
        })
        .await
        .unwrap();
    let replay = replay.iter().find(|message| message.id == approval_id).unwrap();
    assert_eq!(replay.attempt_count, 2);
    let foreign: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = $1")
            .bind(&foreign_id)
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(foreign, (0, None));
}

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_DSN pointing at a dedicated PostgreSQL database"]
#[allow(clippy::too_many_lines)]
async fn serializes_budget_and_commits_settlement_ledger_and_outbox_atomically() {
    let storage = storage().await;
    let repository = PgWalletKeyRepository::from_resolved_keys(
        storage.clone(),
        "test-kek-v1",
        [("test-kek-v1".to_owned(), [4_u8; 32])],
    )
    .unwrap();
    let wallet_key = unique_key("pg-budget-wallet-key");
    let wallet = repository
        .create_local(&unique("pg-budget-wallet"), &wallet_key, 1)
        .await
        .unwrap();
    let unit = BudgetUnit {
        asset: "USDC".to_owned(),
        decimals: 6,
    };
    storage
        .set_spending_limits(&wallet.id, &unit, Some(Amount::new(100)), Some(Amount::new(100)), 2)
        .await
        .unwrap();
    let alternate_wallet_key = unique_key("pg-budget-wallet-b-key");
    let alternate_wallet = repository
        .create_local(&unique("pg-budget-wallet-b"), &alternate_wallet_key, 1)
        .await
        .unwrap();
    storage
        .set_spending_limits(
            &alternate_wallet.id,
            &unit,
            Some(Amount::new(100)),
            Some(Amount::new(100)),
            2,
        )
        .await
        .unwrap();
    let client = PaymentClient {
        id: unique("pg-client"),
        name: "PostgreSQL contract client".to_owned(),
        auth_principal: unique("principal:pg"),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::ClientStorage::insert(&storage, &client).await.unwrap();
    let scenario = PaymentScenario {
        id: unique("pg-scenario"),
        client_id: client.id.clone(),
        external_ref: unique("pg-news-ingestion"),
        name: "PostgreSQL news ingestion".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::ScenarioStorage::register(&storage, &scenario)
        .await
        .unwrap();
    let asset = AssetIdentity {
        network: "eip155:8453".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    };
    let recipient = "0x2222222222222222222222222222222222222222".to_owned();
    let policy = PaymentPolicy {
        id: unique("pg-policy"),
        scenario_id: scenario.id.clone(),
        wallet_ids: vec![wallet.id.clone()],
        protocols: vec!["x402".to_owned()],
        networks: vec![asset.network.clone()],
        assets: vec![asset.clone()],
        budget_unit: unit.clone(),
        allowed_recipients: vec![recipient.clone()],
        allowed_origins: vec!["https://feed.example".to_owned()],
        inbound_limits: Some(PaymentPolicyLimits {
            max_per_payment: Amount::new(100),
            max_per_day: Amount::new(100),
            approval_threshold: None,
            approval_enabled: false,
            approval_overage: None,
        }),
        outbound_limits: Some(PaymentPolicyLimits {
            max_per_payment: Amount::new(100),
            max_per_day: Amount::new(100),
            approval_threshold: None,
            approval_enabled: true,
            approval_overage: None,
        }),
        enabled: true,
        created_at: 3,
        updated_at: 3,
    };
    crate::storage::PolicyStorage::insert(&storage, &policy).await.unwrap();
    let mut alternate_policy = policy.clone();
    alternate_policy.id = unique("pg-policy-b");
    alternate_policy.wallet_ids = vec![alternate_wallet.id.clone()];
    alternate_policy.created_at = 4;
    alternate_policy.updated_at = 4;
    crate::storage::PolicyStorage::insert(&storage, &alternate_policy)
        .await
        .unwrap();

    let first = intent(
        &unique("pg-intent-a"),
        &unique("pg-idempotency-a"),
        &scenario.id,
        &wallet.id,
        &policy.id,
        &asset,
        &recipient,
    );
    let second = intent(
        &unique("pg-intent-b"),
        &unique("pg-idempotency-b"),
        &scenario.id,
        &alternate_wallet.id,
        &alternate_policy.id,
        &asset,
        &recipient,
    );
    let barrier = Arc::new(Barrier::new(3));
    let left_storage = storage.clone();
    let left_barrier = Arc::clone(&barrier);
    let left_client = client.clone();
    let left_scenario = scenario.clone();
    let left_policy = policy.clone();
    let left = tokio::spawn(async move {
        left_barrier.wait().await;
        reserve_authoritatively(&left_storage, &left_client, &left_scenario, &left_policy, &first).await
    });
    let right_storage = storage.clone();
    let right_barrier = Arc::clone(&barrier);
    let right_client = client.clone();
    let right_scenario = scenario.clone();
    let right_policy = alternate_policy.clone();
    let right = tokio::spawn(async move {
        right_barrier.wait().await;
        reserve_authoritatively(&right_storage, &right_client, &right_scenario, &right_policy, &second).await
    });
    barrier.wait().await;
    let outcomes = [left.await.unwrap(), right.await.unwrap()];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(Error::PolicyDenied(_))))
            .count(),
        1
    );

    let page = crate::storage::PaymentStorage::list(
        &storage,
        &PaymentQuery {
            scenario_id: Some(scenario.id.clone()),
            ..PaymentQuery::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.total, 1);
    let current = page.items[0].clone();
    assert!(matches!(
        reserve_authoritatively(&storage, &client, &scenario, &policy, &current)
            .await
            .unwrap(),
        IdempotentWrite::Replay(_)
    ));

    let challenge_event = crate::storage::EventStorage::find_for_intent_type(
        &storage,
        &current.id,
        PaymentEventType::ChallengeReceived,
        0,
    )
    .await
    .unwrap()
    .expect("authorization must atomically persist its challenge event");
    let challenge: PaymentChallenge = serde_json::from_value(
        challenge_event
            .payload
            .get("challenge")
            .cloned()
            .expect("challenge event must contain the bound challenge"),
    )
    .unwrap();
    let OutboundSigningRead::Authorized(authorization) = storage
        .load_for_principal(Some(&client.auth_principal), &current.id)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("reserved payment must be authorized for signing");
    };
    let mut signed = authorization.identity.intent.clone();
    signed.status = PaymentStatus::Signed;
    signed.updated_at += 1;
    signed.next_reconcile_at = Some(signed.updated_at + 1);
    let credential_hash = unique("pg-credential-hash");
    let sealed_credential = SealedSensitiveMaterial {
        version: 1,
        key_id: "test-material-key".to_owned(),
        nonce: "test-nonce".to_owned(),
        ciphertext: "test-ciphertext".to_owned(),
    };
    let signing_events = [
        PaymentEvent {
            id: unique("pg-sign-requested"),
            intent_id: signed.id.clone(),
            event_type: PaymentEventType::SignRequested,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: serde_json::json!({"challenge_hash": challenge.challenge_hash.clone()}),
            created_at: signed.updated_at,
        },
        PaymentEvent {
            id: unique("pg-signed"),
            intent_id: signed.id.clone(),
            event_type: PaymentEventType::Signed,
            attempt_no: 0,
            signature_ref: Some(credential_hash.clone()),
            tx_hash: None,
            payload: serde_json::json!({
                "credential_hash": credential_hash.clone(),
                "sealed_credential": sealed_credential.clone(),
            }),
            created_at: signed.updated_at,
        },
    ];
    let guard = SigningGuard {
        principal: client.auth_principal.clone(),
        client_id: client.id.clone(),
        expected_client_updated_at: client.updated_at,
        scenario_id: scenario.id.clone(),
        expected_scenario_updated_at: scenario.updated_at,
        policy_id: authorization.policy.id.clone(),
        expected_policy_updated_at: authorization.policy.updated_at,
        wallet_id: authorization.wallet.wallet.id.clone(),
        expected_wallet_updated_at: authorization.wallet.updated_at,
        expected_key_version: authorization.active_key.version,
        expected_intent_updated_at: current.updated_at,
        challenge_hash: challenge.challenge_hash.clone(),
        evaluated_at: current.created_at,
    };
    let signing_write = SigningWrite {
        intent: &signed,
        challenge: &challenge,
        credential_hash: &credential_hash,
        sealed_credential: &sealed_credential,
        guard: &guard,
        events: &signing_events,
        outbox: &[],
    };
    assert!(matches!(
        storage.commit_signed(signing_write).await.unwrap(),
        IdempotentWrite::Inserted(_)
    ));

    let submitted_at = signed.updated_at + 1;
    let submitted_rows = sqlx::query(
        "UPDATE pay_intent SET status = 'submitted', updated_at = $1 \
         WHERE id = $2 AND status = 'signed' AND updated_at = $3",
    )
    .bind(submitted_at)
    .bind(&signed.id)
    .bind(signed.updated_at)
    .execute(storage.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(submitted_rows, 1);
    let submitted = crate::storage::PaymentStorage::get(&storage, &signed.id)
        .await
        .unwrap()
        .unwrap();

    let mut pending = submitted.clone();
    pending.status = PaymentStatus::PendingSettlement;
    pending.updated_at += 1;
    pending.next_reconcile_at = Some(pending.updated_at + 5);
    let verified = PaymentEvent {
        id: unique("pg-verified"),
        intent_id: pending.id.clone(),
        event_type: PaymentEventType::CredentialVerified,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({"verification_ref": "pg-verify-only"}),
        created_at: pending.updated_at,
    };
    let mut invalid_release = pending.clone();
    invalid_release.reservation_status = ReservationStatus::Released;
    assert!(matches!(
        storage
            .commit_progress(PaymentProgressWrite {
                intent: &invalid_release,
                expected_updated_at: submitted.updated_at,
                events: std::slice::from_ref(&verified),
                outbox: &[],
            })
            .await,
        Err(Error::PaymentConflict(_))
    ));
    let unchanged = crate::storage::PaymentStorage::get(&storage, &signed.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.status, PaymentStatus::Submitted);
    assert_eq!(unchanged.reservation_status, ReservationStatus::Reserved);
    let current = storage
        .commit_progress(PaymentProgressWrite {
            intent: &pending,
            expected_updated_at: submitted.updated_at,
            events: std::slice::from_ref(&verified),
            outbox: &[],
        })
        .await
        .unwrap();
    assert_eq!(current.status, PaymentStatus::PendingSettlement);
    assert_eq!(current.reservation_status, ReservationStatus::Reserved);

    let mut settled = current.clone();
    settled.status = PaymentStatus::Settled;
    settled.reservation_status = ReservationStatus::Consumed;
    settled.settled_amount = settled.amount;
    settled.tx_hash = Some(unique("0xpg-settlement"));
    settled.updated_at += 1;
    settled.next_reconcile_at = None;
    settled.settled_at = Some(settled.updated_at);
    let ledger = PaymentLedgerEntry {
        id: unique("pg-ledger"),
        intent_id: Some(settled.id.clone()),
        scenario_id: Some(settled.scenario_id.clone()),
        wallet_id: settled.wallet_id.clone().unwrap(),
        entry_type: LedgerEntryType::Payment,
        direction: PaymentDirection::Outbound,
        asset: asset.clone(),
        amount: settled.amount,
        posting_key: unique("pg-chain-fact"),
        tx_hash: settled.tx_hash.clone(),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: None,
        effective_at: settled.updated_at,
        created_at: settled.updated_at,
    };
    let outbox = OutboxMessage {
        id: unique("pg-outbox"),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: settled.id.clone(),
        event_type: "settled".to_owned(),
        destination: format!("scenario:{}.payments", settled.scenario_id),
        correlation_id: settled.id.clone(),
        run_id: unique("pg-settled-notification"),
        payload: serde_json::json!({"intent_id": settled.id.clone()}),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        available_at: settled.updated_at,
        lease_owner: None,
        lease_expires_at: None,
        last_error: None,
        created_at: settled.updated_at,
        delivered_at: None,
    };
    storage
        .commit_settlement(SettlementWrite {
            intent: &settled,
            expected_updated_at: current.updated_at,
            events: &[],
            ledger_entries: std::slice::from_ref(&ledger),
            outbox: std::slice::from_ref(&outbox),
        })
        .await
        .unwrap();
    assert!(matches!(
        crate::storage::LedgerStorage::append(&storage, &ledger).await.unwrap(),
        IdempotentWrite::Replay(_)
    ));
    let claimed = storage
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-test-worker".to_owned(),
            class: crate::storage::OutboxDeliveryClass::PaymentEvents,
            now: settled.updated_at,
            lease_until: settled.updated_at + 30,
            limit: 10,
        })
        .await
        .unwrap();
    let claimed = claimed.iter().find(|message| message.id == outbox.id).unwrap();
    storage
        .mark_delivered(&claimed.id, "pg-test-worker", settled.updated_at + 1)
        .await
        .unwrap();
    let snapshot = storage
        .budget_snapshot(
            &scenario.id,
            settled.wallet_id.as_deref().unwrap(),
            PaymentDirection::Outbound,
            &unit,
            0,
            settled.updated_at,
        )
        .await
        .unwrap();
    assert_eq!(snapshot.scenario_spent_and_reserved, Amount::new(60));
    assert_eq!(snapshot.wallet_spent_and_reserved, Amount::new(60));
    let losing_wallet_id = if settled.wallet_id.as_deref() == Some(wallet.id.as_str()) {
        &alternate_wallet.id
    } else {
        &wallet.id
    };
    let losing_snapshot = storage
        .budget_snapshot(
            &scenario.id,
            losing_wallet_id,
            PaymentDirection::Outbound,
            &unit,
            0,
            settled.updated_at,
        )
        .await
        .unwrap();
    assert_eq!(losing_snapshot.scenario_spent_and_reserved, Amount::new(60));
    assert_eq!(losing_snapshot.wallet_spent_and_reserved, Amount::ZERO);

    let race_wallet_key = unique_key("pg-ledger-race-wallet-key");
    let race_wallet = repository
        .create_local(&unique("pg-ledger-race-wallet"), &race_wallet_key, 1)
        .await
        .unwrap();
    storage
        .set_spending_limits(
            &race_wallet.id,
            &unit,
            Some(Amount::new(100)),
            Some(Amount::new(100)),
            2,
        )
        .await
        .unwrap();
    let race_scenario = PaymentScenario {
        id: unique("pg-ledger-race-scenario"),
        client_id: client.id.clone(),
        external_ref: unique("pg-ledger-race"),
        name: "PostgreSQL ledger race".to_owned(),
        enabled: true,
        created_at: 10,
        updated_at: 10,
    };
    crate::storage::ScenarioStorage::register(&storage, &race_scenario)
        .await
        .unwrap();
    let mut race_policy = policy.clone();
    race_policy.id = unique("pg-ledger-race-policy");
    race_policy.scenario_id.clone_from(&race_scenario.id);
    race_policy.wallet_ids = vec![race_wallet.id.clone()];
    race_policy.created_at = 11;
    race_policy.updated_at = 11;
    crate::storage::PolicyStorage::insert(&storage, &race_policy)
        .await
        .unwrap();
    let race_intent = intent(
        &unique("pg-ledger-race-intent"),
        &unique("pg-ledger-race-idempotency"),
        &race_scenario.id,
        &race_wallet.id,
        &race_policy.id,
        &asset,
        &recipient,
    );
    let race_now = race_intent.created_at;
    let confirmed = PaymentLedgerEntry {
        id: unique("pg-ledger-race-entry"),
        intent_id: None,
        scenario_id: Some(race_scenario.id.clone()),
        wallet_id: race_wallet.id.clone(),
        entry_type: LedgerEntryType::Payment,
        direction: PaymentDirection::Outbound,
        asset: asset.clone(),
        amount: Amount::new(60),
        posting_key: unique("pg-ledger-race-fact"),
        tx_hash: Some(unique("0xpg-ledger-race")),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: None,
        effective_at: race_now,
        created_at: race_now,
    };
    let race_barrier = Arc::new(Barrier::new(3));
    let ledger_storage = storage.clone();
    let ledger_barrier = Arc::clone(&race_barrier);
    let ledger_scenario_id = race_scenario.id.clone();
    let ledger_wallet_id = race_wallet.id.clone();
    let ledger_entry = confirmed.clone();
    let ledger_task = tokio::spawn(async move {
        ledger_storage
            .transaction(|connection| {
                Box::pin(async move {
                    super::ledger::lock_financial_scopes(
                        connection,
                        std::slice::from_ref(&ledger_scenario_id),
                        std::slice::from_ref(&ledger_wallet_id),
                    )
                    .await?;
                    let inserted = super::ledger::insert_ledger(connection, &ledger_entry).await?;
                    ledger_barrier.wait().await;
                    Ok(inserted)
                })
            })
            .await
    });
    let reservation_storage = storage.clone();
    let reservation_barrier = Arc::clone(&race_barrier);
    let reservation_intent = race_intent.clone();
    let reservation_client = client.clone();
    let reservation_scenario = race_scenario.clone();
    let reservation_policy = race_policy.clone();
    let reservation_task = tokio::spawn(async move {
        reservation_barrier.wait().await;
        reserve_authoritatively(
            &reservation_storage,
            &reservation_client,
            &reservation_scenario,
            &reservation_policy,
            &reservation_intent,
        )
        .await
    });
    race_barrier.wait().await;
    assert!(matches!(
        ledger_task.await.unwrap().unwrap(),
        IdempotentWrite::Inserted(_)
    ));
    assert!(matches!(reservation_task.await.unwrap(), Err(Error::PolicyDenied(_))));
    let race_snapshot = storage
        .budget_snapshot(
            &race_scenario.id,
            &race_wallet.id,
            PaymentDirection::Outbound,
            &unit,
            0,
            race_now,
        )
        .await
        .unwrap();
    assert_eq!(race_snapshot.scenario_spent_and_reserved, Amount::new(60));
    assert_eq!(race_snapshot.wallet_spent_and_reserved, Amount::new(60));

    let offer = PaymentOffer {
        id: unique("pg-inbound-expiry-offer"),
        scenario_id: scenario.id.clone(),
        policy_id: policy.id.clone(),
        resource: format!("https://feed.example/mcp/expiry-{}", Uuid::new_v4()),
        protocols: vec!["x402".to_owned()],
        asset: asset.clone(),
        amount: Amount::new(20),
        wallet_id: wallet.id.clone(),
        enabled: true,
        created_at: 21,
        updated_at: 21,
    };
    crate::storage::OfferStorage::insert(&storage, &offer).await.unwrap();
    let payment_wallet = storage.get_payment_wallet(&wallet.id).await.unwrap().unwrap();
    let expired_candidate = record_expiring_inbound_challenge(
        &storage,
        &client,
        &scenario,
        &policy,
        &offer,
        &payment_wallet,
        2_000,
        2_010,
    )
    .await;
    let raced_candidate = record_expiring_inbound_challenge(
        &storage,
        &client,
        &scenario,
        &policy,
        &offer,
        &payment_wallet,
        2_001,
        2_010,
    )
    .await;
    let claimed = storage
        .claim_reconciliation(&ReconciliationLeaseRequest {
            owner: unique("pg-expiry-worker"),
            now: 2_010,
            lease_until: 2_040,
            limit: 1_000,
        })
        .await
        .unwrap();
    let claimed_expired = claimed
        .iter()
        .find(|intent| intent.id == expired_candidate.id)
        .cloned()
        .expect("due created challenge must be claimed");
    let claimed_raced = claimed
        .iter()
        .find(|intent| intent.id == raced_candidate.id)
        .cloned()
        .expect("second due created challenge must be claimed");

    let mut expired = claimed_expired.clone();
    expired.status = PaymentStatus::Expired;
    expired.reservation_status = ReservationStatus::Released;
    expired.next_reconcile_at = None;
    expired.updated_at += 1;
    let expired_event = failed_event(&expired, "challenge_expired");
    let expired = storage
        .commit_progress(PaymentProgressWrite {
            intent: &expired,
            expected_updated_at: claimed_expired.updated_at,
            events: std::slice::from_ref(&expired_event),
            outbox: &[],
        })
        .await
        .unwrap();
    assert_eq!(expired.status, PaymentStatus::Expired);
    assert_eq!(expired.reservation_status, ReservationStatus::Released);
    let after_expiry = storage
        .budget_snapshot(&scenario.id, &wallet.id, PaymentDirection::Inbound, &unit, 0, 2_010)
        .await
        .unwrap();
    assert_eq!(after_expiry.scenario_spent_and_reserved, offer.amount);
    assert_eq!(after_expiry.wallet_spent_and_reserved, offer.amount);

    let mut raced_expiry = claimed_raced.clone();
    raced_expiry.status = PaymentStatus::Expired;
    raced_expiry.reservation_status = ReservationStatus::Released;
    raced_expiry.next_reconcile_at = None;
    raced_expiry.updated_at += 1;
    let raced_expiry_event = failed_event(&raced_expiry, "challenge_expired_race");
    let mut submitted = claimed_raced.clone();
    submitted.status = PaymentStatus::Submitted;
    submitted.next_reconcile_at = Some(2_011);
    submitted.updated_at += 1;
    let credential = PaymentCredential {
        protocol: submitted.protocol.clone(),
        challenge_hash: submitted.challenge_hash.clone().unwrap(),
        credential_hash: unique("pg-raced-credential"),
        payload: serde_json::json!({"proof": "opaque"}),
    };
    let submitted_event = PaymentEvent {
        id: unique("pg-raced-submitted"),
        intent_id: submitted.id.clone(),
        event_type: PaymentEventType::Submitted,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({"credential_fingerprint": credential.credential_hash.clone()}),
        created_at: submitted.updated_at,
    };
    let transition_barrier = Arc::new(Barrier::new(3));
    let expiry_storage = storage.clone();
    let expiry_barrier = Arc::clone(&transition_barrier);
    let expected_updated_at = claimed_raced.updated_at;
    let expiry_task = tokio::spawn(async move {
        expiry_barrier.wait().await;
        expiry_storage
            .commit_progress(PaymentProgressWrite {
                intent: &raced_expiry,
                expected_updated_at,
                events: std::slice::from_ref(&raced_expiry_event),
                outbox: &[],
            })
            .await
    });
    let verify_storage = storage.clone();
    let verify_barrier = Arc::clone(&transition_barrier);
    let verify_task = tokio::spawn(async move {
        verify_barrier.wait().await;
        verify_storage
            .verify_incoming(VerificationWrite {
                intent: &submitted,
                credential: &credential,
                expected_updated_at,
                events: std::slice::from_ref(&submitted_event),
                outbox: &[],
            })
            .await
    });
    transition_barrier.wait().await;
    let transition_results = [expiry_task.await.unwrap(), verify_task.await.unwrap()];
    assert_eq!(
        transition_results.iter().filter(|result| result.is_ok()).count(),
        1,
        "exactly one expiry/credential transition must win: {transition_results:?}"
    );
    assert_eq!(
        transition_results
            .iter()
            .filter(|result| matches!(result, Err(Error::PaymentConflict(_))))
            .count(),
        1,
        "the losing expiry/credential transition must be a conflict: {transition_results:?}"
    );
    let raced = crate::storage::PaymentStorage::get(&storage, &claimed_raced.id)
        .await
        .unwrap()
        .unwrap();
    let bound_credential: Option<String> = sqlx::query_scalar("SELECT credential_hash FROM pay_intent WHERE id = $1")
        .bind(&raced.id)
        .fetch_one(storage.pool())
        .await
        .unwrap();
    match raced.status {
        PaymentStatus::Expired => {
            assert_eq!(raced.reservation_status, ReservationStatus::Released);
            assert!(bound_credential.is_none());
        }
        PaymentStatus::Submitted => {
            assert_eq!(raced.reservation_status, ReservationStatus::Reserved);
            assert!(bound_credential.is_some());
        }
        status => panic!("unexpected expiry/credential race winner: {status:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_expiring_inbound_challenge(
    storage: &PostgresStorage,
    client: &PaymentClient,
    scenario: &PaymentScenario,
    policy: &PaymentPolicy,
    offer: &PaymentOffer,
    wallet: &PaymentWallet,
    created_at: i64,
    expires_at: i64,
) -> PaymentIntent {
    let challenge = PaymentChallenge {
        scenario_id: scenario.id.clone(),
        offer_id: Some(offer.id.clone()),
        resource: offer.resource.clone(),
        protocol: "x402".to_owned(),
        asset: offer.asset.clone(),
        amount: offer.amount,
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
        offer_id: Some(offer.id.clone()),
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
        offer: offer.clone(),
        policy: policy.clone(),
        wallet: wallet.clone(),
        origin: "https://feed.example".to_owned(),
        day_started_at: 0,
        evaluated_at: created_at,
    };
    let persisted = storage
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

fn failed_event(intent: &PaymentIntent, stage: &str) -> PaymentEvent {
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

async fn sign_fixture(
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

async fn reserve_authoritatively(
    storage: &PostgresStorage,
    client: &PaymentClient,
    scenario: &PaymentScenario,
    policy: &PaymentPolicy,
    intent: &PaymentIntent,
) -> crate::Result<IdempotentWrite<AuthorizationRecord>> {
    let wallet = storage
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
    storage
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

fn signing_challenge(intent: &PaymentIntent) -> PaymentChallenge {
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

fn recover_address(bytes: &[u8], digest: &[u8; 32]) -> String {
    let signature = Signature::try_from(&bytes[..64]).unwrap();
    let recovery = RecoveryId::try_from(bytes[64].checked_sub(27).unwrap()).unwrap();
    let verifying_key = VerifyingKey::recover_from_prehash(digest, &signature, recovery).unwrap();
    let encoded = verifying_key.to_encoded_point(false);
    let hash = Keccak256::digest(&encoded.as_bytes()[1..]);
    format!("0x{}", hex::encode(&hash[12..]))
}

fn intent(
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
