use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use tempfile::TempDir;
use uuid::Uuid;

use crate::{config::SecretRefConfig, model::BudgetUnit};

use super::*;
use crate::config::StorageProvider;
use crate::storage::sqlite::SqliteRepository;
use crate::storage::EncryptedWalletKeyVersion;
use crate::utils::aead::UuidNonceCodec;
use crate::utils::aead::WALLET_KEY_ENCRYPTION_SCHEME;

fn assert_aes_uuid_envelope(version: &EncryptedWalletKeyVersion) {
    assert_eq!(version.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    assert!(UuidNonceCodec::parse(&version.wrap_nonce).is_ok());
    assert!(UuidNonceCodec::parse(&version.key_nonce).is_ok());
}

async fn assert_imported_key_is_active(storage: &SqliteRepository, version: &EncryptedWalletKeyVersion) {
    let client = IClientRepository::get(
        &crate::storage::client::sqlite::ClientSqliteRepository::new(storage.clone()),
        "client",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(version.activated_at, client.created_at);
    assert!(version.activated_at <= time::OffsetDateTime::now_utc().unix_timestamp());
}

fn import_resource_document(private_key_env: &str) -> String {
    format!(
        r"consoleVersion: core.agentvault.io/v1
kind: Client
metadata: {{ name: client }}
data: {{ name: Quant Ingestor, auth_principal: 'principal:quant' }}
---
consoleVersion: core.agentvault.io/v1
kind: Wallet
metadata: {{ name: wallet }}
data:
  custody: local
  private_key_env: {private_key_env}
  budget_unit: {{ asset: USDC, decimals: 6 }}
  max_per_payment: '100'
  max_per_day: '1000'
---
consoleVersion: core.agentvault.io/v1
kind: Scenario
metadata: {{ name: scenario }}
data: {{ client_id: client, name: News ingestion }}
---
consoleVersion: core.agentvault.io/v1
kind: Policy
metadata: {{ name: policy }}
data:
  scenario_id: scenario
  wallet_ids: [wallet]
  protocols: [x402]
  networks: ['eip155:8453']
  assets:
    - {{ network: 'eip155:8453', asset: USDC, token_contract: '0x1111111111111111111111111111111111111111', decimals: 6 }}
  budget_unit: {{ asset: USDC, decimals: 6 }}
  allowed_recipients: ['0x2222222222222222222222222222222222222222']
  allowed_origins: ['https://feed.example']
  inbound_limits: {{ max_per_payment: '100', max_per_day: '1000', approval_enabled: false }}
  outbound_limits: {{ max_per_payment: '100', max_per_day: '1000', approval_threshold: '50', approval_enabled: true, approval_overage: '50' }}
---
consoleVersion: core.agentvault.io/v1
kind: Price
metadata: {{ name: price }}
data:
  scenario_id: scenario
  policy_id: policy
  resource: 'https://feed.example/mcp/realtime'
  protocols: [x402]
  asset: {{ network: 'eip155:8453', asset: USDC, token_contract: '0x1111111111111111111111111111111111111111', decimals: 6 }}
  amount: '10'
  wallet_id: wallet
---
consoleVersion: core.agentvault.io/v1
kind: Approver
metadata: {{ name: scenario-admin }}
data: {{ scenario_id: scenario, auth_principal: 'principal:admin' }}
"
    )
}

fn external_wallet_resource_document() -> &'static str {
    r"consoleVersion: core.agentvault.io/v1
kind: Client
metadata: { name: client }
data: { name: MarketPulse, auth_principal: 'principal:marketpulse' }
---
consoleVersion: core.agentvault.io/v1
kind: Wallet
metadata: { name: revenue-wallet }
data:
  custody: external
  algorithm: secp256k1
  address: '0x2222222222222222222222222222222222222222'
---
consoleVersion: core.agentvault.io/v1
kind: Scenario
metadata: { name: scenario }
data: { client_id: client, name: Paid market feed }
---
consoleVersion: core.agentvault.io/v1
kind: Policy
metadata: { name: policy }
data:
  scenario_id: scenario
  wallet_ids: [revenue-wallet]
  protocols: [x402]
  networks: ['eip155:8453']
  assets:
    - { network: 'eip155:8453', asset: USDC, token_contract: '0x1111111111111111111111111111111111111111', decimals: 6 }
  budget_unit: { asset: USDC, decimals: 6 }
  inbound_limits: { max_per_payment: '100', max_per_day: '1000', approval_enabled: false }
"
}

#[test]
fn rejects_duplicate_authority_and_unsupported_console_versions() {
    let batch = ImportBatch {
        clients: vec![
            ClientSpec {
                id: "a".to_owned(),
                name: "A".to_owned(),
                auth_principal: "principal".to_owned(),
                enabled: true,
            },
            ClientSpec {
                id: "b".to_owned(),
                name: "B".to_owned(),
                auth_principal: "principal".to_owned(),
                enabled: true,
            },
        ],
        ..ImportBatch::default()
    };
    assert!(ResourceImporter::validate_batch(&batch).is_err());

    let temp = TempDir::new().unwrap();
    let resource = temp.path().join("client.yaml");
    std::fs::write(
        &resource,
        "consoleVersion: core.agentvault.io/v2\nkind: Client\nmetadata: { name: client }\ndata: { name: Client, auth_principal: principal }\n",
    )
    .unwrap();
    assert!(ResourceImporter::read_resources(&[resource]).is_err());
}

#[test]
fn limits_require_an_explicit_budget_unit() {
    let temp = TempDir::new().unwrap();
    let resource = temp.path().join("wallet.yaml");
    std::fs::write(
        &resource,
        "consoleVersion: core.agentvault.io/v1\nkind: Wallet\nmetadata: { name: wallet }\ndata: { custody: external, algorithm: external, address: recipient, max_per_day: '100' }\n",
    )
    .unwrap();
    let batch = ResourceImporter::read_resources(&[resource]).unwrap();
    assert!(ResourceImporter::validate_batch(&batch).is_err());
}

#[test]
fn wallet_creation_scope_rejects_mixed_resource_batches() {
    let wallet = WalletSpec {
        id: "wallet".to_owned(),
        enabled: true,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        source: WalletSource::External {
            algorithm: "external".to_owned(),
            address: "recipient".to_owned(),
            public_key: String::new(),
        },
    };
    assert!(ImportBatch {
        wallets: vec![wallet],
        ..ImportBatch::default()
    }
    .contains_only_wallets());
    assert!(!ImportBatch::default().contains_only_wallets());
    assert!(!ImportBatch {
        clients: vec![ClientSpec {
            id: "client".to_owned(),
            name: "Client".to_owned(),
            auth_principal: "principal:client".to_owned(),
            enabled: true,
        }],
        wallets: vec![WalletSpec {
            id: "wallet".to_owned(),
            enabled: true,
            budget_unit: None,
            max_per_payment: None,
            max_per_day: None,
            source: WalletSource::External {
                algorithm: "external".to_owned(),
                address: "recipient".to_owned(),
                public_key: String::new(),
            },
        }],
        ..ImportBatch::default()
    }
    .contains_only_wallets());
}

#[test]
fn normalization_is_order_independent_and_duplicates_fail_closed() {
    let temp = TempDir::new().unwrap();
    let resource = temp.path().join("policy.yaml");
    std::fs::write(
        &resource,
        r"consoleVersion: core.agentvault.io/v1
kind: Policy
metadata: { name: policy }
data:
  scenario_id: scenario
  wallet_ids: [wallet-b, wallet-a]
  protocols: [x402, mpp]
  networks: [tempo:mainnet, eip155:8453]
  assets:
    - { network: tempo:mainnet, asset: USDC, decimals: 6 }
    - { network: 'eip155:8453', asset: USDC, decimals: 6 }
  budget_unit: { asset: USDC, decimals: 6 }
  outbound_limits: { max_per_payment: '10', max_per_day: '100', approval_enabled: false }
",
    )
    .unwrap();
    let mut batch = ResourceImporter::read_resources(&[resource]).unwrap();
    ResourceImporter::normalize_batch(&mut batch);
    ResourceImporter::validate_batch(&batch).unwrap();
    assert_eq!(batch.policies[0].wallet_ids, ["wallet-a", "wallet-b"]);
    assert_eq!(batch.policies[0].networks, ["eip155:8453", "tempo:mainnet"]);

    batch.policies[0].protocols.push("x402".to_owned());
    ResourceImporter::normalize_batch(&mut batch);
    assert!(ResourceImporter::validate_batch(&batch).is_err());
}

#[tokio::test]
async fn sqlite_resource_import_is_idempotent_encrypted_and_drift_safe() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("resource-import.sqlite3");
    let resource_path = temp.path().join("resources.yaml");
    let suffix = Uuid::new_v4().simple().to_string().to_ascii_uppercase();
    let private_key_env = format!("AGENTVAULT_TEST_WALLET_KEY_{suffix}");
    let kek_env = format!("AGENTVAULT_TEST_WALLET_KEK_{suffix}");
    let private_key = [7_u8; 32];
    std::env::set_var(&private_key_env, hex::encode(private_key));
    std::env::set_var(&kek_env, URL_SAFE_NO_PAD.encode([9_u8; 32]));

    let mut config = AppConfig::default();
    config.storage.provider = StorageProvider::Sqlite;
    config.storage.sqlite.database_file = database.to_string_lossy().into_owned();
    config.agentvault.key_management.active_key_id = "test-kek-v1".to_owned();
    config.agentvault.key_management.keys = BTreeMap::from([(
        "test-kek-v1".to_owned(),
        SecretRefConfig {
            file: String::new(),
            env: kek_env.clone(),
        },
    )]);

    let resources = import_resource_document(&private_key_env);
    assert!(!resources.contains(&hex::encode(private_key)));
    std::fs::write(&resource_path, &resources).unwrap();

    let first = ResourceImporter::import_from_paths(&config, std::slice::from_ref(&resource_path))
        .await
        .unwrap();
    assert_eq!(first, ImportReport { created: 6, reused: 0 });
    let second = ResourceImporter::import_from_paths(&config, std::slice::from_ref(&resource_path))
        .await
        .unwrap();
    assert_eq!(second, ImportReport { created: 0, reused: 6 });

    let storage = SqliteRepository::connect(&config.storage.sqlite).await.unwrap();
    let stored_wallet = crate::storage::wallet::sqlite::WalletSqliteRepository::new(storage.clone())
        .get_payment_wallet("wallet")
        .await
        .unwrap()
        .unwrap();
    assert!(stored_wallet.enabled);
    assert_eq!(
        stored_wallet.budget_unit,
        Some(BudgetUnit {
            asset: "USDC".to_owned(),
            decimals: 6,
        })
    );
    assert_eq!(stored_wallet.max_per_payment, Some(Amount::from(100_u128)));
    assert_eq!(stored_wallet.max_per_day, Some(Amount::from(1000_u128)));
    let versions = crate::storage::wallet::sqlite::WalletSqliteRepository::new(storage.clone())
        .list_key_versions("wallet")
        .await
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_imported_key_is_active(&storage, &versions[0]).await;
    assert_aes_uuid_envelope(&versions[0]);
    assert_ne!(versions[0].key_ciphertext, private_key);
    assert_ne!(versions[0].wrapped_data_key, private_key);

    let drifted = resources.replace("Quant Ingestor", "Changed Name");
    std::fs::write(&resource_path, drifted).unwrap();
    assert!(matches!(
        ResourceImporter::import_from_paths(&config, std::slice::from_ref(&resource_path)).await,
        Err(Error::PaymentConflict(_))
    ));
    assert_eq!(
        IClientRepository::get(
            &crate::storage::client::sqlite::ClientSqliteRepository::new(storage.clone()),
            "client"
        )
        .await
        .unwrap()
        .unwrap()
        .name,
        "Quant Ingestor"
    );

    std::env::remove_var(private_key_env);
    std::env::remove_var(kek_env);
}

#[tokio::test]
async fn import_reuses_policy_derived_wallet_budget_unit() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("resource-import.sqlite3");
    let resource_path = temp.path().join("resources.yaml");
    let suffix = Uuid::new_v4().simple().to_string().to_ascii_uppercase();
    let kek_env = format!("AGENTVAULT_TEST_WALLET_KEK_{suffix}");
    std::env::set_var(&kek_env, URL_SAFE_NO_PAD.encode([9_u8; 32]));
    std::fs::write(&resource_path, external_wallet_resource_document()).unwrap();

    let mut config = AppConfig::default();
    config.storage.provider = StorageProvider::Sqlite;
    config.storage.sqlite.database_file = database.to_string_lossy().into_owned();
    config.agentvault.key_management.active_key_id = "test-kek-v1".to_owned();
    config.agentvault.key_management.keys = BTreeMap::from([(
        "test-kek-v1".to_owned(),
        SecretRefConfig {
            file: String::new(),
            env: kek_env.clone(),
        },
    )]);

    assert_eq!(
        ResourceImporter::import_from_paths(&config, std::slice::from_ref(&resource_path))
            .await
            .unwrap(),
        ImportReport { created: 4, reused: 0 }
    );
    let storage = SqliteRepository::connect(&config.storage.sqlite).await.unwrap();
    assert_eq!(
        crate::storage::wallet::sqlite::WalletSqliteRepository::new(storage.clone())
            .get_payment_wallet("revenue-wallet")
            .await
            .unwrap()
            .unwrap()
            .budget_unit,
        Some(BudgetUnit {
            asset: "USDC".to_owned(),
            decimals: 6,
        })
    );
    assert_eq!(
        ResourceImporter::import_from_paths(&config, std::slice::from_ref(&resource_path))
            .await
            .unwrap(),
        ImportReport { created: 0, reused: 4 }
    );
    std::env::remove_var(kek_env);
}
