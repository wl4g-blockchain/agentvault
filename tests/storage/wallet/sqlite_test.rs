use crate::storage::sqlite::tests::*;
use crate::storage::wallet::sqlite::SqliteWalletKeyRepository;
use crate::storage::wallet::sqlite::WalletSqliteRepository;
use crate::storage::WalletKeyStatus;
use crate::storage::WalletStorage as _;
use crate::utils::aead::UuidNonceCodec;
use crate::utils::aead::WALLET_KEY_ENCRYPTION_SCHEME;
use crate::Error;

#[tokio::test]
async fn encrypted_versions_sign_only_through_intent_bound_active_keys() {
    let (_directory, storage) = storage().await;
    let repository =
        SqliteWalletKeyRepository::from_resolved_keys(storage.clone(), "kek-v1", [("kek-v1".to_owned(), [9_u8; 32])])
            .unwrap();
    let first = [7_u8; 32];
    let wallet = repository.create_local("wallet-1", &first, 10).await.unwrap();
    let first_payment_wallet = WalletSqliteRepository::new(storage.clone())
        .get_payment_wallet("wallet-1")
        .await
        .unwrap()
        .unwrap();
    let first_key = WalletSqliteRepository::new(storage.clone())
        .load_active_key("wallet-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_key.version, 1);
    assert_eq!(first_key.status, WalletKeyStatus::Active);
    assert_eq!(first_key.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    assert!(UuidNonceCodec::parse(&first_key.wrap_nonce).is_ok());
    assert!(UuidNonceCodec::parse(&first_key.key_nonce).is_ok());
    let first_digest = [0x31_u8; 32];
    let first_signature = sign_fixture(
        &repository,
        &first_payment_wallet,
        &first_key,
        &first_digest,
        "sqlite-key-v1",
    )
    .await
    .unwrap();
    assert_eq!(recover_address(&first_signature.bytes, &first_digest), wallet.address);

    let second = [8_u8; 32];
    let rotated = repository.rotate("wallet-1", &second, 20).await.unwrap();
    assert_ne!(rotated.address, wallet.address);
    let rotated_payment_wallet = WalletSqliteRepository::new(storage.clone())
        .get_payment_wallet("wallet-1")
        .await
        .unwrap()
        .unwrap();
    let active_key = WalletSqliteRepository::new(storage.clone())
        .load_active_key("wallet-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_key.version, 2);
    assert_eq!(active_key.status, WalletKeyStatus::Active);
    assert!(matches!(
        sign_fixture(
            &repository,
            &rotated_payment_wallet,
            &first_key,
            &[0x32_u8; 32],
            "sqlite-stale-key",
        )
        .await,
        Err(Error::PaymentConflict(_))
    ));
    let second_digest = [0x33_u8; 32];
    let second_signature = sign_fixture(
        &repository,
        &rotated_payment_wallet,
        &active_key,
        &second_digest,
        "sqlite-key-v2",
    )
    .await
    .unwrap();
    assert_eq!(
        recover_address(&second_signature.bytes, &second_digest),
        rotated.address
    );
    let versions = WalletSqliteRepository::new(storage.clone())
        .list_key_versions("wallet-1")
        .await
        .unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].status, WalletKeyStatus::Retired);
    assert_eq!(versions[1].status, WalletKeyStatus::Active);
    assert_ne!(versions[0].key_ciphertext, first);
}
