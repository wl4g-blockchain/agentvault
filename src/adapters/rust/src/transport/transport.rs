use rand_core::{OsRng, RngCore as _};
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

    fn close(&self) -> impl Future<Output = std::result::Result<(), TransportError>> + Send {
        async { Ok(()) }
    }
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
    pub(crate) async fn close(&self) -> std::result::Result<(), TransportError> {
        self.transport.close().await
    }

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

impl TransportRequest {
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
}

#[cfg(test)]
#[path = "../../tests/transport/transport_test.rs"]
mod tests;

pub(super) struct ProducerTraceContext {
    pub(super) trace_id: String,
    pub(super) span_id: String,
    pub(super) parent_span_id: Option<String>,
    pub(super) headers: BTreeMap<String, String>,
}

impl ProducerTraceContext {
    pub(crate) fn new(headers: &BTreeMap<String, String>) -> ProducerTraceContext {
        let parent = headers
            .get("traceparent")
            .and_then(|value| Self::parse_traceparent(value));
        let trace_id = parent.as_ref().map_or_else(
            || ProducerTraceContext::random_hex(16),
            |(trace_id, _, _)| trace_id.clone(),
        );
        let parent_span_id = parent.as_ref().map(|(_, span_id, _)| span_id.clone());
        let flags = parent.as_ref().map_or("01", |(_, _, flags)| flags.as_str());
        let span_id = ProducerTraceContext::random_hex(8);
        let mut propagated = headers.clone();
        propagated.insert("traceparent".to_owned(), format!("00-{trace_id}-{span_id}-{flags}"));
        ProducerTraceContext {
            trace_id,
            span_id,
            parent_span_id,
            headers: propagated,
        }
    }
    fn parse_traceparent(value: &str) -> Option<(String, String, String)> {
        let mut fields = value.split('-');
        let version = fields.next()?;
        let trace_id = fields.next()?;
        let span_id = fields.next()?;
        let flags = fields.next()?;
        if fields.next().is_some()
            || version != "00"
            || !Self::valid_lower_hex(trace_id, 32)
            || trace_id.bytes().all(|byte| byte == b'0')
            || !Self::valid_lower_hex(span_id, 16)
            || span_id.bytes().all(|byte| byte == b'0')
            || !Self::valid_lower_hex(flags, 2)
        {
            return None;
        }
        Some((trace_id.to_owned(), span_id.to_owned(), flags.to_owned()))
    }
    fn valid_lower_hex(value: &str, length: usize) -> bool {
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }
    pub(crate) fn random_hex(bytes: usize) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut random = vec![0_u8; bytes];
        loop {
            OsRng.fill_bytes(&mut random);
            if random.iter().any(|byte| *byte != 0) {
                break;
            }
        }
        let mut encoded = String::with_capacity(bytes * 2);
        for byte in random {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}
