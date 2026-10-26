use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::BudgetUnit;
use crate::model::LedgerEntryType;
use crate::model::PaymentChallenge;
use crate::model::PaymentClient;
use crate::model::PaymentCredential;
use crate::model::PaymentDirection;
use crate::model::PaymentEvent;
use crate::model::PaymentEventType;
use crate::model::PaymentLedgerEntry;
use crate::model::PaymentPolicy;
use crate::model::PaymentPolicyLimits;
use crate::model::PaymentPrice;
use crate::model::PaymentScenario;
use crate::model::PaymentStatus;
use crate::model::ReservationStatus;
use crate::storage::client::postgres::ClientPostgresRepository;
use crate::storage::event::postgres::EventPostgresRepository;
use crate::storage::ledger::postgres::LedgerPostgresRepository;
use crate::storage::outbox::postgres::OutboxPostgresRepository;
use crate::storage::payment::postgres::PaymentPostgresRepository;
use crate::storage::policy::postgres::PolicyPostgresRepository;
use crate::storage::postgres::tests::*;
use crate::storage::price::postgres::PricePostgresRepository;
use crate::storage::scenario::postgres::ScenarioPostgresRepository;
use crate::storage::signing::postgres::SigningPostgresRepository;
use crate::storage::wallet::postgres::PgWalletKeyRepository;
use crate::storage::wallet::postgres::WalletPostgresRepository;
use crate::storage::IIntentRepository as _;
use crate::storage::IOutboxRepository as _;
use crate::storage::ISigningRepository as _;
use crate::storage::IWalletRepository as _;
use crate::storage::IdempotentWrite;
use crate::storage::OutboundSigningRead;
use crate::storage::OutboxMessage;
use crate::storage::OutboxStatus;
use crate::storage::PaymentProgressWrite;
use crate::storage::PaymentQuery;
use crate::storage::ReconciliationLeaseRequest;
use crate::storage::SettlementWrite;
use crate::storage::SigningGuard;
use crate::storage::SigningWrite;
use crate::storage::VerificationWrite;
use crate::utils::secure_material::SealedSensitiveMaterial;
use crate::Error;
use std::sync::Arc;
use tokio::sync::Barrier;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_URL pointing at a dedicated PostgreSQL database"]
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
    WalletPostgresRepository::new(storage.clone())
        .set_spending_limits(&wallet.id, &unit, Some(Amount::new(100)), Some(Amount::new(100)), 2)
        .await
        .unwrap();
    let alternate_wallet_key = unique_key("pg-budget-wallet-b-key");
    let alternate_wallet = repository
        .create_local(&unique("pg-budget-wallet-b"), &alternate_wallet_key, 1)
        .await
        .unwrap();
    WalletPostgresRepository::new(storage.clone())
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
    crate::storage::IClientRepository::insert(&ClientPostgresRepository::new(storage.clone()), &client)
        .await
        .unwrap();
    let scenario = PaymentScenario {
        id: unique("pg-scenario"),
        client_id: client.id.clone(),
        name: "PostgreSQL news ingestion".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::IScenarioRepository::register(&ScenarioPostgresRepository::new(storage.clone()), &scenario)
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
    crate::storage::IPolicyRepository::insert(&PolicyPostgresRepository::new(storage.clone()), &policy)
        .await
        .unwrap();
    let mut duplicate_policy = policy.clone();
    duplicate_policy.id = unique("pg-policy-duplicate");
    duplicate_policy.created_at += 1;
    duplicate_policy.updated_at += 1;
    assert!(matches!(
        crate::storage::IPolicyRepository::insert(&PolicyPostgresRepository::new(storage.clone()), &duplicate_policy)
            .await,
        Err(Error::PaymentConflict(_))
    ));
    let excessive_price = PaymentPrice {
        id: unique("pg-price-excessive"),
        scenario_id: scenario.id.clone(),
        policy_id: policy.id.clone(),
        resource: format!("https://feed.example/mcp/excessive-{}", Uuid::new_v4()),
        protocols: vec!["x402".to_owned()],
        asset: asset.clone(),
        amount: Amount::new(101),
        wallet_id: wallet.id.clone(),
        enabled: true,
        created_at: 4,
        updated_at: 4,
    };
    assert!(matches!(
        crate::storage::IPriceRepository::insert(&PricePostgresRepository::new(storage.clone()), &excessive_price)
            .await,
        Err(Error::PaymentConflict(_))
    ));
    let mut alternate_policy = policy.clone();
    alternate_policy.id = unique("pg-policy-b");
    alternate_policy.wallet_ids = vec![alternate_wallet.id.clone()];
    alternate_policy.created_at = 4;
    alternate_policy.updated_at = 4;
    crate::storage::IPolicyRepository::insert(&PolicyPostgresRepository::new(storage.clone()), &alternate_policy)
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
    let mut excessive_intent = intent(
        &unique("pg-intent-excessive"),
        &unique("pg-run-excessive"),
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
            .transaction(|connection| {
                Box::pin(
                    async move { PaymentPostgresRepository::insert_intent(connection, &excessive_intent, None).await },
                )
            })
            .await,
        Err(Error::PaymentConflict(_))
    ));
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

    let page = crate::storage::IIntentRepository::list(
        &PaymentPostgresRepository::new(storage.clone()),
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

    let challenge_event = crate::storage::IEventRepository::find_for_intent_type(
        &EventPostgresRepository::new(storage.clone()),
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
    let OutboundSigningRead::Authorized(authorization) = SigningPostgresRepository::new(storage.clone())
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
        SigningPostgresRepository::new(storage.clone())
            .commit_signed(signing_write)
            .await
            .unwrap(),
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
    let submitted =
        crate::storage::IIntentRepository::get(&PaymentPostgresRepository::new(storage.clone()), &signed.id)
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
        PaymentPostgresRepository::new(storage.clone())
            .commit_progress(PaymentProgressWrite {
                intent: &invalid_release,
                expected_updated_at: submitted.updated_at,
                events: std::slice::from_ref(&verified),
                outbox: &[],
            })
            .await,
        Err(Error::PaymentConflict(_))
    ));
    let unchanged =
        crate::storage::IIntentRepository::get(&PaymentPostgresRepository::new(storage.clone()), &signed.id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(unchanged.status, PaymentStatus::Submitted);
    assert_eq!(unchanged.reservation_status, ReservationStatus::Reserved);
    let current = PaymentPostgresRepository::new(storage.clone())
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
    let observed_at = settled.updated_at;
    let effective_at = observed_at + 30;
    settled.settled_at = Some(effective_at);
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
        effective_at,
        created_at: observed_at,
    };
    let outbox = OutboxMessage {
        id: unique("pg-outbox"),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: settled.id.clone(),
        event_type: "settled".to_owned(),
        destination: format!("scenario:{}.payments", settled.scenario_id),
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
    PaymentPostgresRepository::new(storage.clone())
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
        crate::storage::ILedgerRepository::append(&LedgerPostgresRepository::new(storage.clone()), &ledger)
            .await
            .unwrap(),
        IdempotentWrite::Replay(_)
    ));
    let claimed = OutboxPostgresRepository::new(storage.clone())
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
    OutboxPostgresRepository::new(storage.clone())
        .mark_delivered(&claimed.id, "pg-test-worker", settled.updated_at + 1)
        .await
        .unwrap();
    let snapshot = PaymentPostgresRepository::new(storage.clone())
        .budget_snapshot(
            &scenario.id,
            settled.wallet_id.as_deref().unwrap(),
            PaymentDirection::Outbound,
            &unit,
            0,
            observed_at,
        )
        .await
        .unwrap();
    assert_eq!(snapshot.scenario_spent_and_reserved, Amount::new(60));
    assert_eq!(snapshot.wallet_spent_and_reserved, Amount::new(60));
    let invalid_reversal = PaymentLedgerEntry {
        id: unique("pg-ledger-reversal-invalid"),
        entry_type: LedgerEntryType::Reversal,
        direction: PaymentDirection::Outbound,
        posting_key: unique("pg-ledger-reversal-invalid-fact"),
        reversal_of: Some(ledger.id.clone()),
        created_at: settled.updated_at + 1,
        ..ledger.clone()
    };
    assert!(matches!(
        crate::storage::ILedgerRepository::append(&LedgerPostgresRepository::new(storage.clone()), &invalid_reversal)
            .await,
        Err(Error::PaymentConflict(_))
    ));
    let losing_wallet_id = if settled.wallet_id.as_deref() == Some(wallet.id.as_str()) {
        &alternate_wallet.id
    } else {
        &wallet.id
    };
    let losing_snapshot = PaymentPostgresRepository::new(storage.clone())
        .budget_snapshot(
            &scenario.id,
            losing_wallet_id,
            PaymentDirection::Outbound,
            &unit,
            0,
            observed_at,
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
    WalletPostgresRepository::new(storage.clone())
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
        name: "PostgreSQL ledger race".to_owned(),
        enabled: true,
        created_at: 10,
        updated_at: 10,
    };
    crate::storage::IScenarioRepository::register(&ScenarioPostgresRepository::new(storage.clone()), &race_scenario)
        .await
        .unwrap();
    let mut race_policy = policy.clone();
    race_policy.id = unique("pg-ledger-race-policy");
    race_policy.scenario_id.clone_from(&race_scenario.id);
    race_policy.wallet_ids = vec![race_wallet.id.clone()];
    race_policy.created_at = 11;
    race_policy.updated_at = 11;
    crate::storage::IPolicyRepository::insert(&PolicyPostgresRepository::new(storage.clone()), &race_policy)
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
                    LedgerPostgresRepository::lock_financial_scopes(
                        connection,
                        std::slice::from_ref(&ledger_scenario_id),
                        std::slice::from_ref(&ledger_wallet_id),
                    )
                    .await?;
                    let inserted = LedgerPostgresRepository::insert_ledger(connection, &ledger_entry).await?;
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
    let race_snapshot = PaymentPostgresRepository::new(storage.clone())
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

    let price = PaymentPrice {
        id: unique("pg-inbound-expiry-price"),
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
    crate::storage::IPriceRepository::insert(&PricePostgresRepository::new(storage.clone()), &price)
        .await
        .unwrap();
    let payment_wallet = WalletPostgresRepository::new(storage.clone())
        .get_payment_wallet(&wallet.id)
        .await
        .unwrap()
        .unwrap();
    let expired_candidate = record_expiring_inbound_challenge(
        &storage,
        &client,
        &scenario,
        &policy,
        &price,
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
        &price,
        &payment_wallet,
        2_001,
        2_010,
    )
    .await;
    let claimed = PaymentPostgresRepository::new(storage.clone())
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
        .find(|claim| claim.intent.id == expired_candidate.id)
        .cloned()
        .expect("due created challenge must be claimed");
    let claimed_raced = claimed
        .iter()
        .find(|claim| claim.intent.id == raced_candidate.id)
        .cloned()
        .expect("second due created challenge must be claimed");

    assert_eq!(claimed_expired.attempt_no, 1);
    assert_eq!(claimed_raced.attempt_no, 1);
    let mut expired = claimed_expired.intent.clone();
    expired.status = PaymentStatus::Expired;
    expired.reservation_status = ReservationStatus::Released;
    expired.next_reconcile_at = None;
    expired.updated_at += 1;
    let expired_event = failed_event(&expired, "challenge_expired");
    let expired = PaymentPostgresRepository::new(storage.clone())
        .commit_progress(PaymentProgressWrite {
            intent: &expired,
            expected_updated_at: claimed_expired.intent.updated_at,
            events: std::slice::from_ref(&expired_event),
            outbox: &[],
        })
        .await
        .unwrap();
    assert_eq!(expired.status, PaymentStatus::Expired);
    assert_eq!(expired.reservation_status, ReservationStatus::Released);
    let after_expiry = PaymentPostgresRepository::new(storage.clone())
        .budget_snapshot(&scenario.id, &wallet.id, PaymentDirection::Inbound, &unit, 0, 2_010)
        .await
        .unwrap();
    assert_eq!(after_expiry.scenario_spent_and_reserved, price.amount);
    assert_eq!(after_expiry.wallet_spent_and_reserved, price.amount);

    let mut raced_expiry = claimed_raced.intent.clone();
    raced_expiry.status = PaymentStatus::Expired;
    raced_expiry.reservation_status = ReservationStatus::Released;
    raced_expiry.next_reconcile_at = None;
    raced_expiry.updated_at += 1;
    let raced_expiry_event = failed_event(&raced_expiry, "challenge_expired_race");
    let mut submitted = claimed_raced.intent.clone();
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
    let expected_updated_at = claimed_raced.intent.updated_at;
    let expiry_task = tokio::spawn(async move {
        expiry_barrier.wait().await;
        PaymentPostgresRepository::new(expiry_storage.clone())
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
        PaymentPostgresRepository::new(verify_storage.clone())
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
    let raced = crate::storage::IIntentRepository::get(
        &PaymentPostgresRepository::new(storage.clone()),
        &claimed_raced.intent.id,
    )
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

    let reversal = PaymentLedgerEntry {
        id: unique("pg-ledger-reversal"),
        entry_type: LedgerEntryType::Reversal,
        direction: PaymentDirection::Inbound,
        posting_key: unique("pg-ledger-reversal-fact"),
        reversal_of: Some(ledger.id.clone()),
        created_at: settled.updated_at + 1,
        ..ledger
    };
    assert!(matches!(
        crate::storage::ILedgerRepository::append(&LedgerPostgresRepository::new(storage.clone()), &reversal)
            .await
            .unwrap(),
        IdempotentWrite::Inserted(_)
    ));
}
