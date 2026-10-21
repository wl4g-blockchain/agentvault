use std::time::Duration;

use super::{effective_ttl, validate_component};
use crate::{
    cache::{CacheError, CacheFactory},
    config::{CacheConfig, CacheProvider, SecretRefConfig},
};

#[test]
fn shared_contract_validates_keys_and_caps_ttl() {
    assert!(validate_component("offer.v1").is_ok());
    assert!(validate_component("").is_err());
    assert!(validate_component("unsafe:key").is_err());
    assert_eq!(
        effective_ttl(Duration::from_mins(1), Duration::from_secs(5)).unwrap(),
        Duration::from_secs(5)
    );
    assert!(effective_ttl(Duration::ZERO, Duration::from_secs(5)).is_err());
}

#[tokio::test]
async fn factory_selects_reuses_and_closes_the_default_memory_provider() {
    let factory = CacheFactory::new();
    let config = CacheConfig::default();
    let cache = factory.create(&config).await.unwrap();
    assert_eq!(cache.provider(), CacheProvider::Memory);
    let reused = factory.create(&config).await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&cache, &reused));
    assert!(std::sync::Arc::ptr_eq(&cache, &factory.get("default").await.unwrap()));
    cache
        .set("offer", "one", vec![1, 2], Duration::from_secs(1))
        .await
        .unwrap();

    assert_eq!(cache.get("offer", "one").await.unwrap(), Some(vec![1, 2]));
    assert!(cache.exists("offer", "one").await.unwrap());
    cache.clear("offer").await.unwrap();
    assert!(!cache.exists("offer", "one").await.unwrap());
    cache.ping().await.unwrap();
    factory.close("default").await.unwrap();
    factory.close("default").await.unwrap();
    assert!(matches!(factory.get("default").await, Err(CacheError::NotFound(_))));
    assert!(cache.ping().await.is_err());
}

#[tokio::test]
async fn factory_rejects_same_key_with_different_configuration() {
    let factory = CacheFactory::new();
    let config = CacheConfig::default();
    factory.create(&config).await.unwrap();

    let mut conflicting = config;
    conflicting.memory.max_entries += 1;
    assert!(matches!(
        factory.create(&conflicting).await,
        Err(CacheError::Conflict(key)) if key == "default"
    ));
}

#[tokio::test]
async fn failed_provider_initialization_rolls_back_the_registration() {
    let factory = CacheFactory::new();
    let mut config = CacheConfig {
        instance_key: "unavailable-redis".to_owned(),
        provider: CacheProvider::Redis,
        ..CacheConfig::default()
    };
    config.redis.retry_attempts = 0;
    config.redis.urls = vec![SecretRefConfig {
        file: String::new(),
        env: "AGENTVAULT_TEST_REDIS_URL_MUST_NOT_EXIST".to_owned(),
    }];

    assert!(factory.create(&config).await.is_err());
    assert!(matches!(
        factory.get("unavailable-redis").await,
        Err(CacheError::NotFound(_))
    ));
}

#[tokio::test]
async fn shutdown_all_unregisters_and_closes_every_instance() {
    let factory = CacheFactory::new();
    let first = factory.create(&CacheConfig::default()).await.unwrap();
    let second_config = CacheConfig {
        instance_key: "secondary".to_owned(),
        ..CacheConfig::default()
    };
    let second = factory.create(&second_config).await.unwrap();

    factory.shutdown_all().await.unwrap();
    factory.shutdown_all().await.unwrap();
    assert!(first.ping().await.is_err());
    assert!(second.ping().await.is_err());
}
