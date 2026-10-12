use std::{
    fs,
    os::unix::fs::{FileTypeExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use async_trait::async_trait;
use tokio::{
    io::{AsyncWriteExt as _, BufReader},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};

use crate::{
    config::LocalConfig,
    framing::read_bounded_line,
    model::sign::SignTransport,
    secret::ensure_private_dir,
    server::{RuntimeComponent, Shutdown},
    service::PaymentSignService,
    Error, Result,
};

/// Owner-only Unix-socket signing endpoint.
pub struct LocalSignServer {
    config: LocalConfig,
    service: Arc<PaymentSignService>,
}

impl LocalSignServer {
    #[must_use]
    pub fn new(config: LocalConfig, service: Arc<PaymentSignService>) -> Self {
        Self { config, service }
    }

    async fn serve(self, mut shutdown: Shutdown) -> Result<()> {
        let socket_path = PathBuf::from(&self.config.socket_path);
        prepare_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|source| Error::WriteFile {
            path: socket_path.clone(),
            source,
        })?;
        let _guard = SocketGuard(socket_path.clone());
        let permits = Arc::new(Semaphore::new(self.config.max_connections));
        let mut connections = JoinSet::new();
        tracing::info!(socket = %socket_path.display(), "local signing endpoint started");

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        tracing::warn!("rejecting local signing connection at concurrency limit");
                        continue;
                    };
                    let service = Arc::clone(&self.service);
                    let request_timeout = self.config.request_timeout;
                    connections.spawn(async move {
                        let _permit = permit;
                        if let Err(error) = handle_connection(stream, service, request_timeout).await {
                            tracing::warn!(%error, "local signing connection failed");
                        }
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::warn!(%error, "local signing connection task failed");
                    }
                }
            }
        }
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                tracing::warn!(%error, "local signing connection task failed during shutdown");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl RuntimeComponent for LocalSignServer {
    fn name(&self) -> &'static str {
        "local signing endpoint"
    }

    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()> {
        self.serve(shutdown).await.context("serve local signing endpoint")
    }
}

async fn handle_connection(
    stream: UnixStream,
    service: Arc<PaymentSignService>,
    request_timeout: std::time::Duration,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let payload = timeout(
        request_timeout,
        read_bounded_line(&mut reader, service.max_request_bytes(), "local signing request"),
    )
    .await
    .map_err(|_| Error::Timeout("local signing request read".to_owned()))??;
    if payload.is_empty() {
        return Ok(());
    }
    if let Some(response) = service.handle_json(&payload, SignTransport::Local) {
        let mut encoded = serde_json::to_vec(&response)?;
        encoded.push(b'\n');
        writer.write_all(&encoded).await?;
        writer.shutdown().await?;
    }
    Ok(())
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
        Err(source) => Err(Error::ReadFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
