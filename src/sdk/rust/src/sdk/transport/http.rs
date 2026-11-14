use crate::transport_core::ProducerTraceContext;
use crate::{HttpConfig, HttpMethod, TransportError, TransportRequest, TransportResponse, VaultTransport};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[derive(Clone, Debug)]
pub struct HttpVaultTransport {
    client: reqwest::Client,
    config: HttpConfig,
    closed: Arc<AtomicBool>,
}
impl HttpVaultTransport {
    pub fn new(mut config: HttpConfig) -> Result<Self, TransportError> {
        let url = reqwest::Url::parse(&config.base_url)
            .map_err(|_| TransportError::new("invalid AgentVault HTTP base URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
            || config.request_timeout.is_zero()
            || config.max_response_bytes == 0
        {
            return Err(TransportError::new("invalid AgentVault HTTP configuration"));
        }
        if let Some(token) = &config.authguard_bearer {
            reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| TransportError::new("invalid AuthGuard bearer"))?;
        }
        let normalized_length = config.base_url.trim_end_matches('/').len();
        config.base_url.truncate(normalized_length);
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| TransportError::new("initialize AgentVault HTTP client"))?;
        Ok(Self {
            client,
            config,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }
}
impl VaultTransport for HttpVaultTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::new("AgentVault HTTP transport is closed"));
        }
        if !request.path.starts_with("/api/v1/")
            || request.path.contains('\\')
            || request.path.split('/').any(|part| part == "..")
            || request.body.len() > self.config.max_response_bytes
        {
            return Err(TransportError::new("invalid AgentVault HTTP request"));
        }
        let trace = ProducerTraceContext::new(&request.headers);
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut call = self
            .client
            .request(method, format!("{}{}", self.config.base_url, request.path))
            .body(request.body);
        for (name, value) in trace.headers {
            call = call.header(name, value);
        }
        if let Some(token) = &self.config.authguard_bearer {
            call = call.bearer_auth(token);
        }
        tracing::info!(event.name="agentvault.sdk.transport.http.produced",trace_id=%trace.trace_id,span_id=%trace.span_id,parent_id=%trace.parent_span_id.unwrap_or_default(),"AgentVault HTTP request");
        let mut response = call.send().await.map_err(|error| {
            if error.is_timeout() {
                TransportError::timeout("AgentVault HTTP request timed out")
            } else {
                TransportError::new("AgentVault HTTP request failed")
            }
        })?;
        if response
            .content_length()
            .is_some_and(|size| size > self.config.max_response_bytes as u64)
        {
            return Err(TransportError::response_too_large(
                "AgentVault HTTP response exceeds configured limit",
            ));
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.to_string(), value.to_owned())))
            .collect::<BTreeMap<_, _>>();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TransportError::new("read AgentVault HTTP response"))?
        {
            if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                return Err(TransportError::response_too_large(
                    "AgentVault HTTP response exceeds configured limit",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(TransportResponse { status, headers, body })
    }
    fn close(&self) -> impl std::future::Future<Output = Result<(), TransportError>> + Send {
        self.closed.store(true, Ordering::Release);
        std::future::ready(Ok(()))
    }
}
#[cfg(test)]
#[path = "../../../tests/transport/http_test.rs"]
mod tests;
