use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use fs2::FileExt;
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    crypto,
    protocol::validate_identifier,
    secret::{ensure_private_dir, read_private_file, read_private_text},
    Error, Result,
};

const STORE_VERSION: u8 = 1;
const MASTER_KEY_PREFIX: &str = "wallet-master-key-v1:";
const META_FILE: &str = "store.json";
const KEYS_DIR: &str = "keys";
const LOCK_FILE: &str = ".lock";
const KEK_INFO: &[u8] = b"agent-wallet/kek/v1";
const META_AAD: &[u8] = b"agent-wallet/store-metadata/v1";
const MAX_STORE_FILE_BYTES: usize = 64 * 1024;
const MAX_MASTER_KEY_FILE_BYTES: usize = 256;
const MAX_PRIVATE_KEY_FILE_BYTES: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct KeyInfo {
    pub id: String,
    pub algorithm: String,
    pub address: String,
    pub public_key: String,
    pub created_at: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoreMetadata {
    version: u8,
    kdf: String,
    aead: String,
    salt_b64: String,
    nonce_b64: String,
    encrypted_data_key_b64: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct KeyRecord {
    version: u8,
    id: String,
    algorithm: String,
    address: String,
    public_key: String,
    created_at: i64,
    nonce_b64: String,
    ciphertext_b64: String,
}

pub struct EncryptedFileStore {
    root: PathBuf,
    data_key: Zeroizing<[u8; 32]>,
    mutation_lock: RwLock<()>,
    _process_lock: File,
}

impl EncryptedFileStore {
    /// Opens or initializes one process-exclusive encrypted store.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, an invalid master key, corrupt data,
    /// or an already locked store.
    pub fn open(root: impl AsRef<Path>, master_key_file: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        ensure_private_dir(&root, "wallet store directory")?;
        ensure_private_dir(root.join(KEYS_DIR), "wallet keys directory")?;
        let lock_path = root.join(LOCK_FILE);
        let process_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)
            .map_err(|source| Error::WriteFile {
                path: lock_path.clone(),
                source,
            })?;
        if !process_lock.metadata()?.is_file() {
            return Err(Error::Config("wallet store lock must be a regular file".to_owned()));
        }
        process_lock
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| Error::WriteFile {
                path: lock_path.clone(),
                source,
            })?;
        process_lock.try_lock_exclusive().map_err(|source| {
            Error::Config(format!(
                "wallet store is already open by another process ({}): {source}",
                root.display()
            ))
        })?;

        let master_key = read_master_key(master_key_file)?;
        let metadata_path = root.join(META_FILE);
        let data_key = if metadata_path.exists() {
            decrypt_data_key(&read_json::<StoreMetadata>(&metadata_path)?, &master_key)?
        } else {
            let mut generated = Zeroizing::new([0_u8; 32]);
            OsRng.fill_bytes(generated.as_mut());
            let metadata = encrypt_data_key(&generated, &master_key)?;
            write_json_atomic(&metadata_path, &metadata)?;
            generated
        };

        Ok(Self {
            root,
            data_key,
            mutation_lock: RwLock::new(()),
            _process_lock: process_lock,
        })
    }

    /// Generates and stores a new secp256k1 key.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or existing ID or a storage failure.
    pub fn generate(&self, id: &str) -> Result<KeyInfo> {
        let private_key = crypto::generate_private_key();
        self.insert(id, &private_key)
    }

    /// Imports raw secp256k1 private-key bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid key/ID, duplicate ID, or storage failure.
    pub fn import(&self, id: &str, private_key: &[u8]) -> Result<KeyInfo> {
        crypto::validate_private_key(private_key)?;
        self.insert(id, private_key)
    }

    /// Imports a hex private key from an owner-only regular file.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe file, invalid key, duplicate ID, or
    /// storage failure.
    pub fn import_file(&self, id: &str, path: impl AsRef<Path>) -> Result<KeyInfo> {
        let source = read_private_text(path, "private key file", MAX_PRIVATE_KEY_FILE_BYTES)?;
        let encoded = source.trim().strip_prefix("0x").unwrap_or(source.trim());
        let private_key = Zeroizing::new(
            hex::decode(encoded).map_err(|_| Error::InvalidKey("private key file must contain hex".to_owned()))?,
        );
        self.import(id, &private_key)
    }

    /// Returns public metadata for one key.
    ///
    /// # Errors
    ///
    /// Returns an error when the ID is invalid, missing, or the record is corrupt.
    pub fn get(&self, id: &str) -> Result<KeyInfo> {
        validate_identifier("wallet_id", id)?;
        let _guard = self
            .mutation_lock
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = self.read_record(id)?;
        Ok(record.info())
    }

    /// Lists validated public metadata for all stored keys.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read or a record is corrupt.
    pub fn list(&self) -> Result<Vec<KeyInfo>> {
        let _guard = self
            .mutation_lock
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut keys = Vec::new();
        for entry in fs::read_dir(self.root.join(KEYS_DIR))? {
            let entry = entry?;
            if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|extension| extension == "json") {
                let record = read_json::<KeyRecord>(&entry.path())?;
                Self::validate_record(&record, None)?;
                if self.key_path(&record.id) != entry.path() {
                    return Err(Error::Crypto(
                        "wallet record filename does not match its key ID".to_owned(),
                    ));
                }
                keys.push(record.info());
            }
        }
        keys.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(keys)
    }

    /// Deletes one encrypted key record.
    ///
    /// # Errors
    ///
    /// Returns an error when the ID is invalid/missing or deletion is not durable.
    pub fn delete(&self, id: &str) -> Result<()> {
        validate_identifier("wallet_id", id)?;
        let _guard = self
            .mutation_lock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.key_path(id);
        if !path.exists() {
            return Err(Error::KeyNotFound(id.to_owned()));
        }
        fs::remove_file(&path).map_err(|source| Error::WriteFile {
            path: path.clone(),
            source,
        })?;
        File::open(self.root.join(KEYS_DIR))?.sync_all()?;
        Ok(())
    }

    /// Decrypts one key, signs a digest, and returns its public identity.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid digest/ID, missing key, corrupt record,
    /// or cryptographic failure.
    pub fn sign_digest(&self, id: &str, digest: &[u8]) -> Result<(KeyInfo, [u8; crypto::EOA_SIGNATURE_BYTES])> {
        validate_identifier("wallet_id", id)?;
        let _guard = self
            .mutation_lock
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = self.read_record(id)?;
        let mut private_key = self.decrypt_record(&record)?;
        let signature = crypto::sign_digest(&private_key, digest);
        private_key.zeroize();
        Ok((record.info(), signature?))
    }

    /// Rewraps only the data-key envelope under a replacement master key.
    ///
    /// # Errors
    ///
    /// Returns an error when either master key is unsafe/invalid or the atomic
    /// metadata update fails.
    pub fn rotate_master_key(
        &self,
        current_master_key_file: impl AsRef<Path>,
        new_master_key_file: impl AsRef<Path>,
    ) -> Result<()> {
        let _guard = self
            .mutation_lock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = read_master_key(current_master_key_file)?;
        let metadata_path = self.root.join(META_FILE);
        let stored_data_key = decrypt_data_key(&read_json::<StoreMetadata>(&metadata_path)?, &current)?;
        if stored_data_key.as_ref() != self.data_key.as_ref() {
            return Err(Error::InvalidMasterKey(
                "current key does not unlock this store".to_owned(),
            ));
        }
        let replacement = read_master_key(new_master_key_file)?;
        write_json_atomic(&metadata_path, &encrypt_data_key(&self.data_key, &replacement)?)
    }

    fn insert(&self, id: &str, private_key: &[u8]) -> Result<KeyInfo> {
        validate_identifier("wallet_id", id)?;
        crypto::validate_private_key(private_key)?;
        let _guard = self
            .mutation_lock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.key_path(id);
        if path.exists() {
            return Err(Error::KeyExists(id.to_owned()));
        }

        let info = KeyInfo {
            id: id.to_owned(),
            algorithm: "secp256k1".to_owned(),
            address: crypto::address(private_key)?,
            public_key: crypto::public_key(private_key)?,
            created_at: unix_timestamp(),
        };
        let aad = record_aad(&info);
        let (nonce, ciphertext) = encrypt(&self.data_key, private_key, &aad)?;
        let record = KeyRecord {
            version: STORE_VERSION,
            id: info.id.clone(),
            algorithm: info.algorithm.clone(),
            address: info.address.clone(),
            public_key: info.public_key.clone(),
            created_at: info.created_at,
            nonce_b64: BASE64.encode(nonce),
            ciphertext_b64: BASE64.encode(ciphertext),
        };
        write_json_atomic(&path, &record)?;
        Ok(info)
    }

    fn read_record(&self, id: &str) -> Result<KeyRecord> {
        let path = self.key_path(id);
        if !path.exists() {
            return Err(Error::KeyNotFound(id.to_owned()));
        }
        let record = read_json::<KeyRecord>(&path)?;
        Self::validate_record(&record, Some(id))?;
        Ok(record)
    }

    fn validate_record(record: &KeyRecord, expected_id: Option<&str>) -> Result<()> {
        if record.version != STORE_VERSION
            || record.algorithm != "secp256k1"
            || expected_id.is_some_and(|expected| record.id != expected)
            || validate_identifier("wallet_id", &record.id).is_err()
        {
            return Err(Error::Crypto("wallet record metadata is invalid".to_owned()));
        }
        Ok(())
    }

    fn decrypt_record(&self, record: &KeyRecord) -> Result<Zeroizing<Vec<u8>>> {
        let info = record.info();
        let plaintext = Zeroizing::new(decrypt(
            &self.data_key,
            &decode_nonce(&record.nonce_b64)?,
            &decode_base64("ciphertext", &record.ciphertext_b64)?,
            &record_aad(&info),
        )?);
        crypto::validate_private_key(&plaintext)?;
        if crypto::address(&plaintext)? != record.address || crypto::public_key(&plaintext)? != record.public_key {
            return Err(Error::Crypto(
                "wallet record identity does not match its private key".to_owned(),
            ));
        }
        Ok(plaintext)
    }

    fn key_path(&self, id: &str) -> PathBuf {
        let digest = Sha256::digest(id.as_bytes());
        self.root.join(KEYS_DIR).join(format!("{}.json", hex::encode(digest)))
    }
}

impl KeyRecord {
    fn info(&self) -> KeyInfo {
        KeyInfo {
            id: self.id.clone(),
            algorithm: self.algorithm.clone(),
            address: self.address.clone(),
            public_key: self.public_key.clone(),
            created_at: self.created_at,
        }
    }
}

/// Generates a new owner-only master-key file without overwriting an existing path.
///
/// # Errors
///
/// Returns an error for an unsafe parent/path, random or I/O failure, or when
/// the destination already exists.
pub fn generate_master_key_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(Error::Config("master key parent must be a real directory".to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure_private_dir(parent, "master key directory")?;
            }
            Err(source) => {
                return Err(Error::ReadFile {
                    path: parent.to_path_buf(),
                    source,
                });
            }
        }
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    OsRng.fill_bytes(key.as_mut());
    let value = Zeroizing::new(format!("{MASTER_KEY_PREFIX}{}\n", BASE64.encode(key.as_ref())));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| Error::WriteFile {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(value.as_bytes()).map_err(|source| Error::WriteFile {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| Error::WriteFile {
        path: path.to_path_buf(),
        source,
    })?;
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_master_key(path: impl AsRef<Path>) -> Result<Zeroizing<[u8; 32]>> {
    let source = read_private_text(path, "master key file", MAX_MASTER_KEY_FILE_BYTES)
        .map_err(|error| Error::InvalidMasterKey(error.to_string()))?;
    let encoded = source
        .trim()
        .strip_prefix(MASTER_KEY_PREFIX)
        .ok_or_else(|| Error::InvalidMasterKey(format!("expected {MASTER_KEY_PREFIX}<base64>")))?;
    let decoded = Zeroizing::new(
        BASE64
            .decode(encoded)
            .map_err(|_| Error::InvalidMasterKey("master key is not valid base64".to_owned()))?,
    );
    if decoded.len() != 32 {
        return Err(Error::InvalidMasterKey("master key must decode to 32 bytes".to_owned()));
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(&decoded);
    Ok(Zeroizing::new(key))
}

fn encrypt_data_key(data_key: &[u8; 32], master_key: &[u8; 32]) -> Result<StoreMetadata> {
    let mut salt = [0_u8; 32];
    OsRng.fill_bytes(&mut salt);
    let kek = derive_kek(master_key, &salt)?;
    let (nonce, ciphertext) = encrypt(&kek, data_key, META_AAD)?;
    Ok(StoreMetadata {
        version: STORE_VERSION,
        kdf: "hkdf-sha256".to_owned(),
        aead: "xchacha20poly1305".to_owned(),
        salt_b64: BASE64.encode(salt),
        nonce_b64: BASE64.encode(nonce),
        encrypted_data_key_b64: BASE64.encode(ciphertext),
    })
}

fn decrypt_data_key(metadata: &StoreMetadata, master_key: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>> {
    if metadata.version != STORE_VERSION || metadata.kdf != "hkdf-sha256" || metadata.aead != "xchacha20poly1305" {
        return Err(Error::Crypto("unsupported wallet store format".to_owned()));
    }
    let salt = decode_base64("salt", &metadata.salt_b64)?;
    let kek = derive_kek(master_key, &salt)?;
    let plaintext = Zeroizing::new(
        decrypt(
            &kek,
            &decode_nonce(&metadata.nonce_b64)?,
            &decode_base64("encrypted data key", &metadata.encrypted_data_key_b64)?,
            META_AAD,
        )
        .map_err(|_| Error::InvalidMasterKey("master key does not unlock the wallet store".to_owned()))?,
    );
    if plaintext.len() != 32 {
        return Err(Error::Crypto("decrypted data key has an invalid length".to_owned()));
    }
    let mut data_key = Zeroizing::new([0_u8; 32]);
    data_key.copy_from_slice(&plaintext);
    Ok(data_key)
}

fn derive_kek(master_key: &[u8; 32], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut output = Zeroizing::new([0_u8; 32]);
    Hkdf::<Sha256>::new(Some(salt), master_key)
        .expand(KEK_INFO, output.as_mut())
        .map_err(|_| Error::Crypto("failed to derive key-encryption key".to_owned()))?;
    Ok(output)
}

fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<([u8; 24], Vec<u8>)> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| Error::Crypto("encryption failed".to_owned()))?;
    Ok((nonce, ciphertext))
}

fn decrypt(key: &[u8; 32], nonce: &[u8; 24], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad })
        .map_err(|_| Error::Crypto("authentication or decryption failed".to_owned()))
}

fn record_aad(info: &KeyInfo) -> Vec<u8> {
    format!(
        "agent-wallet/key/v1\0{}\0{}\0{}\0{}",
        info.id, info.algorithm, info.address, info.public_key
    )
    .into_bytes()
}

fn decode_nonce(value: &str) -> Result<[u8; 24]> {
    decode_base64("nonce", value)?
        .try_into()
        .map_err(|_| Error::Crypto("nonce has an invalid length".to_owned()))
}

fn decode_base64(field: &str, value: &str) -> Result<Vec<u8>> {
    BASE64
        .decode(value)
        .map_err(|_| Error::Crypto(format!("{field} is not valid base64")))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let source = read_private_file(path, "wallet store file", MAX_STORE_FILE_BYTES)?;
    serde_json::from_slice(&source).map_err(Error::from)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config(format!("path has no parent: {}", path.display())))?;
    ensure_private_dir(parent, "wallet store directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let data = serde_json::to_vec(value)?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(Error::WriteFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, PathBuf, EncryptedFileStore) {
        let temp = TempDir::new().unwrap();
        let master = temp.path().join("master.key");
        generate_master_key_file(&master).unwrap();
        let store = EncryptedFileStore::open(temp.path().join("store"), &master).unwrap();
        (temp, master, store)
    }

    #[test]
    fn encrypts_and_signs_eoa_key() {
        let (temp, _, store) = store();
        let key = Zeroizing::new(vec![7_u8; 32]);
        let info = store.import("payer", &key).unwrap();
        assert!(info.address.starts_with("0x"));

        let record = fs::read_to_string(store.key_path("payer")).unwrap();
        assert!(!record.contains(&hex::encode(key.as_slice())));

        let digest = Sha256::digest(b"payment");
        let (signed_by, signature) = store.sign_digest("payer", &digest).unwrap();
        assert_eq!(signed_by, info);
        assert_eq!(signature.len(), 65);
        assert_eq!(store.list().unwrap(), vec![info]);

        drop(temp);
    }

    #[test]
    fn wrong_master_key_fails_fast() {
        let (temp, _, store) = store();
        store.generate("payer").unwrap();
        drop(store);

        let wrong = temp.path().join("wrong.key");
        generate_master_key_file(&wrong).unwrap();
        assert!(matches!(
            EncryptedFileStore::open(temp.path().join("store"), wrong),
            Err(Error::InvalidMasterKey(_))
        ));
    }

    #[test]
    fn prevents_concurrent_store_processes() {
        let (temp, master, _store) = store();
        assert!(matches!(
            EncryptedFileStore::open(temp.path().join("store"), master),
            Err(Error::Config(message)) if message.contains("already open")
        ));
    }

    #[test]
    fn rotates_only_master_key_envelope() {
        let (temp, current, store) = store();
        let info = store.generate("payer").unwrap();
        let key_record_before = fs::read(store.key_path("payer")).unwrap();
        let replacement = temp.path().join("replacement.key");
        generate_master_key_file(&replacement).unwrap();
        store.rotate_master_key(&current, &replacement).unwrap();
        assert_eq!(fs::read(store.key_path("payer")).unwrap(), key_record_before);
        drop(store);

        assert!(matches!(
            EncryptedFileStore::open(temp.path().join("store"), current),
            Err(Error::InvalidMasterKey(_))
        ));
        let reopened = EncryptedFileStore::open(temp.path().join("store"), replacement).unwrap();
        assert_eq!(reopened.get("payer").unwrap(), info);
    }

    #[test]
    fn import_file_requires_private_permissions() {
        let (temp, _, store) = store();
        let private_key_file = temp.path().join("payer.hex");
        fs::write(&private_key_file, hex::encode([7_u8; 32])).unwrap();
        fs::set_permissions(&private_key_file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.import_file("payer", &private_key_file).is_err());

        fs::set_permissions(&private_key_file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.import_file("payer", &private_key_file).is_ok());
    }
}
