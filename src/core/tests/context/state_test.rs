use super::*;
use std::time::Duration;

#[tokio::test]
async fn provider_views_share_adapters_and_shutdown_keeps_the_original_ports_closed() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.storage.sqlite.database_file = directory.path().join("providers.sqlite3").display().to_string();
    let application = ApplicationScope::isolated();
    let providers = RuntimeProviders::open(&config, Arc::clone(&application)).await.unwrap();
    let cache = application
        .context()
        .get::<dyn ICacheProvider>("cache-default")
        .await
        .unwrap();
    let storage = application
        .context()
        .get::<dyn IStorageProvider>("storage-default")
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&providers.cache, &cache.cache()));
    assert!(Arc::ptr_eq(&providers.storage, &storage.repository()));
    providers
        .cache
        .set("lifecycle", "key", vec![1], Duration::from_secs(10))
        .await
        .unwrap();
    providers.storage.ping().await.unwrap();
    providers.close().await.unwrap();
    assert!(providers.cache.ping().await.is_err());
    assert!(providers.storage.ping().await.is_err());
    providers.close().await.unwrap();
}
