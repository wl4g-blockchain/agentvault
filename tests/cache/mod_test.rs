use super::*;

#[tokio::test]
async fn cache_facade_opens_the_default_non_authoritative_backend() {
    let factory = CacheFactory::new();
    let cache = factory.create(&crate::config::CacheConfig::default()).await.unwrap();
    cache.ping().await.unwrap();
    cache.close().await.unwrap();
}
