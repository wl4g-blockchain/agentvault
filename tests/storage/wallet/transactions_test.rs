use crate::{
    config::SqliteStorageConfig,
    storage::{sqlite::SqliteStorage, IStorage, WalletCreationConfig},
};

#[tokio::test]
async fn invalid_wallet_limits_do_not_leave_a_partial_wallet() {
    let directory = tempfile::tempdir().unwrap();
    let database = SqliteStorage::connect(&SqliteStorageConfig {
        database_file: directory.path().join("wallet.sqlite").display().to_string(),
        ..SqliteStorageConfig::default()
    })
    .await
    .unwrap();
    let ports = std::sync::Arc::new(database).ports();
    let wallet = crate::model::Wallet {
        id: "rejected".into(),
        address: "0x1111111111111111111111111111111111111111".into(),
        algorithm: "external".into(),
        public_key: String::new(),
        created_at: 1,
    };
    let limits = WalletCreationConfig {
        enabled: true,
        max_per_day: Some(crate::model::Amount::new(1)),
        ..WalletCreationConfig::default()
    };
    assert!(ports
        .wallets
        .create_external_configured(&wallet, &limits)
        .await
        .is_err());
    assert!(ports.wallets.get(&wallet.id).await.unwrap().is_none());
}
