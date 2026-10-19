use std::{collections::BTreeMap, fmt, net::IpAddr, str::FromStr as _, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use tonic::{
    metadata::MetadataValue,
    transport::{Channel, ClientTlsConfig, Endpoint},
    Request,
};
use zeroize::Zeroizing;

use crate::{GrpcConfig, HttpMethod, TransportError, TransportRequest, TransportResponse, VaultTransport};

use crate::transport_mqtt::producer_trace_context;

#[allow(clippy::default_trait_access, clippy::doc_markdown)]
mod wire {
    tonic::include_proto!("agentvault.transport.v1");
}

#[derive(Clone)]
pub struct GrpcVaultTransport {
    client: wire::payment_transport_client::PaymentTransportClient<Channel>,
    authguard_bearer: Option<Zeroizing<String>>,
    request_timeout: Duration,
    max_message_bytes: usize,
}

impl fmt::Debug for GrpcVaultTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrpcVaultTransport")
            .field(
                "authguard_bearer",
                &self.authguard_bearer.as_ref().map(|_| "<redacted>"),
            )
            .field("request_timeout", &self.request_timeout)
            .field("max_message_bytes", &self.max_message_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    method: &'static str,
    path: &'a str,
    headers: &'a BTreeMap<String, String>,
    body: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

impl GrpcVaultTransport {
    /// Creates a lazy gRPC channel. Remote endpoints must use HTTPS; plaintext
    /// HTTP/2 is accepted only for loopback development endpoints.
    pub fn new(config: GrpcConfig) -> Result<Self, TransportError> {
        validate_config(&config)?;
        let scheme = if config.tls { "https" } else { "http" };
        let mut endpoint = Endpoint::from_shared(format!("{scheme}://{}", config.endpoint))
            .map_err(|_| TransportError::new("invalid AgentVault gRPC endpoint"))?
            .timeout(config.request_timeout);
        if config.tls {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_webpki_roots())
                .map_err(|_| TransportError::new("invalid AgentVault gRPC TLS configuration"))?;
        }
        let client = wire::payment_transport_client::PaymentTransportClient::new(endpoint.connect_lazy())
            .max_decoding_message_size(config.max_message_bytes)
            .max_encoding_message_size(config.max_message_bytes);
        Ok(Self {
            client,
            authguard_bearer: config.authguard_bearer.map(Zeroizing::new),
            request_timeout: config.request_timeout,
            max_message_bytes: config.max_message_bytes,
        })
    }
}

impl VaultTransport for GrpcVaultTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        let trace = producer_trace_context(&request.headers);
        let wire_headers = request
            .headers
            .iter()
            .filter(|(name, _)| !matches!(name.to_ascii_lowercase().as_str(), "traceparent" | "tracestate"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let encoded = serde_json::to_vec(&WireRequest {
            method: match request.method {
                HttpMethod::Get => "GET",
                HttpMethod::Post => "POST",
            },
            path: &request.path,
            headers: &wire_headers,
            body: URL_SAFE_NO_PAD.encode(&request.body),
        })
        .map_err(|_| TransportError::new("encode AgentVault gRPC request"))?;
        if encoded.len() > self.max_message_bytes {
            return Err(TransportError::response_too_large(
                "AgentVault gRPC request exceeds configured limit",
            ));
        }
        let mut request = Request::new(wire::Envelope { value: encoded });
        request.set_timeout(self.request_timeout);
        let traceparent = trace
            .headers
            .get("traceparent")
            .ok_or_else(|| TransportError::new("create AgentVault gRPC trace context"))?
            .parse()
            .map_err(|_| TransportError::new("create AgentVault gRPC trace metadata"))?;
        request.metadata_mut().insert("traceparent", traceparent);
        if let Some(tracestate) = trace
            .headers
            .iter()
            .find_map(|(name, value)| name.eq_ignore_ascii_case("tracestate").then_some(value))
        {
            request.metadata_mut().insert(
                "tracestate",
                tracestate
                    .parse()
                    .map_err(|_| TransportError::new("invalid AgentVault gRPC tracestate"))?,
            );
        }
        tracing::info!(
            event.name = "agentvault.sdk.transport.grpc.produced",
            trace.id = %trace.trace_id,
            span.id = %trace.span_id,
            parent.id = %trace.parent_span_id.as_deref().unwrap_or(""),
            "calling AgentVault gRPC with W3C Trace Context"
        );
        if let Some(token) = &self.authguard_bearer {
            let value = MetadataValue::try_from(format!("Bearer {}", token.as_str()))
                .map_err(|_| TransportError::new("invalid AuthGuard bearer credential"))?;
            request.metadata_mut().insert("authorization", value);
        }
        let response = self
            .client
            .clone()
            .execute(request)
            .await
            .map_err(|status| {
                if status.code() == tonic::Code::DeadlineExceeded {
                    TransportError::timeout("AgentVault gRPC request timed out")
                } else {
                    TransportError::new(format!("AgentVault gRPC request failed: {}", status.code()))
                }
            })?
            .into_inner();
        if response.value.len() > self.max_message_bytes {
            return Err(TransportError::response_too_large(
                "AgentVault gRPC response exceeds configured limit",
            ));
        }
        let wire: WireResponse = serde_json::from_slice(&response.value)
            .map_err(|_| TransportError::new("invalid AgentVault gRPC response"))?;
        let body = URL_SAFE_NO_PAD
            .decode(&wire.body)
            .map_err(|_| TransportError::new("invalid AgentVault gRPC response body"))?;
        if body.len() > self.max_message_bytes || !(100..=599).contains(&wire.status) {
            return Err(TransportError::new("invalid or oversized AgentVault gRPC response"));
        }
        Ok(TransportResponse {
            status: wire.status,
            headers: wire.headers,
            body,
        })
    }
}

fn validate_config(config: &GrpcConfig) -> Result<(), TransportError> {
    if config.request_timeout.is_zero() || config.max_message_bytes == 0 {
        return Err(TransportError::new("invalid AgentVault gRPC transport limits"));
    }
    let uri = http::Uri::from_str(&format!("http://{}", config.endpoint))
        .map_err(|_| TransportError::new("invalid AgentVault gRPC endpoint"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| TransportError::new("AgentVault gRPC endpoint requires an authority"))?;
    if uri.path_and_query().is_some_and(|value| value.as_str() != "/")
        || (!config.tls && !is_loopback(authority.host()))
    {
        return Err(TransportError::new("unsafe AgentVault gRPC endpoint"));
    }
    if let Some(token) = &config.authguard_bearer {
        if token.trim().is_empty() || token.chars().any(char::is_control) {
            return Err(TransportError::new("invalid AuthGuard bearer credential"));
        }
    }
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
#[path = "../../tests/transport/grpc_test.rs"]
mod tests;
