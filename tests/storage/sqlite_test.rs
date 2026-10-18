use std::{sync::Arc, time::Duration};

use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use sha3::{Digest as _, Keccak256};
use tempfile::TempDir;
use tokio::sync::Barrier;

use crate::{
    config::SqliteStorageConfig,
    model::{
        Amount, AssetIdentity, BudgetUnit, LedgerEntryType, PaymentChallenge, PaymentClient, PaymentDirection,
        PaymentEvent, PaymentEventType, PaymentIntent, PaymentLedgerEntry, PaymentOffer, PaymentPolicy,
        PaymentPolicyLimits, PaymentScenario, PaymentScenarioApprover, PaymentStatus, ReservationStatus,
    },
    protocol::ProtocolSigningRequest,
    storage::{
        ApproverStorage as _, AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite,
        EncryptedWalletKeyVersion, IdempotentWrite, IntentBoundPaymentSignatureRequest, LedgerStorage as _,
        OfferStorage as _, OutboxDeliveryClass, OutboxLeaseRequest, OutboxStorage as _, PaymentProgressWrite,
        PaymentQuery, PaymentStorage as _, PaymentWallet, PolicyStorage as _, ReconciliationLeaseRequest,
        SettlementWrite, WalletKeyStatus, WalletPaymentSignature, WalletPaymentSigner as _, WalletStorage as _,
    },
    utils::aead::{parse_uuid_nonce, WALLET_KEY_ENCRYPTION_SCHEME},
    Error,
};

use super::{key_repository::SqliteWalletKeyRepository, SqliteStorage};

async fn storage() -> (TempDir, SqliteStorage) {
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
    assert_eq!(version, 1);

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

#[tokio::test]
async fn encrypted_versions_sign_only_through_intent_bound_active_keys() {
    let (_directory, storage) = storage().await;
    let repository =
        SqliteWalletKeyRepository::from_resolved_keys(storage.clone(), "kek-v1", [("kek-v1".to_owned(), [9_u8; 32])])
            .unwrap();
    let first = [7_u8; 32];
    let wallet = repository.create_local("wallet-1", &first, 10).await.unwrap();
    let first_payment_wallet = storage.get_payment_wallet("wallet-1").await.unwrap().unwrap();
    let first_key = storage.load_active_key("wallet-1").await.unwrap().unwrap();
    assert_eq!(first_key.version, 1);
    assert_eq!(first_key.status, WalletKeyStatus::Active);
    assert_eq!(first_key.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    assert!(parse_uuid_nonce(&first_key.wrap_nonce).is_ok());
    assert!(parse_uuid_nonce(&first_key.key_nonce).is_ok());
    let first_digest = [0x31_u8; 32];
    let first_signature = sign_fixture(
        &repository,
        &first_payment_wallet,
        &first_key,
        &first_digest,
        "sqlite-key-v1",
    )
    .await
    .unwrap();
    assert_eq!(recover_address(&first_signature.bytes, &first_digest), wallet.address);

    let second = [8_u8; 32];
    let rotated = repository.rotate("wallet-1", &second, 20).await.unwrap();
    assert_ne!(rotated.address, wallet.address);
    let rotated_payment_wallet = storage.get_payment_wallet("wallet-1").await.unwrap().unwrap();
    let active_key = storage.load_active_key("wallet-1").await.unwrap().unwrap();
    assert_eq!(active_key.version, 2);
    assert_eq!(active_key.status, WalletKeyStatus::Active);
    assert!(matches!(
        sign_fixture(
            &repository,
            &rotated_payment_wallet,
            &first_key,
            &[0x32_u8; 32],
            "sqlite-stale-key",
        )
        .await,
        Err(Error::PaymentConflict(_))
    ));
    let second_digest = [0x33_u8; 32];
    let second_signature = sign_fixture(
        &repository,
        &rotated_payment_wallet,
        &active_key,
        &second_digest,
        "sqlite-key-v2",
    )
    .await
    .unwrap();
    assert_eq!(
        recover_address(&second_signature.bytes, &second_digest),
        rotated.address
    );
    let versions = storage.list_key_versions("wallet-1").await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].status, WalletKeyStatus::Retired);
    assert_eq!(versions[1].status, WalletKeyStatus::Active);
    assert_ne!(versions[0].key_ciphertext, first);
}

#[tokio::test]
async fn outbox_class_claim_does_not_lease_foreign_rows_and_replays_after_restart() {
    let (directory, storage) = storage().await;
    let database_file = directory.path().join("agentvault.sqlite3").display().to_string();
    for (id, event_type, destination) in [
        ("approval-message", "approval_required", "client:client-1.approvals"),
        ("payment-message", "payment_settled", "scenario:scenario-1.payments"),
        ("foreign-message", "webhook_event", "webhook:external"),
    ] {
        sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
             run_id, payload, status, attempt_count, available_at, created_at) \
             VALUES (?1, 'payment_intent', 'intent-1', ?2, ?3, ?1, '{}', 'pending', 0, 10, 10)",
        )
        .bind(id)
        .bind(event_type)
        .bind(destination)
        .execute(storage.pool())
        .await
        .unwrap();
    }

    let approval = storage
        .claim(&OutboxLeaseRequest {
            owner: "approval-worker".to_owned(),
            class: OutboxDeliveryClass::ApprovalRequired,
            now: 10,
            lease_until: 20,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        approval.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(),
        ["approval-message"]
    );
    let untouched: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = 'payment-message'")
            .fetch_one(storage.pool())
            .await
            .unwrap();
    assert_eq!(untouched, (0, None));

    drop(storage);
    let reopened = SqliteStorage::connect(&SqliteStorageConfig {
        database_file,
        max_connections: 4,
        busy_timeout: Duration::from_secs(5),
    })
    .await
    .unwrap();
    let payment = reopened
        .claim(&OutboxLeaseRequest {
            owner: "payment-worker".to_owned(),
            class: OutboxDeliveryClass::PaymentEvents,
            now: 21,
            lease_until: 31,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        payment.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(),
        ["payment-message"]
    );
    let replay = reopened
        .claim(&OutboxLeaseRequest {
            owner: "approval-worker-restarted".to_owned(),
            class: OutboxDeliveryClass::ApprovalRequired,
            now: 21,
            lease_until: 31,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].id, "approval-message");
    assert_eq!(replay[0].attempt_count, 2);
    let foreign: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = 'foreign-message'")
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(foreign, (0, None));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // End-to-end transaction setup is intentionally visible in one scenario.
async fn serializes_budget_reservations_and_posts_settlement_atomically() {
    let (_directory, storage) = storage().await;
    let repository =
        SqliteWalletKeyRepository::from_resolved_keys(storage.clone(), "kek-v1", [("kek-v1".to_owned(), [4_u8; 32])])
            .unwrap();
    let wallet = repository.create_local("wallet-budget", &[5_u8; 32], 1).await.unwrap();
    let unit = BudgetUnit {
        asset: "USDC".to_owned(),
        decimals: 6,
    };
    storage
        .set_spending_limits(&wallet.id, &unit, Some(Amount::new(100)), Some(Amount::new(100)), 2)
        .await
        .unwrap();
    let client = PaymentClient {
        id: "client-1".to_owned(),
        name: "Quant Ingestor".to_owned(),
        auth_principal: "principal:quant".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::ClientStorage::insert(&storage, &client).await.unwrap();
    let scenario = PaymentScenario {
        id: "scenario-1".to_owned(),
        client_id: client.id.clone(),
        external_ref: "news-ingestion".to_owned(),
        name: "News ingestion".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::ScenarioStorage::register(&storage, &scenario)
        .await
        .unwrap();
    let approver = PaymentScenarioApprover {
        scenario_id: scenario.id.clone(),
        auth_principal: "principal:admin".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    storage.upsert(&approver).await.unwrap();
    assert!(storage
        .is_authorized(&scenario.id, &approver.auth_principal)
        .await
        .unwrap());

    let asset = AssetIdentity {
        network: "eip155:8453".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    };
    let recipient = "0x2222222222222222222222222222222222222222".to_owned();
    let policy = PaymentPolicy {
        id: "policy-1".to_owned(),
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
            approval_enabled: true,
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
    let mut duplicate_policy = policy.clone();
    duplicate_policy.id = "policy-duplicate".to_owned();
    duplicate_policy.created_at += 1;
    duplicate_policy.updated_at += 1;
    assert!(matches!(
        crate::storage::PolicyStorage::insert(&storage, &duplicate_policy).await,
        Err(Error::PaymentConflict(_))
    ));
    assert!(storage
        .has_enabled_wallet_binding(&scenario.id, &wallet.id)
        .await
        .unwrap());
    assert!(!storage
        .has_enabled_wallet_binding(&scenario.id, "wallet-not-bound")
        .await
        .unwrap());

    let offer = PaymentOffer {
        id: "offer-1".to_owned(),
        scenario_id: scenario.id.clone(),
        policy_id: policy.id.clone(),
        resource: "https://feed.example/paid".to_owned(),
        protocols: vec!["x402".to_owned()],
        asset: asset.clone(),
        amount: Amount::new(10),
        wallet_id: wallet.id.clone(),
        enabled: true,
        created_at: 4,
        updated_at: 4,
    };
    crate::storage::OfferStorage::insert(&storage, &offer).await.unwrap();
    let mut excessive_offer = offer.clone();
    excessive_offer.id = "offer-excessive".to_owned();
    excessive_offer.resource = "https://feed.example/excessive".to_owned();
    excessive_offer.amount = Amount::new(101);
    assert!(matches!(
        crate::storage::OfferStorage::insert(&storage, &excessive_offer).await,
        Err(Error::PaymentConflict(_))
    ));
    assert_eq!(
        storage.resolve_resource(&scenario.id, &offer.resource).await.unwrap(),
        Some(offer.clone())
    );
    assert!(storage
        .resolve_resource(&scenario.id, "https://feed.example/missing")
        .await
        .unwrap()
        .is_none());

    let first = intent(
        "intent-a",
        "key-a",
        &scenario.id,
        &wallet.id,
        &policy.id,
        &asset,
        &recipient,
    );
    let second = intent(
        "intent-b",
        "key-b",
        &scenario.id,
        &wallet.id,
        &policy.id,
        &asset,
        &recipient,
    );
    let mut excessive_intent = intent(
        "intent-excessive",
        "run-excessive",
        &scenario.id,
        &wallet.id,
        &policy.id,
        &asset,
        &recipient,
    );
    excessive_intent.amount = Amount::new(101);
    excessive_intent.reserved_amount = Amount::new(101);
    assert!(matches!(
        storage
            .immediate(|connection| {
                Box::pin(async move { super::payment::insert_intent(connection, &excessive_intent, None).await })
            })
            .await,
        Err(Error::PaymentConflict(_))
    ));
    let barrier = Arc::new(Barrier::new(3));
    let left_storage = storage.clone();
    let left_barrier = barrier.clone();
    let left_client = client.clone();
    let left_scenario = scenario.clone();
    let left_policy = policy.clone();
    let left = tokio::spawn(async move {
        left_barrier.wait().await;
        reserve_authoritatively(&left_storage, &left_client, &left_scenario, &left_policy, &first).await
    });
    let right_storage = storage.clone();
    let right_barrier = barrier.clone();
    let right_client = client.clone();
    let right_scenario = scenario.clone();
    let right_policy = policy.clone();
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
    let replay = reserve_authoritatively(&storage, &client, &scenario, &policy, &current)
        .await
        .unwrap();
    assert!(matches!(replay, IdempotentWrite::Replay(_)));
    let challenge_event = crate::storage::EventStorage::find_for_intent_type(
        &storage,
        &current.id,
        PaymentEventType::ChallengeReceived,
        0,
    )
    .await
    .unwrap()
    .expect("authorization must persist its exact recovery event");
    assert_eq!(challenge_event.intent_id, current.id);

    // Settlement is intentionally a dedicated atomic port. Seed its already-
    // submitted precondition directly in this storage contract test rather
    // than reintroducing a generic intent mutation API that could bypass the
    // intent-bound signing transaction.
    let submitted_at = current.updated_at + 1;
    let changed = sqlx::query(
        "UPDATE pay_intent SET status = 'signed', next_reconcile_at = ?1, updated_at = ?1 \
         WHERE id = ?2 AND status = 'authorized' AND updated_at = ?3",
    )
    .bind(submitted_at)
    .bind(&current.id)
    .bind(current.updated_at)
    .execute(storage.pool())
    .await
    .unwrap();
    assert_eq!(changed.rows_affected(), 1);
    let claimed = storage
        .claim_reconciliation(&ReconciliationLeaseRequest {
            owner: "sqlite-reconciler".to_owned(),
            now: submitted_at,
            lease_until: submitted_at + 30,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    storage
        .renew_reconciliation_lease(&current.id, "sqlite-reconciler", submitted_at + 60)
        .await
        .unwrap();
    storage
        .release_reconciliation_lease(&current.id, "sqlite-reconciler", submitted_at + 5)
        .await
        .unwrap();
    sqlx::query("UPDATE pay_intent SET status = 'submitted' WHERE id = ?1 AND status = 'signed'")
        .bind(&current.id)
        .execute(storage.pool())
        .await
        .unwrap();
    let submitted = crate::storage::PaymentStorage::get(&storage, &current.id)
        .await
        .unwrap()
        .unwrap();
    let mut pending = submitted.clone();
    pending.status = PaymentStatus::PendingSettlement;
    pending.updated_at += 1;
    pending.next_reconcile_at = Some(pending.updated_at + 5);
    let verified = PaymentEvent {
        id: "event-verified".to_owned(),
        intent_id: pending.id.clone(),
        event_type: PaymentEventType::CredentialVerified,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({"verification_ref": "verify-1"}),
        created_at: pending.updated_at,
    };
    let current = storage
        .commit_progress(PaymentProgressWrite {
            intent: &pending,
            expected_updated_at: submitted.updated_at,
            events: std::slice::from_ref(&verified),
            outbox: &[],
        })
        .await
        .unwrap();
    let mut settled = current.clone();
    settled.status = PaymentStatus::Settled;
    settled.reservation_status = ReservationStatus::Consumed;
    settled.settled_amount = settled.amount;
    settled.tx_hash = Some("0xsettlement".to_owned());
    settled.updated_at += 1;
    settled.settled_at = Some(settled.updated_at);
    let ledger = PaymentLedgerEntry {
        id: "ledger-1".to_owned(),
        intent_id: Some(settled.id.clone()),
        scenario_id: Some(settled.scenario_id.clone()),
        wallet_id: wallet.id.clone(),
        entry_type: LedgerEntryType::Payment,
        direction: PaymentDirection::Outbound,
        asset: asset.clone(),
        amount: settled.amount,
        posting_key: "chain:one".to_owned(),
        tx_hash: Some("0xsettlement".to_owned()),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: None,
        effective_at: settled.updated_at,
        created_at: settled.updated_at,
    };
    storage
        .commit_settlement(SettlementWrite {
            intent: &settled,
            expected_updated_at: current.updated_at,
            events: &[],
            ledger_entries: std::slice::from_ref(&ledger),
            outbox: &[],
        })
        .await
        .unwrap();
    assert!(matches!(
        storage.append(&ledger).await.unwrap(),
        IdempotentWrite::Replay(_)
    ));
    let snapshot = storage
        .budget_snapshot(
            &scenario.id,
            &wallet.id,
            PaymentDirection::Outbound,
            &unit,
            0,
            settled.updated_at,
        )
        .await
        .unwrap();
    assert_eq!(snapshot.scenario_spent_and_reserved, Amount::new(60));
    assert_eq!(snapshot.wallet_spent_and_reserved, Amount::new(60));

    let invalid_reversal = PaymentLedgerEntry {
        id: "ledger-reversal-invalid".to_owned(),
        entry_type: LedgerEntryType::Reversal,
        direction: PaymentDirection::Outbound,
        posting_key: "chain:one:reversal-invalid".to_owned(),
        reversal_of: Some(ledger.id.clone()),
        created_at: settled.updated_at + 1,
        ..ledger.clone()
    };
    assert!(matches!(
        storage.append(&invalid_reversal).await,
        Err(Error::PaymentConflict(_))
    ));

    let reversal = PaymentLedgerEntry {
        id: "ledger-reversal".to_owned(),
        entry_type: LedgerEntryType::Reversal,
        direction: PaymentDirection::Inbound,
        posting_key: "chain:one:reversal".to_owned(),
        reversal_of: Some(ledger.id.clone()),
        created_at: settled.updated_at + 1,
        ..ledger
    };
    assert!(matches!(
        storage.append(&reversal).await.unwrap(),
        IdempotentWrite::Inserted(_)
    ));
}

async fn sign_fixture(
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

async fn reserve_authoritatively(
    storage: &SqliteStorage,
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
