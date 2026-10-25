use async_trait::async_trait;
use tokio::sync::Mutex;

use super::*;
use crate::{
    cache::MemoryCache,
    storage::{IntentBoundPaymentSignatureRequest, WalletPaymentSignature, WalletPaymentSigner},
    utils::secure_material::Aes256GcmMaterialProtector,
};

#[derive(Default)]
struct CapturingWalletKeys {
    private_key: Mutex<Vec<u8>>,
}

#[async_trait]
impl WalletPaymentSigner for CapturingWalletKeys {
    async fn sign_payment(&self, _request: IntentBoundPaymentSignatureRequest<'_>) -> Result<WalletPaymentSignature> {
        panic!("wallet import test never signs a payment")
    }
}

#[async_trait]
impl WalletKeyManager for CapturingWalletKeys {
    async fn create_local(
        &self,
        wallet_id: &str,
        algorithm: &str,
        private_key: &[u8],
        _config: &WalletCreationConfig,
        created_at: i64,
    ) -> Result<Wallet> {
        *self.private_key.lock().await = private_key.to_vec();
        Ok(Wallet {
            id: wallet_id.to_owned(),
            algorithm: algorithm.to_owned(),
            address: "0x1111111111111111111111111111111111111111".to_owned(),
            public_key: "test-public-key".to_owned(),
            created_at,
        })
    }

    async fn rotate(&self, _wallet_id: &str, _private_key: &[u8], _activated_at: i64) -> Result<Wallet> {
        panic!("wallet import test never rotates a key")
    }
}

async fn test_handler() -> (
    WalletHandler,
    Arc<CapturingWalletKeys>,
    Arc<MemoryCache>,
    Arc<Aes256GcmMaterialProtector>,
) {
    let cache = Arc::new(MemoryCache::new(32, 4096, Duration::from_mins(5)).expect("memory cache"));
    cache.init().await.expect("initialize cache");
    let keys = Arc::new(CapturingWalletKeys::default());
    let protector = Arc::new(
        Aes256GcmMaterialProtector::new("recovery-v1", [("recovery-v1".to_owned(), [9_u8; 32])]).expect("protector"),
    );
    (
        WalletHandler::new(cache.clone(), keys.clone(), protector.clone(), Duration::from_mins(2)),
        keys,
        cache,
        protector,
    )
}

fn encrypted_request(session: &WalletImportSession, wallet_id: &str, private_key: &[u8; 32]) -> CreateEncryptedWallet {
    let server_public_bytes = URL_SAFE_NO_PAD
        .decode(&session.server_public_key)
        .expect("server public key");
    let public_key = PublicEncryptingKey::from_der(&server_public_bytes).expect("RSA SPKI");
    assert_eq!(public_key.key_size_bytes(), RSA_CIPHERTEXT_BYTES);
    let key = OaepPublicEncryptingKey::new(public_key).expect("RSA OAEP");
    let mut envelope = EncryptedWalletPrivateKey {
        session_id: session.session_id.clone(),
        ciphertext: String::new(),
    };
    let label = envelope.label(wallet_id, EVM_EOA_ALGORITHM);
    let mut buffer = [0_u8; RSA_CIPHERTEXT_BYTES];
    let ciphertext = key
        .encrypt(
            &OAEP_SHA256_MGF1SHA256,
            private_key,
            &mut buffer,
            Some(label.as_bytes()),
        )
        .expect("encrypt with public key only");
    envelope.ciphertext = URL_SAFE_NO_PAD.encode(ciphertext);
    CreateEncryptedWallet {
        id: wallet_id.to_owned(),
        algorithm: EVM_EOA_ALGORITHM.to_owned(),
        enabled: true,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        encrypted_private_key: envelope,
    }
}

#[tokio::test]
async fn import_session_decrypts_once_and_never_persists_transport_plaintext() {
    let (handler, keys, cache, protector) = test_handler().await;
    let session = handler
        .create_import_session("operator@example.test", 1_000)
        .await
        .expect("create session");
    assert_eq!(session.expires_at, 1_120);
    assert_eq!(serde_json::to_value(&session).unwrap().as_object().unwrap().len(), 3);
    let cached = cache
        .get(CACHE_NAMESPACE, &session.session_id)
        .await
        .expect("read cached session")
        .expect("cached session");
    let cached_json = String::from_utf8(cached.clone()).expect("cached session UTF-8 JSON");
    let cached: CachedWalletImportSession = serde_json::from_slice(&cached).expect("cached session JSON");
    assert_eq!(cached.version, SESSION_VERSION);
    assert!(!cached.sealed_server_private_key.ciphertext.is_empty());
    assert_ne!(cached.sealed_server_private_key.ciphertext, session.server_public_key);
    let plaintext = protector
        .open(
            &CachedWalletImportSession::context(&session.session_id, "operator@example.test"),
            &cached.sealed_server_private_key,
        )
        .expect("decrypt test session key");
    let temporary_key = PrivateDecryptingKey::from_pkcs8(&plaintext).expect("cached private key");
    assert_eq!(temporary_key.key_size_bytes(), RSA_CIPHERTEXT_BYTES);
    assert!(!cached_json.contains(&URL_SAFE_NO_PAD.encode(plaintext.as_slice())));
    let private_key = [11_u8; 32];
    let request = encrypted_request(&session, "wallet-ui-1", &private_key);
    let encrypted_debug = format!("{:?}", request.encrypted_private_key);
    assert!(!encrypted_debug.contains(&request.encrypted_private_key.ciphertext));
    let replay = request.clone();
    let wallet = handler
        .create_wallet("operator@example.test", request, 1_001)
        .await
        .expect("create wallet");
    assert_eq!(wallet.id, "wallet-ui-1");
    assert_eq!(*keys.private_key.lock().await, private_key);
    assert!(matches!(
        handler.create_wallet("operator@example.test", replay, 1_002).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn import_session_is_principal_bound_and_failed_use_is_consumed() {
    let (handler, _, _, _) = test_handler().await;
    let session = handler
        .create_import_session("first@example.test", 1_000)
        .await
        .expect("create session");
    let request = encrypted_request(&session, "wallet-ui-2", &[12_u8; 32]);
    let retry = request.clone();
    assert!(matches!(
        handler.create_wallet("second@example.test", request, 1_001).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        handler.create_wallet("first@example.test", retry, 1_002).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn authenticated_ciphertext_tampering_fails_closed_and_burns_session() {
    let (handler, _, _, _) = test_handler().await;
    let session = handler
        .create_import_session("operator@example.test", 1_000)
        .await
        .expect("create session");
    let mut request = encrypted_request(&session, "wallet-ui-3", &[13_u8; 32]);
    let retry = request.clone();
    let replacement = if request.encrypted_private_key.ciphertext.starts_with('A') {
        "B"
    } else {
        "A"
    };
    request
        .encrypted_private_key
        .ciphertext
        .replace_range(0..1, replacement);
    assert!(matches!(
        handler.create_wallet("operator@example.test", request, 1_001).await,
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        handler.create_wallet("operator@example.test", retry, 1_002).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn expired_session_is_rejected_at_the_exact_boundary_and_consumed() {
    let (handler, _, _, _) = test_handler().await;
    let session = handler
        .create_import_session("operator@example.test", 1_000)
        .await
        .expect("create session");
    let request = encrypted_request(&session, "wallet-ui-expired", &[14_u8; 32]);
    let replay = request.clone();
    assert!(matches!(
        handler
            .create_wallet("operator@example.test", request, session.expires_at)
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        handler
            .create_wallet("operator@example.test", replay, session.expires_at)
            .await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn ciphertext_is_bound_to_wallet_and_session() {
    let (handler, keys, _, _) = test_handler().await;
    let first = handler.create_import_session("operator", 1_000).await.unwrap();
    let second = handler.create_import_session("operator", 1_000).await.unwrap();
    assert_ne!(first.server_public_key, second.server_public_key);
    let mut request = encrypted_request(&first, "original-wallet", &[15_u8; 32]);
    let mut wrong_wallet = request.clone();
    wrong_wallet.id = "different-wallet".to_owned();
    assert!(matches!(
        handler.create_wallet("operator", wrong_wallet, 1_001).await,
        Err(Error::InvalidRequest(_))
    ));
    request.encrypted_private_key.session_id = second.session_id;
    assert!(matches!(
        handler.create_wallet("operator", request, 1_001).await,
        Err(Error::InvalidRequest(_))
    ));
    assert!(keys.private_key.lock().await.is_empty());
}

#[tokio::test]
async fn concurrent_replays_create_exactly_one_wallet() {
    let (handler, _, _, _) = test_handler().await;
    let session = handler.create_import_session("operator", 1_000).await.unwrap();
    let request = encrypted_request(&session, "concurrent-wallet", &[16_u8; 32]);
    let (first, second) = tokio::join!(
        handler.create_wallet("operator", request.clone(), 1_001),
        handler.create_wallet("operator", request, 1_001),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(matches!(first.err().or(second.err()), Some(Error::NotFound(_))));
}

#[tokio::test]
async fn generation_is_bounded_and_requires_a_principal() {
    let (handler, _, _, _) = test_handler().await;
    assert!(matches!(
        handler.create_import_session("", 1_000).await,
        Err(Error::Unauthenticated)
    ));
    let _busy = handler.key_generation.acquire_many(2).await.unwrap();
    assert!(matches!(
        handler.create_import_session("operator", 1_000).await,
        Err(Error::DependencyUnavailable(_))
    ));
}

#[test]
fn plaintext_and_old_ecdh_envelopes_are_rejected() {
    for extra in ["private_key", "client_public_key", "nonce"] {
        let mut value = serde_json::json!({ "session_id": "session", "ciphertext": "ciphertext" });
        value[extra] = "forbidden".into();
        assert!(serde_json::from_value::<EncryptedWalletPrivateKey>(value).is_err());
    }
    assert!(serde_json::from_value::<CreateEncryptedWallet>(serde_json::json!({
        "id": "wallet", "algorithm": "secp256k1", "private_key": "plaintext"
    }))
    .is_err());
}

// This invokes the production TypeScript module with Node's real Web Crypto,
// then uses the production handler, Cache and SQLite custody implementation.
#[tokio::test]
#[ignore = "requires Node.js 22.6+ for Web Crypto/TypeScript interoperability"]
async fn browser_public_key_encryption_reaches_kek_encrypted_sql() {
    use crate::{
        config::{SecretRefConfig, SqliteStorageConfig, WalletKeyRingConfig},
        storage::{sqlite::SqliteStorage, IStorage as _, WalletStorage as _},
    };

    let directory = tempfile::TempDir::new().unwrap();
    let storage = Arc::new(
        SqliteStorage::connect(&SqliteStorageConfig {
            database_file: directory.path().join("wallet-import.sqlite3").display().to_string(),
            ..SqliteStorageConfig::default()
        })
        .await
        .unwrap(),
    );
    let kek_env = format!("AGENTVAULT_TEST_IMPORT_{}", Uuid::new_v4().simple());
    std::env::set_var(&kek_env, URL_SAFE_NO_PAD.encode([23_u8; 32]));
    let config = WalletKeyRingConfig {
        active_key_id: "import-test-kek".to_owned(),
        keys: [(
            "import-test-kek".to_owned(),
            SecretRefConfig {
                env: kek_env.clone(),
                file: String::new(),
            },
        )]
        .into(),
        ..WalletKeyRingConfig::default()
    };
    let repository = Arc::clone(&storage).wallet_key_manager(&config);
    std::env::remove_var(kek_env);
    let (_, _, cache, protector) = test_handler().await;
    let handler = WalletHandler::new(cache.clone(), repository.unwrap(), protector, Duration::from_mins(2));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let now = i64::try_from(now).unwrap();
    let session = handler.create_import_session("browser-operator", now).await.unwrap();
    let encrypted = browser_encrypt(&session);
    let transport_ciphertext = encrypted.ciphertext.clone();
    let request = CreateEncryptedWallet {
        id: "wallet-browser-sql".to_owned(),
        algorithm: EVM_EOA_ALGORITHM.to_owned(),
        enabled: true,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        encrypted_private_key: encrypted,
    };
    let replay = request.clone();
    let wallet = handler
        .create_wallet("browser-operator", request, now + 1)
        .await
        .unwrap();
    assert_eq!(wallet.address, crate::utils::crypto::address(&[11_u8; 32]).unwrap());
    let versions = storage.list_key_versions(&wallet.id).await.unwrap();
    assert_eq!(versions.len(), 1);
    let version = &versions[0];
    assert_eq!(version.kek_id, "import-test-kek");
    assert_ne!(version.key_ciphertext, [11_u8; 32]);
    assert_ne!(URL_SAFE_NO_PAD.encode(&version.key_ciphertext), transport_ciphertext);
    assert_persisted_key_decrypts_with_kek(version);
    assert!(cache.get(CACHE_NAMESPACE, &session.session_id).await.unwrap().is_none());
    assert!(matches!(
        handler.create_wallet("browser-operator", replay, now + 2).await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(storage.list_key_versions(&wallet.id).await.unwrap().len(), 1);
}

fn browser_encrypt(session: &WalletImportSession) -> EncryptedWalletPrivateKey {
    use std::{
        io::Write as _,
        process::{Command, Stdio},
    };
    let mut child = Command::new("node")
        .args([
            "--experimental-strip-types",
            "--input-type=module",
            "-e",
            r"
            import { encryptWithPublicKey } from './web/src/security/e2ee.ts';
            let input = '';
            for await (const chunk of process.stdin) input += chunk;
            const session = JSON.parse(input);
            const ciphertext = await encryptWithPublicKey(
                session.server_public_key, new Uint8Array(32).fill(11),
                `agentvault.wallet-import.v2\0${session.session_id}\0wallet-browser-sql\0secp256k1`);
            process.stdout.write(JSON.stringify({ session_id: session.session_id, ciphertext }));
        ",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Node.js must be installed for this explicit interoperability gate");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(session).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Web Crypto encryption failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_persisted_key_decrypts_with_kek(version: &crate::storage::EncryptedWalletKeyVersion) {
    use crate::utils::aead::{Aes256GcmUuidNonce, UuidNonce, WALLET_KEY_ENCRYPTION_SCHEME};
    use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};

    assert_eq!(version.encryption_scheme, WALLET_KEY_ENCRYPTION_SCHEME);
    let wrap_aad = [
        b"agentvault.wrap.v1".as_slice(),
        version.aad.as_slice(),
        version.kek_id.as_bytes(),
    ]
    .concat();
    let kek = Aes256GcmUuidNonce::new_from_slice(&[23_u8; 32]).unwrap();
    let data_key = Zeroizing::new(
        kek.decrypt(
            UuidNonce::from_slice(&version.wrap_nonce),
            Payload {
                msg: &version.wrapped_data_key,
                aad: &wrap_aad,
            },
        )
        .unwrap(),
    );
    let cipher = Aes256GcmUuidNonce::new_from_slice(&data_key).unwrap();
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                UuidNonce::from_slice(&version.key_nonce),
                Payload {
                    msg: &version.key_ciphertext,
                    aad: &version.aad,
                },
            )
            .unwrap(),
    );
    assert_eq!(plaintext.as_slice(), &[11_u8; 32]);
    let wrong_kek = Aes256GcmUuidNonce::new_from_slice(&[24_u8; 32]).unwrap();
    assert!(wrong_kek
        .decrypt(
            UuidNonce::from_slice(&version.wrap_nonce),
            Payload {
                msg: &version.wrapped_data_key,
                aad: &wrap_aad,
            }
        )
        .is_err());
}
