use crate::model::PaymentLedgerEntry;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicI64, Ordering},
        Mutex,
    },
};

use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::PaymentDirection;
use crate::model::SettlementProviderResult;
use crate::storage::AuthorizationRecord;
use crate::storage::AuthorizationWrite;
use crate::storage::BudgetSnapshot;
use crate::storage::ChallengeWrite;
use crate::storage::IdempotentWrite;
use crate::storage::OutboundEvidenceWrite;
use crate::storage::Page;
use crate::storage::PaymentQuery;
use crate::storage::ReconciliationClaim;
use crate::storage::VerificationWrite;

use super::*;
use crate::model::SettlementOperation;
use crate::model::SettlementProviderRequest;
use crate::payment::ExternalSettlementProvider;
use crate::payment::ProviderReconciliationRequest;
use crate::payment::VerifyOnlyProviderEvidence;
use async_trait::async_trait;

const RECIPIENT: &str = "0x2222222222222222222222222222222222222222";
const TX_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct FixedClock(AtomicI64);

impl FixedClock {
    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl UnixClock for FixedClock {
    fn now(&self) -> Result<i64> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

struct MockEvidence {
    outcomes: Mutex<VecDeque<Result<ReconciliationEvidence>>>,
}

#[async_trait]
impl ReconciliationEvidenceSource for MockEvidence {
    async fn lookup(&self, _intent: &PaymentIntent, _observed_at: i64) -> Result<ReconciliationEvidence> {
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(ReconciliationEvidence::Pending))
    }
}

struct BlockingEvidence {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ReconciliationEvidenceSource for BlockingEvidence {
    async fn lookup(&self, _intent: &PaymentIntent, _observed_at: i64) -> Result<ReconciliationEvidence> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ReconciliationEvidence::Pending)
    }
}

#[derive(Default)]
struct MockProvider {
    operations: Mutex<Vec<SettlementOperation>>,
}

#[async_trait]
impl ExternalSettlementProvider for MockProvider {
    async fn execute(
        &self,
        operation: SettlementOperation,
        _request: &SettlementProviderRequest,
    ) -> Result<SettlementProviderResult> {
        self.operations.lock().unwrap().push(operation);
        Ok(SettlementProviderResult {
            valid: true,
            settled: true,
            payer: Some("0xpayer".to_owned()),
            transaction: Some(TX_HASH.to_owned()),
            settlement_ref: Some("settlement-1".to_owned()),
            effect_ref: Some("log:7".to_owned()),
            invalid_reason: None,
            asset: Some(intent(PaymentStatus::PendingSettlement).asset),
            amount: Some(Amount::new(1_000_000)),
            recipient: Some(RECIPIENT.to_owned()),
            confirmations: Some(3),
            extensions: BTreeMap::new(),
        })
    }
}

struct MockRequestBuilder;

impl ProviderReconciliationRequest for MockRequestBuilder {
    fn authority(&self) -> &'static str {
        "facilitator-a"
    }

    fn build(&self, intent: &PaymentIntent) -> Result<Option<SettlementProviderRequest>> {
        Ok(Some(SettlementProviderRequest {
            protocol: intent.protocol.clone(),
            run_id: intent.run_id.clone(),
            payment_payload: json!({"durable": true}),
            payment_requirements: json!({"intent_id": intent.id}),
        }))
    }
}

#[derive(Default)]
struct MockState {
    intent: Option<PaymentIntent>,
    lease_owner: Option<String>,
    lease_until: Option<i64>,
    last_claim_at: Option<i64>,
    reconcile_attempts: u32,
    releases: Vec<i64>,
    commits: u32,
    ledger: Vec<PaymentLedgerEntry>,
    events: Vec<PaymentEvent>,
    settlement_failures_remaining: u32,
}

#[derive(Default)]
struct MockStorage {
    state: Mutex<MockState>,
}

#[async_trait]
impl PaymentStorage for MockStorage {
    async fn find_run(&self, _scenario_id: &str, _run_id: &str) -> Result<Option<AuthorizationRecord>> {
        Ok(None)
    }

    async fn authorize(&self, _write: AuthorizationWrite<'_>) -> Result<IdempotentWrite<AuthorizationRecord>> {
        Err(Error::Storage("unused test operation".to_owned()))
    }

    async fn budget_snapshot(
        &self,
        _scenario_id: &str,
        _wallet_id: &str,
        _direction: crate::model::PaymentDirection,
        budget_unit: &crate::model::BudgetUnit,
        _day_started_at: i64,
        _now: i64,
    ) -> Result<BudgetSnapshot> {
        Ok(BudgetSnapshot::empty(budget_unit.clone()))
    }

    async fn record_challenge(&self, _write: ChallengeWrite<'_>) -> Result<IdempotentWrite<PaymentIntent>> {
        Err(Error::Storage("unused test operation".to_owned()))
    }

    async fn get_for_challenge(&self, _scenario_id: &str, _challenge_hash: &str) -> Result<Option<PaymentIntent>> {
        Ok(None)
    }

    async fn verify_incoming(&self, _write: VerificationWrite<'_>) -> Result<PaymentIntent> {
        Err(Error::Storage("unused test operation".to_owned()))
    }

    async fn get(&self, intent_id: &str) -> Result<Option<PaymentIntent>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .intent
            .clone()
            .filter(|intent| intent.id == intent_id))
    }

    async fn get_for_scenario(&self, scenario_id: &str, intent_id: &str) -> Result<Option<PaymentIntent>> {
        Ok(self
            .get(intent_id)
            .await?
            .filter(|intent| intent.scenario_id == scenario_id))
    }

    async fn record_outbound_evidence(
        &self,
        _write: OutboundEvidenceWrite<'_>,
    ) -> Result<IdempotentWrite<PaymentIntent>> {
        Err(Error::Storage("unused test operation".to_owned()))
    }

    async fn commit_progress(&self, write: PaymentProgressWrite<'_>) -> Result<PaymentIntent> {
        let mut state = self.state.lock().unwrap();
        if state.lease_owner.as_deref().is_none() {
            return Err(Error::Storage("payment progress requires a lease".to_owned()));
        }
        assert_eq!(write.events.len(), 1);
        match write.intent.status {
            PaymentStatus::PendingSettlement => {
                assert!(write.outbox.is_empty());
                assert_eq!(write.intent.reservation_status, ReservationStatus::Reserved);
            }
            PaymentStatus::Expired => {
                assert_eq!(write.outbox.len(), 1);
                assert_eq!(write.intent.reservation_status, ReservationStatus::Released);
            }
            status => panic!("unexpected progress status {status:?}"),
        }
        state.commits += 1;
        state.events.extend_from_slice(write.events);
        state.intent = Some(write.intent.clone());
        state.lease_owner = None;
        state.lease_until = None;
        Ok(write.intent.clone())
    }

    async fn list(&self, _query: &PaymentQuery) -> Result<Page<PaymentIntent>> {
        let items = self
            .state
            .lock()
            .unwrap()
            .intent
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        Ok(Page {
            total: items.len() as u64,
            items,
        })
    }

    async fn claim_reconciliation(&self, request: &ReconciliationLeaseRequest) -> Result<Vec<ReconciliationClaim>> {
        let mut state = self.state.lock().unwrap();
        let available = state.intent.as_ref().is_some_and(|intent| {
            ((intent.needs_reconciliation() && intent.next_reconcile_at.is_none_or(|due| due <= request.now))
                || (intent.status == PaymentStatus::Created
                    && intent.expires_at.is_some_and(|expires_at| expires_at <= request.now)))
                && state.lease_until.is_none_or(|lease| lease <= request.now)
        });
        if !available || request.limit == 0 {
            return Ok(Vec::new());
        }
        state.lease_owner = Some(request.owner.clone());
        state.lease_until = Some(request.lease_until);
        state.last_claim_at = Some(request.now);
        state.reconcile_attempts = state.reconcile_attempts.saturating_add(1);
        Ok(state
            .intent
            .clone()
            .map(|intent| ReconciliationClaim {
                intent,
                attempt_no: state.reconcile_attempts,
            })
            .into_iter()
            .collect())
    }

    async fn renew_reconciliation_lease(&self, intent_id: &str, owner: &str, lease_until: i64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.intent.as_ref().is_none_or(|intent| intent.id != intent_id)
            || state.lease_owner.as_deref() != Some(owner)
        {
            return Err(Error::Storage("lease ownership conflict".to_owned()));
        }
        state.lease_until = Some(lease_until);
        Ok(())
    }

    async fn release_reconciliation_lease(&self, intent_id: &str, owner: &str, next_attempt_at: i64) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.intent.as_ref().is_none_or(|intent| intent.id != intent_id)
            || state.lease_owner.as_deref() != Some(owner)
        {
            return Err(Error::Storage("lease ownership conflict".to_owned()));
        }
        let last_claim_at = state.last_claim_at.unwrap();
        let intent = state.intent.as_mut().unwrap();
        intent.next_reconcile_at = Some(next_attempt_at);
        intent.updated_at = last_claim_at;
        state.lease_owner = None;
        state.lease_until = None;
        state.releases.push(next_attempt_at);
        Ok(())
    }

    async fn commit_settlement(&self, write: SettlementWrite<'_>) -> Result<PaymentIntent> {
        let mut state = self.state.lock().unwrap();
        if state.lease_owner.as_deref().is_none() {
            return Err(Error::Storage("settlement requires a lease".to_owned()));
        }
        if state.settlement_failures_remaining > 0 {
            state.settlement_failures_remaining -= 1;
            return Err(Error::Storage("injected local settlement commit failure".to_owned()));
        }
        if state.commits > 0 {
            let stored = state.intent.clone().unwrap();
            if stored == *write.intent {
                return Ok(stored);
            }
            return Err(Error::PaymentConflict("settlement commit changed".to_owned()));
        }
        assert_eq!(write.events.len(), 1);
        assert_eq!(write.intent.status, PaymentStatus::Settled);
        assert_eq!(write.ledger_entries.len(), 1);
        assert_eq!(write.outbox.len(), 1);
        state.commits += 1;
        state.ledger.extend_from_slice(write.ledger_entries);
        state.events.extend_from_slice(write.events);
        state.intent = Some(write.intent.clone());
        state.lease_owner = None;
        state.lease_until = None;
        Ok(write.intent.clone())
    }
}

pub(crate) fn intent(status: PaymentStatus) -> PaymentIntent {
    PaymentIntent {
        id: "intent-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
        wallet_id: Some("wallet-1".to_owned()),
        policy_id: Some("policy-1".to_owned()),
        offer_id: None,
        direction: PaymentDirection::Outbound,
        protocol: "x402".to_owned(),
        asset: AssetIdentity {
            network: "eip155:8453".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1234".to_owned()),
            decimals: 6,
        },
        recipient: RECIPIENT.to_owned(),
        amount: Amount::new(1_000_000),
        settled_amount: Amount::ZERO,
        resource: "https://merchant.example/tool".to_owned(),
        challenge_hash: Some("challenge".to_owned()),
        request_hash: "request".to_owned(),
        status,
        reservation_status: ReservationStatus::Reserved,
        reserved_amount: Amount::new(1_000_000),
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at: 90,
        updated_at: 90,
        expires_at: Some(1_000),
        settled_at: None,
    }
}

fn config(worker_id: &str) -> ReconciliationConfig {
    ReconciliationConfig {
        worker_id: worker_id.to_owned(),
        batch_size: 10,
        lease_duration: Duration::from_secs(30),
        initial_backoff: Duration::from_secs(5),
        max_backoff: Duration::from_secs(20),
    }
}

fn job<E>(storage: Arc<MockStorage>, evidence: Arc<E>, clock: Arc<FixedClock>, worker_id: &str) -> PaymentReconciler
where
    E: ReconciliationEvidenceSource + 'static,
{
    PaymentReconciler::with_clock(storage, evidence, clock, config(worker_id)).unwrap()
}

#[tokio::test]
async fn confirmed_evidence_atomically_settles_and_posts_once() {
    let storage = Arc::new(MockStorage::default());
    storage.state.lock().unwrap().intent = Some(intent(PaymentStatus::PendingSettlement));
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::from([Ok(ReconciliationEvidence::Confirmed(Box::new(
            ConfirmedSettlement {
                authority: "facilitator-a".to_owned(),
                asset: intent(PaymentStatus::PendingSettlement).asset,
                recipient: RECIPIENT.to_owned(),
                payer: Some(format!("0x{}", "33".repeat(20))),
                settled_amount: Amount::new(1_000_000),
                posting_key: crate::model::PaymentLedgerEntry::settlement_posting_key(
                    "facilitator-a",
                    "eip155:8453",
                    Some(TX_HASH),
                    Some("settlement-1"),
                    Some("log:7"),
                )
                .unwrap(),
                tx_hash: Some(TX_HASH.to_owned()),
                settlement_ref: Some("settlement-1".to_owned()),
                effect_ref: Some("log:7".to_owned()),
                confirmations: 3,
                minimum_confirmations: 1,
                // Exercise accepted one-second chain/local clock skew.
                effective_at: 89,
            },
        )))])),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker = job(Arc::clone(&storage), evidence, clock, "worker-a");

    let run = worker.run_once().await.unwrap();

    assert_eq!(run.settled, 1);
    let state = storage.state.lock().unwrap();
    assert_eq!(state.commits, 1);
    assert_eq!(state.ledger.len(), 1);
    assert_eq!(state.ledger[0].effective_at, 89);
    assert_eq!(state.intent.as_ref().unwrap().status, PaymentStatus::Settled);
    assert_eq!(state.intent.as_ref().unwrap().settled_at, Some(90));
    assert_eq!(
        state.intent.as_ref().unwrap().reservation_status,
        ReservationStatus::Consumed
    );
}

#[tokio::test]
async fn trusted_chain_evidence_recovers_after_local_commit_failure_and_restart() {
    let storage = Arc::new(MockStorage::default());
    {
        let mut state = storage.state.lock().unwrap();
        let mut payment = intent(PaymentStatus::PendingSettlement);
        payment.direction = PaymentDirection::Inbound;
        state.intent = Some(payment.clone());
        state.settlement_failures_remaining = 1;
    }
    let confirmed = || {
        ReconciliationEvidence::Confirmed(Box::new(ConfirmedSettlement {
            authority: "evm-rpc:eip155:8453".to_owned(),
            asset: intent(PaymentStatus::PendingSettlement).asset,
            recipient: RECIPIENT.to_owned(),
            payer: Some(format!("0x{}", "33".repeat(20))),
            settled_amount: Amount::new(1_000_000),
            posting_key: crate::model::PaymentLedgerEntry::settlement_posting_key(
                "evm-rpc:eip155:8453",
                "eip155:8453",
                Some(TX_HASH),
                None,
                Some("log:7"),
            )
            .unwrap(),
            tx_hash: Some(TX_HASH.to_owned()),
            settlement_ref: None,
            effect_ref: Some("log:7".to_owned()),
            confirmations: 3,
            minimum_confirmations: 1,
            effective_at: 99,
        }))
    };
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::from([Ok(confirmed()), Ok(confirmed())])),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let first_process = job(
        Arc::clone(&storage),
        Arc::clone(&evidence),
        Arc::clone(&clock),
        "worker-before-crash",
    );

    assert!(matches!(first_process.run_once().await, Err(Error::Storage(_))));
    assert!(storage.state.lock().unwrap().ledger.is_empty());

    // A restarted worker reclaims the expired DB lease and consumes the
    // same independently verified on-chain effect exactly once.
    clock.set(131);
    let restarted = job(storage.clone(), evidence, clock, "worker-after-restart");
    assert_eq!(restarted.run_once().await.unwrap().settled, 1);
    let state = storage.state.lock().unwrap();
    assert_eq!(state.ledger.len(), 1);
    assert_eq!(state.intent.as_ref().unwrap().status, PaymentStatus::Settled);
    assert_eq!(state.reconcile_attempts, 2);
    assert_eq!(state.events[0].attempt_no, 2);
}

#[test]
fn confirmed_evidence_cannot_exceed_bounded_future_clock_skew() {
    let payment = intent(PaymentStatus::PendingSettlement);
    let evidence = ConfirmedSettlement {
        authority: "facilitator-a".to_owned(),
        asset: payment.asset.clone(),
        recipient: RECIPIENT.to_owned(),
        payer: Some(format!("0x{}", "33".repeat(20))),
        settled_amount: payment.amount,
        posting_key: crate::model::PaymentLedgerEntry::settlement_posting_key(
            "facilitator-a",
            &payment.asset.network,
            Some(TX_HASH),
            Some("settlement-future"),
            Some("log:8"),
        )
        .unwrap(),
        tx_hash: Some(TX_HASH.to_owned()),
        settlement_ref: Some("settlement-future".to_owned()),
        effect_ref: Some("log:8".to_owned()),
        confirmations: 3,
        minimum_confirmations: 1,
        effective_at: 161,
    };

    assert!(matches!(
        evidence.validate_for(&payment, 100),
        Err(Error::Settlement(reason)) if reason.contains("bounded clock skew")
    ));
}

#[tokio::test]
async fn submitted_verify_recovery_advances_to_pending_settlement() {
    let storage = Arc::new(MockStorage::default());
    storage.state.lock().unwrap().intent = Some(intent(PaymentStatus::Submitted));
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::from([Ok(ReconciliationEvidence::Verified {
            verification_ref: "verification-1".to_owned(),
        })])),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker = job(Arc::clone(&storage), evidence, clock, "worker-a");

    let run = worker.run_once().await.unwrap();

    assert_eq!(run.verified, 1);
    let state = storage.state.lock().unwrap();
    assert_eq!(state.intent.as_ref().unwrap().status, PaymentStatus::PendingSettlement);
    assert_eq!(state.intent.as_ref().unwrap().next_reconcile_at, Some(105));
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.events[0].event_type, PaymentEventType::CredentialVerified);
    assert_eq!(state.events[0].attempt_no, 1);
    assert!(state.ledger.is_empty());
}

#[tokio::test]
async fn expired_inbound_challenge_releases_reserved_capacity() {
    let storage = Arc::new(MockStorage::default());
    let mut challenge = intent(PaymentStatus::Created);
    challenge.direction = PaymentDirection::Inbound;
    challenge.offer_id = Some("offer-1".to_owned());
    challenge.expires_at = Some(99);
    storage.state.lock().unwrap().intent = Some(challenge);
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::new()),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker = job(Arc::clone(&storage), evidence, clock, "worker-a");

    let run = worker.run_once().await.unwrap();

    assert_eq!(run.expired, 1);
    let state = storage.state.lock().unwrap();
    let expired = state.intent.as_ref().unwrap();
    assert_eq!(expired.status, PaymentStatus::Expired);
    assert_eq!(expired.reservation_status, ReservationStatus::Released);
    assert_eq!(state.events[0].event_type, PaymentEventType::Failed);
    assert_eq!(state.events[0].attempt_no, 1);
    assert!(state.ledger.is_empty());
}

#[tokio::test]
async fn proven_expired_inbound_authorization_releases_reserved_capacity() {
    let storage = Arc::new(MockStorage::default());
    let mut payment = intent(PaymentStatus::PendingSettlement);
    payment.direction = PaymentDirection::Inbound;
    payment.expires_at = Some(99);
    storage.state.lock().unwrap().intent = Some(payment);
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::from([Ok(ReconciliationEvidence::Expired {
            reason: "signed authorization expired without an on-chain effect".to_owned(),
        })])),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker = job(Arc::clone(&storage), evidence, clock, "worker-a");

    let run = worker.run_once().await.unwrap();

    assert_eq!(run.expired, 1);
    let state = storage.state.lock().unwrap();
    let expired = state.intent.as_ref().unwrap();
    assert_eq!(expired.status, PaymentStatus::Expired);
    assert_eq!(expired.reservation_status, ReservationStatus::Released);
    assert_eq!(state.events[0].event_type, PaymentEventType::Failed);
    assert_eq!(state.events[0].attempt_no, 1);
    assert_eq!(state.events[0].payload["stage"], "settlement_authorization_expiry");
    assert!(state.ledger.is_empty());
}

#[tokio::test]
async fn unknown_and_pending_evidence_use_persistent_bounded_backoff() {
    let storage = Arc::new(MockStorage::default());
    storage.state.lock().unwrap().intent = Some(intent(PaymentStatus::Unknown));
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::from([
            Ok(ReconciliationEvidence::Unknown {
                reason: "RPC quorum unavailable".to_owned(),
            }),
            Ok(ReconciliationEvidence::Pending),
            Ok(ReconciliationEvidence::Pending),
        ])),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker = job(Arc::clone(&storage), evidence, Arc::clone(&clock), "worker-a");

    assert_eq!(worker.run_once().await.unwrap().unknown, 1);
    clock.set(105);
    assert_eq!(worker.run_once().await.unwrap().pending, 1);
    clock.set(115);
    assert_eq!(worker.run_once().await.unwrap().pending, 1);

    assert_eq!(storage.state.lock().unwrap().releases, vec![105, 115, 135]);
}

#[tokio::test]
async fn provider_error_is_retried_without_settlement_or_reservation_release() {
    let storage = Arc::new(MockStorage::default());
    storage.state.lock().unwrap().intent = Some(intent(PaymentStatus::PendingSettlement));
    let evidence = Arc::new(MockEvidence {
        outcomes: Mutex::new(VecDeque::from([Err(Error::Settlement("provider timeout".to_owned()))])),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker = job(Arc::clone(&storage), evidence, clock, "worker-a");

    let run = worker.run_once().await.unwrap();

    assert_eq!(run.provider_errors, 1);
    let state = storage.state.lock().unwrap();
    assert_eq!(state.commits, 0);
    assert_eq!(state.releases, vec![105]);
    assert_eq!(
        state.intent.as_ref().unwrap().reservation_status,
        ReservationStatus::Reserved
    );
}

#[tokio::test]
async fn durable_lease_and_due_time_prevent_duplicate_claims() {
    let storage = Arc::new(MockStorage::default());
    storage.state.lock().unwrap().intent = Some(intent(PaymentStatus::PendingSettlement));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let evidence = Arc::new(BlockingEvidence {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let clock = Arc::new(FixedClock(AtomicI64::new(100)));
    let worker_a = job(
        Arc::clone(&storage),
        Arc::clone(&evidence),
        Arc::clone(&clock),
        "worker-a",
    );
    let worker_b = job(storage, evidence, clock, "worker-b");

    let first_run = tokio::spawn(async move { worker_a.run_once().await });
    entered.notified().await;
    assert_eq!(worker_b.run_once().await.unwrap().claimed, 0);
    release.notify_one();
    assert_eq!(first_run.await.unwrap().unwrap().claimed, 1);
}

#[tokio::test]
async fn provider_reconciliation_is_verify_only() {
    let provider = Arc::new(MockProvider::default());
    let source = VerifyOnlyProviderEvidence::new(provider.clone(), Arc::new(MockRequestBuilder));

    let evidence = source
        .lookup(&intent(PaymentStatus::PendingSettlement), 100)
        .await
        .unwrap();

    assert!(matches!(evidence, ReconciliationEvidence::Confirmed(_)));
    assert_eq!(
        provider.operations.lock().unwrap().as_slice(),
        &[SettlementOperation::Verify]
    );
}
