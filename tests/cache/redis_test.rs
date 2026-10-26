use std::time::Duration;

use super::{namespace_key, resolve_urls, retry_delay, validate_config};
use crate::config::{RedisCacheConfig, RedisCacheMode, SecretRefConfig};

#[test]
fn namespaces_are_isolated_single_slot_hashes() {
    let prices = namespace_key("agentvault", "prices").unwrap();
    let receipts = namespace_key("agentvault", "receipts").unwrap();

    assert_eq!(prices, "agentvault:{prices}:entries");
    assert_ne!(prices, receipts);
    assert!(namespace_key("agentvault", "bad:key").is_err());
}

#[test]
fn initial_connection_backoff_is_bounded() {
    let minimum = Duration::from_millis(100);
    let maximum = Duration::from_millis(450);

    assert_eq!(retry_delay(minimum, maximum, 0), Duration::from_millis(100));
    assert_eq!(retry_delay(minimum, maximum, 1), Duration::from_millis(200));
    assert_eq!(retry_delay(minimum, maximum, 2), Duration::from_millis(400));
    assert_eq!(retry_delay(minimum, maximum, 3), maximum);
    assert_eq!(retry_delay(minimum, maximum, u32::MAX), maximum);
}

#[test]
fn direct_adapter_construction_rejects_unsafe_or_ambiguous_configuration() {
    let valid = RedisCacheConfig {
        mode: RedisCacheMode::Standalone,
        urls: vec![SecretRefConfig {
            file: String::new(),
            env: "AGENTVAULT_TEST_REDIS_URL".to_owned(),
        }],
        ..RedisCacheConfig::default()
    };
    assert!(validate_config(&valid).is_ok());

    let mut unsafe_prefix = valid.clone();
    unsafe_prefix.key_prefix = "agentvault:{all}".to_owned();
    assert!(validate_config(&unsafe_prefix).is_err());

    let mut multiple_standalone_urls = valid;
    multiple_standalone_urls.urls.push(SecretRefConfig {
        file: String::new(),
        env: "AGENTVAULT_TEST_REDIS_URL_2".to_owned(),
    });
    assert!(validate_config(&multiple_standalone_urls).is_err());
}

#[test]
fn redis_credentials_are_environment_only_and_cluster_accepts_seed_nodes() {
    let file_backed = RedisCacheConfig {
        urls: vec![SecretRefConfig {
            file: "/run/secrets/redis-url".to_owned(),
            env: String::new(),
        }],
        ..RedisCacheConfig::default()
    };
    assert!(resolve_urls(&file_backed).is_err());

    let clustered = RedisCacheConfig {
        mode: RedisCacheMode::Cluster,
        urls: vec![
            SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT_TEST_REDIS_SEED_1".to_owned(),
            },
            SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT_TEST_REDIS_SEED_2".to_owned(),
            },
        ],
        ..RedisCacheConfig::default()
    };
    assert!(validate_config(&clustered).is_ok());
}
