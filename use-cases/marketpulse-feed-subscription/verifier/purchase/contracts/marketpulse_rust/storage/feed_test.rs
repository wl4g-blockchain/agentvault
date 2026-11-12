use std::time::{SystemTime, UNIX_EPOCH};

use super::FeedSqliteRepository;

#[test]
fn initializes_exactly_one_hundred_realistic_feed_rows() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("marketpulse-rust-{nonce}.sqlite3"));
    let storage = FeedSqliteRepository::open(path.to_str().unwrap()).unwrap();
    assert_eq!(storage.yesterday_news(100).unwrap().len(), 50);
    assert_eq!(storage.realtime_news(100).unwrap().len(), 50);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn task_payment_scope_survives_repository_reopen() {
    use agentvault_sdk::A2aTaskStore;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("marketpulse-rust-a2a-{nonce}.sqlite3"));
    let repository = FeedSqliteRepository::open(path.to_str().unwrap()).unwrap();
    let task = serde_json::from_value(
        serde_json::json!({"kind":"task", "id":"task-1", "contextId":"context-1",
        "status":{"state":"submitted"}, "metadata":{"agentvault.payment.run_id":"run-1"}}),
    )
    .unwrap();
    repository.save(&task).await.unwrap();
    drop(repository);
    let repository = FeedSqliteRepository::open(path.to_str().unwrap()).unwrap();
    let task = repository.get("task-1").await.unwrap().unwrap();
    assert_eq!(task.context_id, "context-1");
    assert_eq!(task.metadata.unwrap()["agentvault.payment.run_id"], "run-1");
    drop(repository);
    std::fs::remove_file(path).unwrap();
}
