use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::BudgetUnit;
use crate::model::LedgerEntryType;
use crate::model::PaymentClient;
use crate::model::PaymentDirection;
use crate::model::PaymentEvent;
use crate::model::PaymentEventType;
use crate::model::PaymentLedgerEntry;
use crate::model::PaymentPolicy;
use crate::model::PaymentPolicyLimits;
use crate::model::PaymentPrice;
use crate::model::PaymentScenario;
use crate::model::PaymentScenarioApprover;
use crate::model::PaymentStatus;
use crate::model::ReservationStatus;
use crate::storage::approver::sqlite::ApproverSqliteRepository;
use crate::storage::client::sqlite::ClientSqliteRepository;
use crate::storage::event::sqlite::EventSqliteRepository;
use crate::storage::ledger::sqlite::LedgerSqliteRepository;
use crate::storage::payment::sqlite::PaymentSqliteRepository;
use crate::storage::policy::sqlite::PolicySqliteRepository;
use crate::storage::price::sqlite::PriceSqliteRepository;
use crate::storage::scenario::sqlite::ScenarioSqliteRepository;
use crate::storage::sqlite::tests::*;
use crate::storage::wallet::sqlite::SqliteWalletKeyRepository;
use crate::storage::wallet::sqlite::WalletSqliteRepository;
use crate::storage::ApproverStorage as _;
use crate::storage::IdempotentWrite;
use crate::storage::LedgerStorage as _;
use crate::storage::PaymentProgressWrite;
use crate::storage::PaymentQuery;
use crate::storage::PaymentStorage as _;
use crate::storage::PolicyStorage as _;
use crate::storage::PriceStorage as _;
use crate::storage::ReconciliationLeaseRequest;
use crate::storage::SettlementWrite;
use crate::storage::WalletStorage as _;
use crate::Error;
use std::sync::Arc;
use tokio::sync::Barrier;

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
    WalletSqliteRepository::new(storage.clone())
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
    crate::storage::ClientStorage::insert(&ClientSqliteRepository::new(storage.clone()), &client)
        .await
        .unwrap();
    let scenario = PaymentScenario {
        id: "scenario-1".to_owned(),
        client_id: client.id.clone(),
        name: "News ingestion".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::ScenarioStorage::register(&ScenarioSqliteRepository::new(storage.clone()), &scenario)
        .await
        .unwrap();
    let approver = PaymentScenarioApprover {
        scenario_id: scenario.id.clone(),
        auth_principal: "principal:admin".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    ApproverSqliteRepository::new(storage.clone())
        .upsert(&approver)
        .await
        .unwrap();
    assert!(ApproverSqliteRepository::new(storage.clone())
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
    crate::storage::PolicyStorage::insert(&PolicySqliteRepository::new(storage.clone()), &policy)
        .await
        .unwrap();
    let mut duplicate_policy = policy.clone();
    duplicate_policy.id = "policy-duplicate".to_owned();
    duplicate_policy.created_at += 1;
    duplicate_policy.updated_at += 1;
    assert!(matches!(
        crate::storage::PolicyStorage::insert(&PolicySqliteRepository::new(storage.clone()), &duplicate_policy).await,
        Err(Error::PaymentConflict(_))
    ));
    assert!(PolicySqliteRepository::new(storage.clone())
        .has_enabled_wallet_binding(&scenario.id, &wallet.id)
        .await
        .unwrap());
    assert!(!PolicySqliteRepository::new(storage.clone())
        .has_enabled_wallet_binding(&scenario.id, "wallet-not-bound")
        .await
        .unwrap());

    let offer = PaymentPrice {
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
    crate::storage::PriceStorage::insert(&PriceSqliteRepository::new(storage.clone()), &offer)
        .await
        .unwrap();
    let mut excessive_offer = offer.clone();
    excessive_offer.id = "offer-excessive".to_owned();
    excessive_offer.resource = "https://feed.example/excessive".to_owned();
    excessive_offer.amount = Amount::new(101);
    assert!(matches!(
        crate::storage::PriceStorage::insert(&PriceSqliteRepository::new(storage.clone()), &excessive_offer).await,
        Err(Error::PaymentConflict(_))
    ));
    assert_eq!(
        PriceSqliteRepository::new(storage.clone())
            .resolve_resource(&scenario.id, &offer.resource)
            .await
            .unwrap(),
        Some(offer.clone())
    );
    assert!(PriceSqliteRepository::new(storage.clone())
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
                Box::pin(
                    async move { PaymentSqliteRepository::insert_intent(connection, &excessive_intent, None).await },
                )
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
        &PaymentSqliteRepository::new(storage.clone()),
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
        &EventSqliteRepository::new(storage.clone()),
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
    let claimed = PaymentSqliteRepository::new(storage.clone())
        .claim_reconciliation(&ReconciliationLeaseRequest {
            owner: "sqlite-reconciler".to_owned(),
            now: submitted_at,
            lease_until: submitted_at + 30,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].intent.id, current.id);
    assert_eq!(claimed[0].attempt_no, 1);
    PaymentSqliteRepository::new(storage.clone())
        .renew_reconciliation_lease(&current.id, "sqlite-reconciler", submitted_at + 60)
        .await
        .unwrap();
    PaymentSqliteRepository::new(storage.clone())
        .release_reconciliation_lease(&current.id, "sqlite-reconciler", submitted_at + 5)
        .await
        .unwrap();
    sqlx::query("UPDATE pay_intent SET status = 'submitted' WHERE id = ?1 AND status = 'signed'")
        .bind(&current.id)
        .execute(storage.pool())
        .await
        .unwrap();
    let submitted = crate::storage::PaymentStorage::get(&PaymentSqliteRepository::new(storage.clone()), &current.id)
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
    let current = PaymentSqliteRepository::new(storage.clone())
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
    let observed_at = settled.updated_at;
    let effective_at = observed_at + 30;
    settled.settled_at = Some(effective_at);
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
        effective_at,
        created_at: observed_at,
    };
    PaymentSqliteRepository::new(storage.clone())
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
        LedgerSqliteRepository::new(storage.clone())
            .append(&ledger)
            .await
            .unwrap(),
        IdempotentWrite::Replay(_)
    ));
    let snapshot = PaymentSqliteRepository::new(storage.clone())
        .budget_snapshot(
            &scenario.id,
            &wallet.id,
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
        id: "ledger-reversal-invalid".to_owned(),
        entry_type: LedgerEntryType::Reversal,
        direction: PaymentDirection::Outbound,
        posting_key: "chain:one:reversal-invalid".to_owned(),
        reversal_of: Some(ledger.id.clone()),
        created_at: settled.updated_at + 1,
        ..ledger.clone()
    };
    assert!(matches!(
        LedgerSqliteRepository::new(storage.clone())
            .append(&invalid_reversal)
            .await,
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
        LedgerSqliteRepository::new(storage.clone())
            .append(&reversal)
            .await
            .unwrap(),
        IdempotentWrite::Inserted(_)
    ));

    let excessive_future = PaymentLedgerEntry {
        id: "ledger-excessive-future".to_owned(),
        intent_id: None,
        scenario_id: Some(scenario.id.clone()),
        wallet_id: wallet.id.clone(),
        entry_type: LedgerEntryType::Payment,
        direction: PaymentDirection::Outbound,
        asset,
        amount: Amount::new(1),
        posting_key: "chain:excessive-future".to_owned(),
        tx_hash: Some("0xexcessivefuture".to_owned()),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: None,
        effective_at: observed_at + 61,
        created_at: observed_at,
    };
    LedgerSqliteRepository::new(storage.clone())
        .append(&excessive_future)
        .await
        .unwrap();
    assert!(matches!(
        PaymentSqliteRepository::new(storage.clone()).budget_snapshot(
                &scenario.id,
                &wallet.id,
                PaymentDirection::Outbound,
                &unit,
                0,
                observed_at,
            )
            .await,
        Err(Error::Storage(reason)) if reason.contains("bounded future-skew window")
    ));
}
