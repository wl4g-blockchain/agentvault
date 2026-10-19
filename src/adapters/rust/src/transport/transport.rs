use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::BTreeMap, error, fmt, future::Future, sync::Arc};

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
                code: "TRANSPORT_ERROR".to_owned(),
                message: format!("AgentVault returned status {}", response.status),
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

#[cfg(test)]
#[path = "../../tests/transport/transport_test.rs"]
mod tests;
