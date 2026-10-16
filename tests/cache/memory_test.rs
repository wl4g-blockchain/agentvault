use std::sync::Arc;
use std::time::Duration;

use super::*;

#[tokio::test]
async fn expires_caps_ttl_and_evicts_least_recently_used() {
    let expiring = MemoryCache::new(1, 8, Duration::from_millis(10)).unwrap();
    expiring.init().await.unwrap();
    expiring
        .set("offer", "one", vec![1], Duration::from_secs(60))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert_eq!(expiring.get("offer", "one").await.unwrap(), None);

    let cache = MemoryCache::new(2, 8, Duration::from_secs(60)).unwrap();
    cache.init().await.unwrap();
    cache
        .set("offer", "one", vec![1], Duration::from_secs(1))
        .await
        .unwrap();
    cache
        .set("offer", "two", vec![2], Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(cache.get("offer", "one").await.unwrap(), Some(vec![1]));
    cache
        .set("offer", "three", vec![3], Duration::from_secs(1))
        .await
        .unwrap();

    assert_eq!(cache.get("offer", "two").await.unwrap(), None);
    assert_eq!(cache.get("offer", "one").await.unwrap(), Some(vec![1]));
    assert_eq!(cache.get("offer", "three").await.unwrap(), Some(vec![3]));
}

#[tokio::test]
async fn rejects_unsafe_or_oversized_values_and_removes_idempotently() {
    let cache = MemoryCache::new(1, 1, Duration::from_secs(1)).unwrap();
    cache.init().await.unwrap();
    assert!(cache
        .set("offer", "bad:key", vec![1], Duration::from_secs(1))
        .await
        .is_err());
    assert!(cache
        .set("offer", "one", vec![1, 2], Duration::from_secs(1))
        .await
        .is_err());
    assert!(cache.set("offer", "one", vec![1], Duration::ZERO).await.is_err());

    cache
        .set("offer", "one", vec![1], Duration::from_secs(1))
        .await
        .unwrap();
    cache.delete("offer", "one").await.unwrap();
    cache.delete("offer", "one").await.unwrap();
    assert_eq!(cache.get("offer", "one").await.unwrap(), None);
    cache.ping().await.unwrap();
}

#[tokio::test]
async fn set_if_absent_and_take_are_atomic_under_the_memory_lock() {
    let cache = Arc::new(MemoryCache::new(2, 8, Duration::from_secs(30)).unwrap());
    cache.init().await.unwrap();
    let first = Arc::clone(&cache);
    let second = Arc::clone(&cache);
    let (first_inserted, second_inserted) = tokio::join!(
        first.set_if_absent("request", "one", vec![1], Duration::from_secs(10)),
        second.set_if_absent("request", "one", vec![2], Duration::from_secs(10)),
    );

    assert_ne!(first_inserted.unwrap(), second_inserted.unwrap());
    assert!(matches!(
        cache.take("request", "one").await.unwrap(),
        Some(value) if value == [1] || value == [2]
    ));
    assert_eq!(cache.take("request", "one").await.unwrap(), None);
}

#[tokio::test]
async fn clear_is_namespace_scoped_and_close_is_idempotent() {
    let cache = MemoryCache::new(4, 8, Duration::from_secs(30)).unwrap();
    cache.init().await.unwrap();
    cache
        .set("offers", "one", vec![1], Duration::from_secs(10))
        .await
        .unwrap();
    cache
        .set("receipts", "one", vec![2], Duration::from_secs(10))
        .await
        .unwrap();

    cache.clear("offers").await.unwrap();
    assert!(!cache.exists("offers", "one").await.unwrap());
    assert!(cache.exists("receipts", "one").await.unwrap());
    cache.close().await.unwrap();
    cache.close().await.unwrap();
    assert!(cache.ping().await.is_err());
}
