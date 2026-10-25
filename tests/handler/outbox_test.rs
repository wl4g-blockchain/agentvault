use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::storage::OutboxStatus;

#[derive(Default)]
struct FakeStorage {
    messages: Mutex<Vec<OutboxMessage>>,
    requests: Mutex<Vec<OutboxLeaseRequest>>,
    transitions: Mutex<Vec<String>>,
}

#[async_trait]
impl OutboxStorage for FakeStorage {
    async fn claim(&self, request: &OutboxLeaseRequest) -> Result<Vec<OutboxMessage>> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.messages.lock().unwrap().clone())
    }

    async fn mark_delivered(&self, message_id: &str, _owner: &str, _delivered_at: i64) -> Result<()> {
        self.transitions.lock().unwrap().push(format!("delivered:{message_id}"));
        Ok(())
    }

    async fn reschedule(&self, message_id: &str, _owner: &str, _available_at: i64, _error: &str) -> Result<()> {
        self.transitions.lock().unwrap().push(format!("retry:{message_id}"));
        Ok(())
    }

    async fn mark_dead_letter(&self, message_id: &str, _owner: &str, _error: &str) -> Result<()> {
        self.transitions.lock().unwrap().push(format!("dead:{message_id}"));
        Ok(())
    }
}

struct TestSink;

#[async_trait]
impl PaymentEventSink for TestSink {
    async fn deliver(&self, message: &OutboxMessage) -> PaymentEventDelivery {
        match message.event_type.as_str() {
            "settled" => PaymentEventDelivery::Delivered,
            "retry" => PaymentEventDelivery::Retryable {
                reason: "provider unavailable".to_owned(),
            },
            _ => PaymentEventDelivery::PermanentFailure {
                reason: "unsupported event".to_owned(),
            },
        }
    }
}

fn config() -> PaymentEventDispatcherConfig {
    PaymentEventDispatcherConfig {
        worker_id: "payment-events:test".to_owned(),
        batch_size: 10,
        lease_duration: Duration::from_secs(30),
        delivery_timeout: Duration::from_secs(2),
        poll_interval: Duration::from_secs(1),
        initial_backoff: Duration::from_secs(2),
        max_backoff: Duration::from_secs(30),
        max_attempts: 3,
    }
}

fn message(id: &str, event_type: &str, attempt_count: u32) -> OutboxMessage {
    OutboxMessage {
        id: id.to_owned(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: "intent-1".to_owned(),
        event_type: event_type.to_owned(),
        destination: "scenario:scenario-1.payments".to_owned(),
        run_id: "run-1".to_owned(),
        payload: json!({"intent_id": "intent-1"}),
        status: OutboxStatus::Pending,
        attempt_count,
        available_at: 10,
        lease_owner: Some("payment-events:test".to_owned()),
        lease_expires_at: Some(40),
        last_error: None,
        created_at: 10,
        delivered_at: None,
    }
}

#[tokio::test]
async fn claims_only_payment_class_and_persists_each_sink_disposition() {
    let storage = Arc::new(FakeStorage::default());
    storage.messages.lock().unwrap().extend([
        message("delivered", "settled", 1),
        message("retry", "retry", 1),
        message("permanent", "unsupported", 1),
        message("exhausted", "retry", 3),
    ]);
    let dispatcher = PaymentEventDispatcher::new(storage.clone(), Arc::new(TestSink), config()).unwrap();
    let run = dispatcher.run_once(20).await.unwrap();

    assert_eq!(
        run,
        PaymentEventDispatchRun {
            claimed: 4,
            delivered: 1,
            rescheduled: 1,
            dead_lettered: 2,
        }
    );
    assert_eq!(
        storage.requests.lock().unwrap()[0].class,
        OutboxDeliveryClass::PaymentEvents
    );
    assert_eq!(
        *storage.transitions.lock().unwrap(),
        ["delivered:delivered", "retry:retry", "dead:permanent", "dead:exhausted"]
    );
}

#[tokio::test]
async fn foreign_class_from_adapter_fails_without_acknowledgement() {
    let storage = Arc::new(FakeStorage::default());
    let mut foreign = message("approval", "approval_required", 1);
    foreign.destination = "client:client-1.approvals".to_owned();
    storage.messages.lock().unwrap().push(foreign);
    let dispatcher = PaymentEventDispatcher::new(storage.clone(), Arc::new(TestSink), config()).unwrap();

    assert!(matches!(dispatcher.run_once(20).await, Err(Error::Storage(_))));
    assert!(storage.transitions.lock().unwrap().is_empty());
}

#[test]
fn validates_configuration_and_classifies_destinations_exactly() {
    let mut invalid = config();
    invalid.max_attempts = 0;
    assert!(PaymentEventDispatcher::new(Arc::new(FakeStorage::default()), Arc::new(TestSink), invalid).is_err());

    let approval = OutboxMessage {
        destination: "client:client-1.approvals".to_owned(),
        event_type: "approval_required".to_owned(),
        ..message("approval", "approval_required", 0)
    };
    assert!(OutboxDeliveryClass::ApprovalRequired.matches(&approval));
    assert!(!OutboxDeliveryClass::PaymentEvents.matches(&approval));
    assert!(OutboxDeliveryClass::PaymentEvents.matches(&message("settled", "settled", 0)));
}
