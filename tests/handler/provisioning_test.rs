use std::collections::BTreeMap;
use std::os::unix::fs::symlink;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use tempfile::TempDir;
use uuid::Uuid;

use crate::config::SecretRefConfig;

use super::*;
use crate::{
    config::StorageProvider,
    storage::{sqlite::SqliteStorage, EncryptedWalletKeyVersion},
    utils::aead::{parse_uuid_nonce, WALLET_KEY_ENCRYPTION_SCHEME},
};

fn assert_aes_uuid_envelope(version: &EncryptedWalletKeyVersion) {
    assert_eq!(version.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    assert!(parse_uuid_nonce(&version.wrap_nonce).is_ok());
    assert!(parse_uuid_nonce(&version.key_nonce).is_ok());
}

async fn assert_provisioned_key_is_active(storage: &SqliteStorage, version: &EncryptedWalletKeyVersion) {
    let client = ClientStorage::get(storage, "client").await.unwrap().unwrap();
    assert_eq!(version.activated_at, client.created_at);
    assert!(version.activated_at <= time::OffsetDateTime::now_utc().unix_timestamp());
}

#[test]
fn rejects_duplicate_authority_and_unversioned_manifests() {
    let duplicate = r#"{
          "version": 1,
          "clients": [
            {"id":"a","name":"A","auth_principal":"principal"},
            {"id":"b","name":"B","auth_principal":"principal"}
          ]
        }"#;
    let manifest: ProvisionManifest = serde_json::from_str(duplicate).unwrap();
    assert!(validate_manifest(&manifest).is_err());

    let unsupported: ProvisionManifest = serde_json::from_str(r#"{"version":2}"#).unwrap();
    assert!(validate_manifest(&unsupported).is_err());
}

#[test]
fn limits_require_an_explicit_budget_unit() {
    let body = r#"{
          "version": 1,
          "wallets": [{
            "id":"wallet", "custody":"external", "algorithm":"external",
            "address":"recipient", "max_per_day":"100"
          }]
        }"#;
    let manifest: ProvisionManifest = serde_json::from_str(body).unwrap();
    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn flattened_wallet_source_rejects_inline_keys_and_unknown_fields() {
    for body in [
        r#"{
              "version": 1,
              "wallets": [{
                "id":"wallet", "custody":"local", "private_key_env":"KEY_ENV",
                "private_key":"plaintext-is-forbidden"
              }]
            }"#,
        r#"{
              "version": 1,
              "wallets": [{
                "id":"wallet", "custody":"external", "algorithm":"external",
                "address":"recipient", "unexpected":true
              }]
            }"#,
    ] {
        assert!(serde_json::from_str::<ProvisionManifest>(body).is_err());
    }
}

#[test]
fn manifest_reader_rejects_symbolic_links() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("manifest-target.json");
    let link = temp.path().join("manifest-link.json");
    std::fs::write(&target, r#"{"version":1}"#).unwrap();
    symlink(&target, &link).unwrap();

    assert!(matches!(read_manifest(&link), Err(Error::ReadFile { .. })));
    assert_eq!(read_manifest(&target).unwrap().version, MANIFEST_VERSION);
}

#[test]
fn normalization_is_order_independent_and_duplicates_fail_closed() {
    let body = r#"{
          "version": 1,
          "policies": [{
            "id":"policy", "scenario_id":"scenario",
            "wallet_ids":["wallet-b","wallet-a"],
            "protocols":["x402","mpp"],
            "networks":["tempo:mainnet","eip155:8453"],
            "assets":[
              {"network":"tempo:mainnet","asset":"USDC","decimals":6},
              {"network":"eip155:8453","asset":"USDC","decimals":6}
            ],
            "budget_unit":{"asset":"USDC","decimals":6},
            "outbound_limits":{
              "max_per_payment":"10","max_per_day":"100","approval_enabled":false
            }
          }]
        }"#;
    let mut manifest: ProvisionManifest = serde_json::from_str(body).unwrap();
    normalize_manifest(&mut manifest);
    validate_manifest(&manifest).unwrap();
    assert_eq!(manifest.policies[0].wallet_ids, ["wallet-a", "wallet-b"]);
    assert_eq!(manifest.policies[0].networks, ["eip155:8453", "tempo:mainnet"]);

    manifest.policies[0].protocols.push("x402".to_owned());
    normalize_manifest(&mut manifest);
    assert!(validate_manifest(&manifest).is_err());
}

#[tokio::test]
async fn sqlite_provisioning_is_idempotent_encrypted_and_drift_safe() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("provisioning.sqlite3");
    let manifest_path = temp.path().join("manifest.json");
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

    let manifest = serde_json::json!({
        "version": 1,
        "clients": [{
            "id": "client", "name": "Quant Ingestor", "auth_principal": "principal:quant"
        }],
        "wallets": [{
            "id": "wallet", "custody": "local", "private_key_env": private_key_env.clone(),
            "budget_unit": {"asset": "USDC", "decimals": 6},
            "max_per_payment": "100", "max_per_day": "1000"
        }],
        "scenarios": [{
            "id": "scenario", "client_id": "client",
            "name": "News ingestion"
        }],
        "policies": [{
            "id": "policy", "scenario_id": "scenario", "wallet_ids": ["wallet"],
            "protocols": ["x402"], "networks": ["eip155:8453"],
            "assets": [{
                "network": "eip155:8453", "asset": "USDC",
                "token_contract": "0x1111111111111111111111111111111111111111", "decimals": 6
            }],
            "budget_unit": {"asset": "USDC", "decimals": 6},
            "allowed_recipients": ["0x2222222222222222222222222222222222222222"],
            "allowed_origins": ["https://feed.example"],
            "inbound_limits": {
                "max_per_payment": "100", "max_per_day": "1000", "approval_enabled": false
            },
            "outbound_limits": {
                "max_per_payment": "100", "max_per_day": "1000", "approval_enabled": true,
                "approval_threshold": "50", "approval_overage": "50"
            }
        }],
        "offers": [{
            "id": "offer", "scenario_id": "scenario", "policy_id": "policy",
            "resource": "https://feed.example/mcp/realtime", "protocols": ["x402"],
            "asset": {
                "network": "eip155:8453", "asset": "USDC",
                "token_contract": "0x1111111111111111111111111111111111111111", "decimals": 6
            },
            "amount": "10", "wallet_id": "wallet"
        }],
        "approvers": [{"scenario_id": "scenario", "auth_principal": "principal:admin"}]
    })
    .to_string();
    assert!(!manifest.contains(&hex::encode(private_key)));
    std::fs::write(&manifest_path, &manifest).unwrap();

    let first = provision_from_path(&config, &manifest_path).await.unwrap();
    assert_eq!(first, ProvisioningReport { created: 6, reused: 0 });
    let second = provision_from_path(&config, &manifest_path).await.unwrap();
    assert_eq!(second, ProvisioningReport { created: 0, reused: 6 });

    let storage = SqliteStorage::connect(&config.storage.sqlite).await.unwrap();
    let stored_wallet = storage.get_payment_wallet("wallet").await.unwrap().unwrap();
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
    let versions = storage.list_key_versions("wallet").await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_provisioned_key_is_active(&storage, &versions[0]).await;
    assert_aes_uuid_envelope(&versions[0]);
    assert_ne!(versions[0].key_ciphertext, private_key);
    assert_ne!(versions[0].wrapped_data_key, private_key);

    let drifted = manifest.replace("Quant Ingestor", "Changed Name");
    std::fs::write(&manifest_path, drifted).unwrap();
    assert!(matches!(
        provision_from_path(&config, &manifest_path).await,
        Err(Error::PaymentConflict(_))
    ));
    assert_eq!(
        ClientStorage::get(&storage, "client").await.unwrap().unwrap().name,
        "Quant Ingestor"
    );

    std::env::remove_var(private_key_env);
    std::env::remove_var(kek_env);
}
