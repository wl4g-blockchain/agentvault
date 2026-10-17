use std::{collections::BTreeMap, error, fmt, future::Future, sync::Arc, time::Duration};

use reqwest::{redirect::Policy as RedirectPolicy, Client, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{Error, Result as SdkResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TransportRequest {
    pub method: HttpMethod,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("headers", &"<redacted>")
            .field("body", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("headers", &"<redacted>")
            .field("body", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    Other,
    Timeout,
    ResponseTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    kind: TransportErrorKind,
    message: String,
}

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Other,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Timeout,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn response_too_large(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::ResponseTooLarge,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> TransportErrorKind {
        self.kind
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl error::Error for TransportError {}

pub trait VaultTransport: Send + Sync {
    fn send(
        &self,
        request: TransportRequest,
    ) -> impl Future<Output = std::result::Result<TransportResponse, TransportError>> + Send;
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

pub(crate) struct ApiClient<T> {
    transport: Arc<T>,
}

impl<T> Clone for ApiClient<T> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
        }
    }
}

impl<T> ApiClient<T> {
    pub(crate) fn new(transport: T) -> Self {
        Self {
            transport: Arc::new(transport),
        }
    }
}

impl<T: VaultTransport> ApiClient<T> {
    pub(crate) async fn get<R: DeserializeOwned>(&self, path: String) -> SdkResult<R> {
        self.execute(TransportRequest {
            method: HttpMethod::Get,
            path,
            headers: BTreeMap::from([("accept".to_owned(), "application/json".to_owned())]),
            body: Vec::new(),
        })
        .await
    }

    pub(crate) async fn post<B: Serialize, R: DeserializeOwned>(&self, path: String, body: &B) -> SdkResult<R> {
        let headers = BTreeMap::from([
            ("accept".to_owned(), "application/json".to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
        ]);
        let body = serde_json::to_vec(body).map_err(|failure| Error::Encode(failure.to_string()))?;
        self.execute(TransportRequest {
            method: HttpMethod::Post,
            path,
            headers,
            body,
        })
        .await
    }

    async fn execute<R: DeserializeOwned>(&self, request: TransportRequest) -> SdkResult<R> {
        let response = self
            .transport
            .send(request)
            .await
            .map_err(|failure| Error::Transport(failure.to_string()))?;
        if (200..300).contains(&response.status) {
            return serde_json::from_slice(&response.body).map_err(|failure| Error::Decode(failure.to_string()));
        }
        let failure = serde_json::from_slice::<ErrorEnvelope>(&response.body).map_or_else(
            |_| ErrorBody {
                code: "HTTP_ERROR".to_owned(),
                message: format!("AgentVault returned HTTP {}", response.status),
            },
            |envelope| envelope.error,
        );
        if response.status == 409 {
            return Err(Error::Conflict {
                code: failure.code,
                message: failure.message,
            });
        }
        Err(Error::Api {
            status: response.status,
            code: failure.code,
            message: failure.message,
        })
    }
}

pub(crate) fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

const DEFAULT_MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Safe HTTP transport for `AgentVault` control/data-plane APIs.
///
/// `AgentVault` does not authenticate HTTP callers itself. Deployments protected
/// by `AuthGuard` may add an edge credential with
/// [`Self::with_authguard_bearer`]. A request can select only a relative path,
/// redirects are disabled, and responses are bounded.
#[derive(Clone)]
pub struct HttpVaultTransport {
    client: Client,
    base_url: Url,
    authguard_bearer: Option<String>,
    max_response_bytes: usize,
}

impl fmt::Debug for HttpVaultTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpVaultTransport")
            .field("base_url", &self.base_url)
            .field(
                "authguard_bearer",
                &self.authguard_bearer.as_ref().map(|_| "<redacted>"),
            )
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl HttpVaultTransport {
    /// Creates a transport with redirects disabled and bounded network I/O.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-HTTP(S) authority, embedded URL credentials,
    /// query/fragment components, zero limits, or an invalid
    /// HTTP client configuration.
    pub fn new(base_url: &str) -> Result<Self, TransportError> {
        Self::with_limits(
            base_url,
            Duration::from_secs(30),
            Duration::from_secs(5),
            DEFAULT_MAX_RESPONSE_BYTES,
        )
    }

    /// Creates a transport with explicit positive timeout and response limits.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as [`Self::new`].
    pub fn with_limits(
        base_url: &str,
        request_timeout: Duration,
        connect_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, TransportError> {
        let mut base_url = Url::parse(base_url).map_err(|_| TransportError::new("invalid AgentVault base URL"))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(TransportError::new("unsafe AgentVault base URL"));
        }
        if request_timeout.is_zero() || connect_timeout.is_zero() || max_response_bytes == 0 {
            return Err(TransportError::new("AgentVault HTTP transport limits are invalid"));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        let client = Client::builder()
            .redirect(RedirectPolicy::none())
            .timeout(request_timeout)
            .connect_timeout(connect_timeout)
            .build()
            .map_err(|error| TransportError::new(format!("build AgentVault HTTP client: {}", error.without_url())))?;
        Ok(Self {
            client,
            base_url,
            authguard_bearer: None,
            max_response_bytes,
        })
    }

    /// Adds an optional `AuthGuard` edge credential. The reverse proxy consumes
    /// this header; `AgentVault` never parses or validates it.
    pub fn with_authguard_bearer(mut self, token: impl Into<String>) -> Result<Self, TransportError> {
        let token = token.into();
        if token.trim().is_empty() || token.chars().any(char::is_control) {
            return Err(TransportError::new("invalid AuthGuard bearer credential"));
        }
        self.authguard_bearer = Some(token);
        Ok(self)
    }

    fn endpoint(&self, path: &str) -> Result<Url, TransportError> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
            return Err(TransportError::new("AgentVault request path must be origin-relative"));
        }
        let mut endpoint = self.base_url.clone();
        endpoint.set_path("");
        endpoint.set_query(None);
        endpoint
            .join(path)
            .map_err(|_| TransportError::new("invalid AgentVault request path"))
    }
}

impl VaultTransport for HttpVaultTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        let endpoint = self.endpoint(&request.path)?;
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut outbound = self.client.request(method, endpoint);
        if let Some(token) = &self.authguard_bearer {
            outbound = outbound.bearer_auth(token);
        }
        for (name, value) in request.headers {
            if name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("host") {
                return Err(TransportError::new("reserved AgentVault request header"));
            }
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| TransportError::new("invalid AgentVault request header name"))?;
            let value = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| TransportError::new("invalid AgentVault request header value"))?;
            outbound = outbound.header(name, value);
        }
        let mut response = outbound.body(request.body).send().await.map_err(|error| {
            if error.is_timeout() {
                TransportError::timeout("AgentVault HTTP request timed out")
            } else {
                TransportError::new(format!("AgentVault HTTP request failed: {}", error.without_url()))
            }
        })?;
        if response
            .content_length()
            .and_then(|value| usize::try_from(value).ok())
            .is_some_and(|value| value > self.max_response_bytes)
        {
            return Err(TransportError::response_too_large(
                "AgentVault response exceeds configured limit",
            ));
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                value
                    .to_str()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
                    .map_err(|_| TransportError::new("AgentVault response contains a non-text header"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TransportError::new("read AgentVault HTTP response"))?
        {
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > self.max_response_bytes)
            {
                return Err(TransportError::response_too_large(
                    "AgentVault response exceeds configured limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(TransportResponse { status, headers, body })
    }
}

#[cfg(test)]
#[path = "../../tests/transport/http_test.rs"]
mod tests;
