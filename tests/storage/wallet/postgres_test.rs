use crate::storage::postgres::tests::*;
use crate::storage::wallet::postgres::PgWalletKeyRepository;
use crate::storage::wallet::postgres::WalletPostgresRepository;
use crate::storage::WalletKeyStatus;
use crate::storage::WalletStorage as _;
use crate::utils::aead::UuidNonceCodec;
use crate::utils::aead::WALLET_KEY_ENCRYPTION_SCHEME;
use crate::Error;

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_URL pointing at a dedicated PostgreSQL database"]
async fn migrates_schema_and_signs_only_through_intent_bound_active_keys() {
    let storage = storage().await;
    let version: i32 = sqlx::query_scalar("SELECT max(version) FROM pay_schema_version")
        .fetch_one(storage.pool())
        .await
        .unwrap();
    assert_eq!(version, 2);

    let repository = PgWalletKeyRepository::from_resolved_keys(
        storage.clone(),
        "test-kek-v1",
        [("test-kek-v1".to_owned(), [9_u8; 32])],
    )
    .unwrap();
    let wallet_id = unique("pg-key-wallet");
    let first = unique_key("pg-key-v1");
    let first_wallet = repository.create_local(&wallet_id, &first, 10).await.unwrap();
    let first_payment_wallet = WalletPostgresRepository::new(storage.clone())
        .get_payment_wallet(&wallet_id)
        .await
        .unwrap()
        .unwrap();
    let first_key = WalletPostgresRepository::new(storage.clone())
        .load_active_key(&wallet_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_key.version, 1);
    assert_eq!(first_key.status, WalletKeyStatus::Active);
    assert_eq!(first_key.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    assert!(UuidNonceCodec::parse(&first_key.wrap_nonce).is_ok());
    assert!(UuidNonceCodec::parse(&first_key.key_nonce).is_ok());
    let first_digest = [0x41_u8; 32];
    let first_signature = sign_fixture(
        &repository,
        &first_payment_wallet,
        &first_key,
        &first_digest,
        &unique("pg-key-v1"),
    )
    .await
    .unwrap();
    assert_eq!(
        recover_address(&first_signature.bytes, &first_digest),
        first_wallet.address
    );

    let second = unique_key("pg-key-v2");
    let rotated = repository.rotate(&wallet_id, &second, 20).await.unwrap();
    assert_ne!(rotated.address, first_wallet.address);
    let rotated_payment_wallet = WalletPostgresRepository::new(storage.clone())
        .get_payment_wallet(&wallet_id)
        .await
        .unwrap()
        .unwrap();
    let active_key = WalletPostgresRepository::new(storage.clone())
        .load_active_key(&wallet_id)
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
            &[0x42_u8; 32],
            &unique("pg-stale-key"),
        )
        .await,
        Err(Error::PaymentConflict(_))
    ));
    let second_digest = [0x43_u8; 32];
    let second_signature = sign_fixture(
        &repository,
        &rotated_payment_wallet,
        &active_key,
        &second_digest,
        &unique("pg-key-v2"),
    )
    .await
    .unwrap();
    assert_eq!(
        recover_address(&second_signature.bytes, &second_digest),
        rotated.address
    );
    let versions = WalletPostgresRepository::new(storage.clone())
        .list_key_versions(&wallet_id)
        .await
        .unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].status, WalletKeyStatus::Retired);
    assert_eq!(versions[1].status, WalletKeyStatus::Active);
    assert_ne!(versions[0].key_ciphertext, first);
}
