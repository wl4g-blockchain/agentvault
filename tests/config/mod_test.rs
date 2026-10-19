use super::*;

#[test]
fn config_facade_keeps_storage_and_cache_at_the_process_root() {
    let config = AppConfig::default();
    let _ = (&config.storage, &config.cache, &config.agentvault);
}
