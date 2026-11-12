use super::*;
use crate::storage::feed::FeedSqliteRepository;

#[tokio::test]
async fn delivery_requires_a_payment_bound_native_task() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("marketpulse-a2a-scope-{nonce}.sqlite3"));
    let handler = FeedHandler::new(FeedSqliteRepository::open(path.to_str().unwrap()).unwrap(), false);
    let request = json!({"jsonrpc":"2.0", "id":1, "method":"message/send", "params":{"message":{
        "kind":"message", "messageId":"request-1", "role":"user", "parts":[{"kind":"text", "text":"news"}]}}});
    let response = FeedHandler::a2a(State(handler), None, HeaderMap::new(), Bytes::from(request.to_string())).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    std::fs::remove_file(path).unwrap();
}
