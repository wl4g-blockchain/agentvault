use crate::config::{SqliteStorageConfig, StorageConfig, StorageProvider};
use crate::storage::*;
use std::time::Duration;
use tempfile::TempDir;

fn sqlite_config(directory: &TempDir, name: &str) -> StorageConfig {
    StorageConfig {
        provider: StorageProvider::Sqlite,
        sqlite: SqliteStorageConfig {
            database_file: directory.path().join(format!("{name}.sqlite3")).display().to_string(),
            max_connections: 2,
            busy_timeout: Duration::from_secs(2),
        },
        ..StorageConfig::default()
    }
}

#[tokio::test]
async fn factory_selects_and_initializes_the_sqlite_provider() {
    let directory = TempDir::new().unwrap();
    let config = sqlite_config(&directory, "primary");
    let factory = StorageFactory::new();
    let storage = factory.create(&config).await.unwrap();

    assert_eq!(storage.provider(), StorageProvider::Sqlite);
    storage.ping().await.unwrap();
    storage.close().await.unwrap();
    assert!(storage.ping().await.is_err());
}

#[tokio::test]
async fn initialization_failure_is_reported_without_retaining_a_provider() {
    let factory = StorageFactory::new();
    let config = StorageConfig {
        sqlite: SqliteStorageConfig {
            database_file: "/proc/agentvault/cannot-open.sqlite3".to_owned(),
            ..SqliteStorageConfig::default()
        },
        ..StorageConfig::default()
    };

    assert!(factory.create(&config).await.is_err());
}

#[tokio::test]
async fn composed_ports_share_lifecycle_and_keep_provider_instances_isolated() {
    let directory = TempDir::new().unwrap();
    let factory = StorageFactory::new();
    let first = factory.create(&sqlite_config(&directory, "first")).await.unwrap();
    let second = factory.create(&sqlite_config(&directory, "second")).await.unwrap();
    let first_ports = first.clone().ports();
    let second_ports = second.clone().ports();
    let mut client = crate::model::PaymentClient {
        id: "same-client-id".to_owned(),
        name: "First instance".to_owned(),
        auth_principal: "first-principal".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    first_ports.clients.insert(&client).await.unwrap();
    assert_eq!(
        first.clone().ports().clients.get(&client.id).await.unwrap(),
        Some(client.clone())
    );
    assert!(second_ports.clients.get(&client.id).await.unwrap().is_none());
    client.name = "Second instance".to_owned();
    client.auth_principal = "second-principal".to_owned();
    second_ports.clients.insert(&client).await.unwrap();

    first.close().await.unwrap();
    first.close().await.unwrap();
    assert!(first_ports.clients.get(&client.id).await.is_err());
    assert!(first_ports.wallets.get("any-wallet").await.is_err());
    assert!(first_ports.payments.get("any-intent").await.is_err());
    assert_eq!(second_ports.clients.get(&client.id).await.unwrap(), Some(client));
    second.close().await.unwrap();
}
