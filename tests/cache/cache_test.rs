use std::time::Duration;

use super::{effective_ttl, validate_component};
use crate::{
    cache::CacheFactory,
    config::{CacheConfig, CacheProvider, SecretRefConfig},
};

#[test]
fn shared_contract_validates_keys_and_caps_ttl() {
    assert!(validate_component("price.v1").is_ok());
    assert!(validate_component("").is_err());
    assert!(validate_component("unsafe:key").is_err());
    assert_eq!(
        effective_ttl(Duration::from_mins(1), Duration::from_secs(5)).unwrap(),
        Duration::from_secs(5)
    );
    assert!(effective_ttl(Duration::ZERO, Duration::from_secs(5)).is_err());
}

#[tokio::test]
async fn factory_selects_and_initializes_the_default_memory_provider() {
    let factory = CacheFactory::new();
    let config = CacheConfig::default();
    let cache = factory.create(&config).await.unwrap();
    assert_eq!(cache.provider(), CacheProvider::Memory);
    cache
        .set("price", "one", vec![1, 2], Duration::from_secs(1))
        .await
        .unwrap();

    assert_eq!(cache.get("price", "one").await.unwrap(), Some(vec![1, 2]));
    assert!(cache.exists("price", "one").await.unwrap());
    cache.clear("price").await.unwrap();
    assert!(!cache.exists("price", "one").await.unwrap());
    cache.ping().await.unwrap();
    cache.close().await.unwrap();
    cache.close().await.unwrap();
    assert!(cache.ping().await.is_err());
}

#[tokio::test]
async fn failed_provider_initialization_closes_the_component() {
    let factory = CacheFactory::new();
    let mut config = CacheConfig {
        provider: CacheProvider::Redis,
        ..CacheConfig::default()
    };
    config.redis.retry_attempts = 0;
    config.redis.urls = vec![SecretRefConfig {
        file: String::new(),
        env: "AGENTVAULT_TEST_REDIS_URL_MUST_NOT_EXIST".to_owned(),
    }];

    assert!(factory.create(&config).await.is_err());
}
