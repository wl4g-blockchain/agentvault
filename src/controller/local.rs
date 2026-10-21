//! Unix-socket adapter for payment authorization.
//!
//! The wire format is deliberately small and strict: every JSON envelope is
//! prefixed by one unsigned 32-bit big-endian length. The adapter accepts only
//! the typed payment authorization operation; arbitrary digest signing is not
//! representable on this transport.

use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap},
    fmt, fs, io,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    cmd::{RuntimeComponent, Shutdown},
    config::LocalTransportConfig,
    controller::http::{AuthorizePaymentRequest, AuthorizePaymentResponse, CallerPrincipal, PaymentApi},
    Error,
};
use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::{UnixListener, UnixStream},
    sync::{watch, Mutex, Semaphore},
    task::JoinSet,
};

const AUTHORIZE_PATH: &str = "/api/v1/payments/authorize";
const SOCKET_MODE: u32 = 0o660;
const CREATED_DIRECTORY_MODE: u32 = 0o750;
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_HEADER_COUNT: usize = 16;
const MAX_HEADER_NAME_BYTES: usize = 64;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
const RESPONSE_CACHE_CAPACITY: usize = 4_096;
const RESPONSE_CACHE_TTL: Duration = Duration::from_mins(5);
const OVERLOAD_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const UNKNOWN_REQUEST_SPAN_ID: &str = "0000000000000001";

/// Length-prefixed payment transport protected by Unix ownership and mode.
pub struct LocalController {
    settings: LocalSettings,
    api: Arc<PaymentApi>,
    request_spans: Arc<RequestSpanRegistry>,
}

impl LocalController {
    /// Builds the local transport from already validated application
    /// dependencies. The socket filesystem owner/group is the transport trust
    /// boundary; `AgentVault` performs no duplicate credential validation.
    ///
    /// # Errors
    ///
    /// Returns an error when the local transport settings are unsafe.
    pub fn new(config: LocalTransportConfig, api: Arc<PaymentApi>) -> crate::Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            settings: LocalSettings {
                socket_path: PathBuf::from(config.socket_path),
                max_connections: config.max_connections,
                request_timeout: config.request_timeout,
            },
            api,
            request_spans: Arc::new(RequestSpanRegistry::new()),
        })
    }

    async fn serve(self, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let (listener, _socket_guard) = bind_socket(&self.settings.socket_path).await?;
        let permits = Arc::new(Semaphore::new(self.settings.max_connections));
        let context = ConnectionContext {
            api: Arc::clone(&self.api),
            request_spans: Arc::clone(&self.request_spans),
            request_timeout: self.settings.request_timeout,
        };
        let mut workers = JoinSet::new();

        tracing::info!(
            local.socket = %self.settings.socket_path.display(),
            local.socket_mode = format_args!("{SOCKET_MODE:o}"),
            local.max_connections = self.settings.max_connections,
            "AgentVault Unix-socket payment controller listening"
        );

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.context("accept Unix-socket payment connection")?;
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        let response = error_response(
                            UNKNOWN_REQUEST_SPAN_ID,
                            429,
                            "RATE_LIMITED",
                            "too many concurrent local payment connections",
                        );
                        let _ = tokio::time::timeout(
                            OVERLOAD_WRITE_TIMEOUT,
                            write_response(&mut stream, &response),
                        )
                        .await;
                        continue;
                    };
                    let worker_context = context.clone();
                    let worker_shutdown = shutdown.clone();
                    workers.spawn(async move {
                        let _permit = permit;
                        if let Err(error) = handle_connection(stream, worker_context, worker_shutdown).await {
                            tracing::debug!(error = %error, "Unix-socket payment connection closed with an error");
                        }
                    });
                }
                Some(result) = workers.join_next(), if !workers.is_empty() => {
                    if let Err(error) = result {
                        tracing::error!(error = %error, "Unix-socket payment worker failed");
                    }
                }
            }
        }

        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                tracing::error!(error = %error, "Unix-socket payment worker failed during shutdown");
            }
        }
        Ok(())
    }
}

impl fmt::Debug for LocalController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalController")
            .field("settings", &self.settings)
            .field("api", &"<payment-api>")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl RuntimeComponent for LocalController {
    fn name(&self) -> &'static str {
        "Unix-socket payment controller"
    }

    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()> {
        self.serve(shutdown).await
    }
}

#[derive(Clone, Debug)]
struct LocalSettings {
    socket_path: PathBuf,
    max_connections: usize,
    request_timeout: Duration,
}

#[derive(Clone)]
struct ConnectionContext {
    api: Arc<PaymentApi>,
    request_spans: Arc<RequestSpanRegistry>,
    request_timeout: Duration,
}

async fn handle_connection(
    mut stream: UnixStream,
    context: ConnectionContext,
    mut shutdown: Shutdown,
) -> anyhow::Result<()> {
    loop {
        let frame = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            result = tokio::time::timeout(context.request_timeout, read_frame(&mut stream)) => {
                if let Ok(result) = result {
                    result?
                } else {
                    let response = error_response(
                        UNKNOWN_REQUEST_SPAN_ID,
                        408,
                        "REQUEST_TIMEOUT",
                        "local payment request timed out",
                    );
                    let _ = tokio::time::timeout(
                        context.request_timeout,
                        write_response(&mut stream, &response),
                    )
                    .await;
                    return Ok(());
                }
            }
        };
        let Some(frame) = frame else {
            return Ok(());
        };
        let response = process_frame(&frame, &context).await;
        tokio::time::timeout(context.request_timeout, write_response(&mut stream, &response))
            .await
            .context("write Unix-socket payment response before timeout")??;
    }
}

async fn process_frame(frame: &[u8], context: &ConnectionContext) -> ResponseEnvelope {
    let request_span_id = probe_request_span_id(frame).unwrap_or_else(|| UNKNOWN_REQUEST_SPAN_ID.to_owned());
    let command = match parse_command(frame) {
        Ok(command) => command,
        Err(failure) => {
            return error_response(
                failure.request_span_id.as_deref().unwrap_or(&request_span_id),
                failure.status,
                failure.code,
                failure.message,
            );
        }
    };
    let request_hash = Sha256::digest(frame).into();
    let principal = command.principal.as_str().to_owned();
    match context
        .request_spans
        .begin(&principal, &command.request_span_id, request_hash)
        .await
    {
        RequestSpanDisposition::Cached(response) => response,
        RequestSpanDisposition::Conflict => error_response(
            &command.request_span_id,
            409,
            "PAYMENT_CONFLICT",
            "request span id was reused with different content",
        ),
        RequestSpanDisposition::Full => error_response(
            &command.request_span_id,
            429,
            "RATE_LIMITED",
            "the local request cache is at capacity",
        ),
        RequestSpanDisposition::Wait(mut receiver) => {
            let waited = tokio::time::timeout(context.request_timeout, async {
                loop {
                    if let Some(response) = receiver.borrow().clone() {
                        return Some(response);
                    }
                    if receiver.changed().await.is_err() {
                        return None;
                    }
                }
            })
            .await;
            match waited {
                Ok(Some(response)) => response,
                Ok(None) => error_response(
                    &command.request_span_id,
                    500,
                    "INTERNAL_ERROR",
                    "the correlated payment request was interrupted",
                ),
                Err(_) => error_response(
                    &command.request_span_id,
                    408,
                    "REQUEST_TIMEOUT",
                    "the correlated payment request timed out",
                ),
            }
        }
        RequestSpanDisposition::New => {
            let response = match unix_time() {
                Ok(now) => match tokio::time::timeout(
                    context.request_timeout,
                    context.api.authorize(command.principal.as_str(), &command.request, now),
                )
                .await
                {
                    Ok(Ok(value)) => success_response(&command.request_span_id, &value),
                    Ok(Err(error)) => response_for_error(&command.request_span_id, error),
                    Err(_) => error_response(
                        &command.request_span_id,
                        408,
                        "REQUEST_TIMEOUT",
                        "payment authorization timed out",
                    ),
                },
                Err(error) => response_for_error(&command.request_span_id, error),
            };
            context
                .request_spans
                .complete(&principal, &command.request_span_id, response.clone())
                .await;
            response
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestEnvelope {
    #[serde(rename = "type")]
    message_type: String,
    version: u8,
    request_span_id: String,
    request: WireRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

struct ParsedCommand {
    request_span_id: String,
    principal: CallerPrincipal,
    request: AuthorizePaymentRequest,
}

#[derive(Debug)]
struct ParseFailure {
    request_span_id: Option<String>,
    status: u16,
    code: &'static str,
    message: &'static str,
}

impl ParseFailure {
    fn invalid(request_span_id: Option<String>, message: &'static str) -> Self {
        Self {
            request_span_id,
            status: 400,
            code: "INVALID_REQUEST",
            message,
        }
    }
}

fn parse_command(payload: &[u8]) -> Result<ParsedCommand, ParseFailure> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ParseFailure::invalid(None, "local payment envelope is too large"));
    }
    let envelope: RequestEnvelope = serde_json::from_slice(payload).map_err(|_| {
        ParseFailure::invalid(
            probe_request_span_id(payload),
            "invalid local payment authorization envelope",
        )
    })?;
    let request_span_id = valid_request_span_id(&envelope.request_span_id).then(|| envelope.request_span_id.clone());
    if envelope.message_type != "payment.authorize"
        || envelope.version != 1
        || request_span_id.is_none()
        || envelope.request.method != "POST"
        || envelope.request.path != AUTHORIZE_PATH
    {
        return Err(ParseFailure::invalid(
            request_span_id,
            "invalid local payment authorization envelope contract",
        ));
    }
    let request_span_id = envelope.request_span_id;
    validate_headers(&envelope.request.headers)
        .map_err(|message| ParseFailure::invalid(Some(request_span_id.clone()), message))?;
    let body = decode_canonical_body(&envelope.request.body)
        .map_err(|message| ParseFailure::invalid(Some(request_span_id.clone()), message))?;
    let request: AuthorizePaymentRequest = serde_json::from_slice(&body).map_err(|_| {
        ParseFailure::invalid(
            Some(request_span_id.clone()),
            "invalid local payment authorization request body",
        )
    })?;
    Ok(ParsedCommand {
        request_span_id,
        principal: CallerPrincipal::unrestricted(),
        request,
    })
}

fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), &'static str> {
    if headers.is_empty() || headers.len() > MAX_HEADER_COUNT {
        return Err("invalid local payment authorization headers");
    }
    for (name, value) in headers {
        if name.is_empty()
            || name.len() > MAX_HEADER_NAME_BYTES
            || value.len() > MAX_HEADER_VALUE_BYTES
            || !name.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            || value.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
            || !matches!(name.as_str(), "accept" | "content-type")
        {
            return Err("invalid local payment authorization headers");
        }
    }
    if headers.get("content-type").map(String::as_str) != Some("application/json") {
        return Err("content-type must be application/json");
    }
    if headers.get("accept").is_some_and(|value| value != "application/json") {
        return Err("accept must be application/json");
    }
    Ok(())
}

fn decode_canonical_body(encoded: &str) -> Result<Vec<u8>, &'static str> {
    let max_encoded = MAX_BODY_BYTES.saturating_mul(4).div_ceil(3);
    if encoded.len() > max_encoded || !encoded.bytes().all(is_base64url_byte) {
        return Err("invalid or oversized local payment authorization body");
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "invalid local payment authorization body encoding")?;
    if decoded.len() > MAX_BODY_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err("non-canonical or oversized local payment authorization body");
    }
    Ok(decoded)
}

const fn is_base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn probe_request_span_id(payload: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let request_span_id = value.get("requestSpanId")?.as_str()?;
    valid_request_span_id(request_span_id).then(|| request_span_id.to_owned())
}

fn valid_request_span_id(value: &str) -> bool {
    value != "0000000000000000"
        && value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseEnvelope {
    #[serde(rename = "type")]
    message_type: &'static str,
    version: u8,
    request_span_id: String,
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

fn success_response(request_span_id: &str, response: &AuthorizePaymentResponse) -> ResponseEnvelope {
    match serde_json::to_vec(response) {
        Ok(body) => encoded_response(request_span_id, 200, body),
        Err(_) => error_response(
            request_span_id,
            500,
            "INTERNAL_ERROR",
            "the operation could not be completed",
        ),
    }
}

fn response_for_error(request_span_id: &str, error: Error) -> ResponseEnvelope {
    let (status, code, message) = match error {
        Error::InvalidRequest(_) | Error::PaymentProtocol(_) => (400, "INVALID_REQUEST", error.to_string()),
        Error::Unauthenticated => (401, "UNAUTHORIZED", "a valid bearer token is required".to_owned()),
        Error::Forbidden | Error::PolicyDenied(_) => (403, "FORBIDDEN", error.to_string()),
        Error::NotFound(_) => (404, "NOT_FOUND", error.to_string()),
        Error::PaymentConflict(_) | Error::InvalidTransition(_) => (409, "PAYMENT_CONFLICT", error.to_string()),
        other => {
            tracing::error!(error = %other, "local payment operation failed");
            (500, "INTERNAL_ERROR", "the operation could not be completed".to_owned())
        }
    };
    error_response(request_span_id, status, code, &message)
}

fn error_response(request_span_id: &str, status: u16, code: &'static str, message: &str) -> ResponseEnvelope {
    let body = serde_json::to_vec(&json!({"error": {"code": code, "message": message}})).unwrap_or_else(|_| {
        br#"{"error":{"code":"INTERNAL_ERROR","message":"the operation could not be completed"}}"#.to_vec()
    });
    encoded_response(request_span_id, status, body)
}

fn encoded_response(request_span_id: &str, status: u16, body: Vec<u8>) -> ResponseEnvelope {
    ResponseEnvelope {
        message_type: "payment.authorization_result",
        version: 1,
        request_span_id: request_span_id.to_owned(),
        status,
        headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body: URL_SAFE_NO_PAD.encode(body),
    }
}

async fn read_frame<R>(reader: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0_u8; 4];
    match reader.read(&mut prefix[..1]).await? {
        0 => return Ok(None),
        1 => {
            reader.read_exact(&mut prefix[1..]).await?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local payment frame prefix read exceeded its buffer",
            ))
        }
    }
    let length = usize::try_from(u32::from_be_bytes(prefix)).unwrap_or(usize::MAX);
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid local payment frame length",
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn write_response<W>(writer: &mut W, response: &ResponseEnvelope) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(response).context("encode Unix-socket payment response")?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(anyhow!("Unix-socket payment response exceeds the wire limit"));
    }
    let length = u32::try_from(payload.len()).context("encode Unix-socket payment response length")?;
    writer
        .write_all(&length.to_be_bytes())
        .await
        .context("write Unix-socket payment response length")?;
    writer
        .write_all(&payload)
        .await
        .context("write Unix-socket payment response body")?;
    writer.flush().await.context("flush Unix-socket payment response")
}

enum RequestSpanDisposition {
    New,
    Wait(watch::Receiver<Option<ResponseEnvelope>>),
    Cached(ResponseEnvelope),
    Conflict,
    Full,
}

struct CachedRequest {
    request_hash: [u8; 32],
    response: Option<ResponseEnvelope>,
    completion: watch::Sender<Option<ResponseEnvelope>>,
    updated_at: Instant,
}

struct RequestSpanRegistry {
    entries: Mutex<HashMap<(String, String), CachedRequest>>,
}

impl RequestSpanRegistry {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    async fn begin(&self, principal: &str, request_span_id: &str, request_hash: [u8; 32]) -> RequestSpanDisposition {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, entry| {
            entry.response.is_none() || now.saturating_duration_since(entry.updated_at) <= RESPONSE_CACHE_TTL
        });
        let key = (principal.to_owned(), request_span_id.to_owned());
        if let Some(entry) = entries.get_mut(&key) {
            if entry.request_hash != request_hash {
                return RequestSpanDisposition::Conflict;
            }
            entry.updated_at = now;
            return entry.response.clone().map_or_else(
                || RequestSpanDisposition::Wait(entry.completion.subscribe()),
                RequestSpanDisposition::Cached,
            );
        }
        if entries.len() >= RESPONSE_CACHE_CAPACITY {
            let oldest_completed = entries
                .iter()
                .filter(|(_, entry)| entry.response.is_some())
                .min_by_key(|(_, entry)| entry.updated_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest_completed {
                entries.remove(&oldest);
            } else {
                return RequestSpanDisposition::Full;
            }
        }
        let (completion, _) = watch::channel(None);
        match entries.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(CachedRequest {
                    request_hash,
                    response: None,
                    completion,
                    updated_at: now,
                });
                RequestSpanDisposition::New
            }
            Entry::Occupied(_) => RequestSpanDisposition::Conflict,
        }
    }

    async fn complete(&self, principal: &str, request_span_id: &str, response: ResponseEnvelope) {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get_mut(&(principal.to_owned(), request_span_id.to_owned())) {
            entry.response = Some(response.clone());
            entry.updated_at = Instant::now();
            entry.completion.send_replace(Some(response));
        }
    }
}

async fn bind_socket(path: &Path) -> anyhow::Result<(UnixListener, BoundSocket)> {
    prepare_socket_parent(path)?;
    remove_stale_socket(path).await?;
    let listener = UnixListener::bind(path).with_context(|| format!("bind Unix socket {}", path.display()))?;
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect bound Unix socket {}", path.display()))?;
    if !metadata.file_type().is_socket() {
        return Err(anyhow!("bound local payment endpoint is not a Unix socket"));
    }
    let guard = BoundSocket {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
        .with_context(|| format!("set Unix socket permissions on {}", path.display()))?;
    Ok((listener, guard))
}

fn prepare_socket_parent(path: &Path) -> anyhow::Result<()> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(anyhow!("local payment socket path must be an absolute file path"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("local payment socket path has no parent directory"))?;
    let existed = parent.exists();
    fs::create_dir_all(parent).with_context(|| format!("create Unix socket directory {}", parent.display()))?;
    let metadata =
        fs::symlink_metadata(parent).with_context(|| format!("inspect Unix socket directory {}", parent.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(anyhow!("local payment socket parent must be a real directory"));
    }
    if !existed {
        fs::set_permissions(parent, fs::Permissions::from_mode(CREATED_DIRECTORY_MODE))
            .with_context(|| format!("secure Unix socket directory {}", parent.display()))?;
    }
    Ok(())
}

async fn remove_stale_socket(path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect Unix socket {}", path.display())),
    };
    if !metadata.file_type().is_socket() {
        return Err(anyhow!(
            "refusing to replace non-socket local payment path {}",
            path.display()
        ));
    }
    match tokio::time::timeout(OVERLOAD_WRITE_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Ok(_)) => {
            return Err(anyhow!(
                "refusing to replace active local payment socket {}",
                path.display()
            ));
        }
        Ok(Err(error)) if matches!(error.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound) => {}
        Ok(Err(error)) => {
            return Err(error).with_context(|| format!("probe existing Unix socket {}", path.display()));
        }
        Err(_) => {
            return Err(anyhow!(
                "timed out probing existing local payment socket {}",
                path.display()
            ));
        }
    }
    let current =
        fs::symlink_metadata(path).with_context(|| format!("reinspect stale Unix socket {}", path.display()))?;
    if !current.file_type().is_socket() || current.dev() != metadata.dev() || current.ino() != metadata.ino() {
        return Err(anyhow!(
            "local payment socket changed while checking whether it was stale"
        ));
    }
    fs::remove_file(path).with_context(|| format!("remove stale Unix socket {}", path.display()))
}

struct BoundSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket() && metadata.dev() == self.device && metadata.ino() == self.inode {
            if let Err(error) = fs::remove_file(&self.path) {
                tracing::warn!(
                    error = %error,
                    local.socket = %self.path.display(),
                    "failed to remove owned Unix socket"
                );
            }
        }
    }
}

fn validate_config(config: &LocalTransportConfig) -> crate::Result<()> {
    let path = Path::new(&config.socket_path);
    if !config.enabled
        || config.socket_path.trim().is_empty()
        || config.socket_path.contains('\0')
        || !path.is_absolute()
        || path.file_name().is_none()
        || config.max_connections == 0
        || config.request_timeout.is_zero()
    {
        return Err(Error::Config(
            "enabled local payment transport configuration is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn unix_time() -> crate::Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidRequest("system clock is before the Unix epoch".to_owned()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::InvalidRequest("system clock is out of range".to_owned()))
}

#[cfg(test)]
#[path = "../../tests/controller/local_test.rs"]
mod tests;
