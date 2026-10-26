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
