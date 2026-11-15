use super::WalletKeyRepository;
use crate::storage::wallet::sqlite::WalletSqliteRepository;

#[tokio::test]
async fn shared_custody_rejects_rebound_envelopes_and_keeps_keys_redacted() {
    let (_directory, database) = crate::storage::sqlite::tests::storage().await;
    let custody = WalletKeyRepository::<WalletSqliteRepository>::from_resolved_keys(
        database,
        "test-version",
        [("test-version".into(), [7_u8; 32])],
    )
    .unwrap();
    let private_key = [3_u8; 32];
    let wallet = WalletKeyRepository::<WalletSqliteRepository>::wallet_from_private_key(
        "wallet-test",
        crate::model::wallet::EVM_EOA_ALGORITHM,
        &private_key,
        10,
    )
    .unwrap();
    let envelope = custody.encrypt_version(&wallet, &private_key, 1, 10).unwrap();
    assert_eq!(custody.decrypt_version(&envelope).unwrap().as_slice(), private_key);
    let mut rebound = envelope.clone();
    rebound.wallet_id = "another-wallet".into();
    assert!(custody.decrypt_version(&rebound).is_err());
    rebound = envelope.clone();
    rebound.version += 1;
    assert!(custody.decrypt_version(&rebound).is_err());
    rebound = envelope;
    rebound.key_ciphertext[0] ^= 1;
    assert!(custody.decrypt_version(&rebound).is_err());
    assert!(!format!("{custody:?}").contains(&hex::encode([7_u8; 32])));
}
