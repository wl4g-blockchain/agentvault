#[test]
fn library_facade_exposes_only_cohesive_top_level_modules() {
    let config = agentvault::AppConfig::default();
    let _ = (&config.storage, &config.cache);
}
