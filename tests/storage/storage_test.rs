use super::*;
use crate::{
    config::{SqliteStorageConfig, StorageConfig, StorageProvider},
    model::{Amount, BudgetUnit, Wallet},
    Error,
};
use std::{sync::Arc, time::Duration};
use tempfile::TempDir;

fn payment_wallet() -> PaymentWallet {
    PaymentWallet {
        wallet: Wallet {
            id: "wallet-1".to_owned(),
            algorithm: "external".to_owned(),
            address: "opaque-recipient".to_owned(),
            public_key: String::new(),
            created_at: 1,
        },
        custody: WalletCustody::External,
        enabled: true,
        active_key_version: None,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        updated_at: 1,
    }
}

#[test]
fn unbound_wallet_needs_no_budget_unit() {
    assert!(payment_wallet().validate_payment_metadata().is_ok());
}

#[test]
fn scalar_wallet_limits_require_one_valid_unit() {
    let mut wallet = payment_wallet();
    wallet.max_per_day = Some(Amount::new(100));
    assert_eq!(wallet.validate_payment_metadata(), Err("budget_unit"));

    wallet.budget_unit = Some(BudgetUnit {
        asset: "USDC".to_owned(),
        decimals: 6,
    });
    wallet.max_per_payment = Some(Amount::new(101));
    assert_eq!(wallet.validate_payment_metadata(), Err("max_per_day"));

    wallet.max_per_payment = Some(Amount::new(50));
    assert!(wallet.validate_payment_metadata().is_ok());
}

#[test]
fn only_local_wallet_with_active_key_is_signing_ready() {
    let mut wallet = payment_wallet();
    assert!(!wallet.signing_ready());
    wallet.active_key_version = Some(1);
    assert_eq!(wallet.validate_payment_metadata(), Err("active_key_version"));
    wallet.custody = WalletCustody::Local;
    assert!(wallet.signing_ready());
    assert!(wallet.validate_payment_metadata().is_ok());
    wallet.enabled = false;
    assert!(!wallet.signing_ready());
}

#[test]
fn local_wallet_must_match_payment_network_before_signing() {
    let mut wallet = payment_wallet();
    wallet.enabled = true;
    wallet.custody = WalletCustody::Local;
    wallet.active_key_version = Some(1);
    wallet.wallet.algorithm = "secp256k1".to_owned();
    wallet.wallet.address = "0xabcdef0123456789abcdef0123456789abcdef01".to_owned();
    assert!(wallet.signing_ready_for_payment_network("eip155:8453"));

    wallet.wallet.algorithm = "ed25519".to_owned();
    assert!(!wallet.signing_ready_for_payment_network("eip155:8453"));
}

fn sqlite_config(directory: &TempDir, instance_key: &str) -> StorageConfig {
    StorageConfig {
        instance_key: instance_key.to_owned(),
        provider: StorageProvider::Sqlite,
        sqlite: SqliteStorageConfig {
            database_file: directory
                .path()
                .join(format!("{instance_key}.sqlite3"))
                .display()
                .to_string(),
            max_connections: 2,
            busy_timeout: Duration::from_secs(2),
        },
        ..StorageConfig::default()
    }
}

#[tokio::test]
async fn factory_selects_reuses_and_closes_the_sqlite_provider() {
    let directory = TempDir::new().unwrap();
    let config = sqlite_config(&directory, "primary");
    let factory = StorageFactory::new();
    let storage = factory.create(&config).await.unwrap();

    assert_eq!(storage.provider(), StorageProvider::Sqlite);
    storage.ping().await.unwrap();
    let reused = factory.create(&config).await.unwrap();
    assert!(Arc::ptr_eq(&storage, &reused));
    assert!(Arc::ptr_eq(&storage, &factory.get("primary").await.unwrap()));

    factory.close("primary").await.unwrap();
    assert!(storage.ping().await.is_err());
    factory.close("primary").await.unwrap();
    assert!(matches!(factory.get("primary").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn factory_rejects_same_key_with_different_storage_configuration() {
    let directory = TempDir::new().unwrap();
    let config = sqlite_config(&directory, "primary");
    let factory = StorageFactory::new();
    factory.create(&config).await.unwrap();

    let mut conflicting = config;
    conflicting.sqlite.max_connections += 1;
    assert!(matches!(factory.create(&conflicting).await, Err(Error::Config(message)) if message.contains("different")));
    factory.shutdown_all().await.unwrap();
}

#[tokio::test]
async fn initialization_failure_rolls_back_storage_registration() {
    let factory = StorageFactory::new();
    let config = StorageConfig {
        instance_key: "cannot-open".to_owned(),
        sqlite: SqliteStorageConfig {
            database_file: "/proc/agentvault/cannot-open.sqlite3".to_owned(),
            ..SqliteStorageConfig::default()
        },
        ..StorageConfig::default()
    };

    assert!(factory.create(&config).await.is_err());
    assert!(matches!(factory.get("cannot-open").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn shutdown_all_unregisters_every_storage_instance() {
    let first_directory = TempDir::new().unwrap();
    let second_directory = TempDir::new().unwrap();
    let factory = StorageFactory::new();
    factory
        .create(&sqlite_config(&first_directory, "primary"))
        .await
        .unwrap();
    factory
        .create(&sqlite_config(&second_directory, "secondary"))
        .await
        .unwrap();

    factory.shutdown_all().await.unwrap();
    factory.shutdown_all().await.unwrap();
    assert!(matches!(factory.get("primary").await, Err(Error::NotFound(_))));
    assert!(matches!(factory.get("secondary").await, Err(Error::NotFound(_))));
}
