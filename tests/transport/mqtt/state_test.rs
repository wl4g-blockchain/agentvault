use super::*;
#[tokio::test]
async fn coalesces_exact_duplicates_and_conflicts_on_reuse() {
    let backend = Arc::new(crate::cache::MemoryCache::new(16, 16 * 1024, RESPONSE_CACHE_TTL).unwrap());
    backend.init().await.unwrap();
    let cache = ResponseCache::new(backend);
    let first_hash = ResponseCache::digest(b"first");
    assert!(matches!(
        cache.begin("client-a", "0123456789abcdef", first_hash).await,
        CacheDisposition::New
    ));
    assert!(matches!(
        cache.begin("client-a", "0123456789abcdef", first_hash).await,
        CacheDisposition::InFlight
    ));
    let response = MqttWireCodec::error_response("0123456789abcdef", 400, "INVALID_REQUEST", "invalid");
    cache.complete("client-a", "0123456789abcdef", response).await;
    assert!(matches!(
        cache.begin("client-a", "0123456789abcdef", first_hash).await,
        CacheDisposition::Cached(_)
    ));
    assert!(matches!(
        cache
            .begin("client-a", "0123456789abcdef", ResponseCache::digest(b"different"))
            .await,
        CacheDisposition::Conflict
    ));
}
