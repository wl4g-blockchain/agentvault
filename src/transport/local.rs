use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    time::{timeout, Duration},
};

use crate::{secret::ensure_private_dir, Error, Result, WalletService};

const MAX_CONCURRENT_CONNECTIONS: usize = 64;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Serves bounded signing requests on an owner-only Unix socket.
///
/// # Errors
///
/// Returns an error when the socket cannot be prepared, bound, or accepted.
pub async fn run(socket_path: impl AsRef<Path>, service: Arc<WalletService>) -> Result<()> {
    let socket_path = socket_path.as_ref().to_path_buf();
    prepare_socket(&socket_path)?;
    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|source| Error::WriteFile {
        path: socket_path.clone(),
        source,
    })?;
    let _guard = SocketGuard(socket_path.clone());
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    tracing::info!(socket = %socket_path.display(), "local signing transport started");

    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            tracing::warn!("rejecting local signing connection at concurrency limit");
            continue;
        };
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_connection(stream, service).await {
                tracing::warn!(%error, "local signing connection failed");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, service: Arc<WalletService>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let payload = timeout(
        REQUEST_READ_TIMEOUT,
        read_bounded_line(&mut reader, service.max_request_bytes()),
    )
    .await
    .map_err(|_| Error::InvalidRequest("request read timed out".to_owned()))??;
    if payload.is_empty() {
        return Ok(());
    }
    if let Some(response) = service.handle_json(&payload) {
        let mut encoded = serde_json::to_vec(&response)?;
        encoded.push(b'\n');
        writer.write_all(&encoded).await?;
        writer.shutdown().await?;
    }
    Ok(())
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R, limit: usize) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(1024);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(output);
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if output.len() + consumed > limit {
            return Err(Error::InvalidRequest(
                "request exceeds configured byte limit".to_owned(),
            ));
        }
        output.extend_from_slice(&available[..consumed]);
        let complete = available.get(consumed.saturating_sub(1)) == Some(&b'\n');
        reader.consume(consumed);
        if complete {
            output.pop();
            return Ok(output);
        }
    }
}

fn prepare_socket(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent, "local socket directory")?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path).map_err(|source| Error::WriteFile {
            path: path.to_path_buf(),
            source,
        }),
        Ok(_) => Err(Error::Config(format!(
            "local socket path already exists and is not a socket: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::ReadFile {
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use uuid::Uuid;

    use crate::{
        config::SecurityConfig,
        protocol::{ClientPolicy, SignRequest, SignResponse, EIP712_SECP256K1, PROTOCOL_VERSION},
        store::{generate_master_key_file, EncryptedFileStore},
    };

    use super::*;

    #[tokio::test]
    async fn signs_over_unix_socket() {
        let temp = TempDir::new().unwrap();
        let master = temp.path().join("master.key");
        generate_master_key_file(&master).unwrap();
        let store = Arc::new(EncryptedFileStore::open(temp.path().join("store"), master).unwrap());
        store.generate("payer").unwrap();
        let security = SecurityConfig {
            client_policies: vec![ClientPolicy {
                client_id_prefix: "local-".to_owned(),
                wallet_ids: vec!["payer".to_owned()],
                purposes: vec!["x402.payment".to_owned()],
            }],
            ..SecurityConfig::default()
        };
        let service = Arc::new(WalletService::new(store, security));
        let socket = temp.path().join("run/wallet.sock");
        let task = tokio::spawn(run(socket.clone(), service));

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
        let request = SignRequest {
            version: PROTOCOL_VERSION.to_owned(),
            request_id: Uuid::new_v4(),
            client_id: "local-test".to_owned(),
            wallet_id: "payer".to_owned(),
            purpose: "x402.payment".to_owned(),
            scheme: EIP712_SECP256K1.to_owned(),
            digest_b64: BASE64.encode(Sha256::digest(b"payment")),
            issued_at: now,
            expires_at: now + 30,
            metadata: BTreeMap::new(),
        };
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        let mut line = serde_json::to_vec(&request).unwrap();
        line.push(b'\n');
        stream.write_all(&line).await.unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).await.unwrap();
        let response: SignResponse = serde_json::from_str(&response).unwrap();
        assert!(response.error.is_none());
        assert!(response.signature.unwrap().starts_with("0x"));

        task.abort();
    }
}
