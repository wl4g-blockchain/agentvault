//! Fixed-authority Solana RPC client construction.
//!
//! Callers retain ownership of payment semantics. This module only applies the
//! shared endpoint, redirect, deadline, response-size, trace, and secret-header
//! transport policy to the official Solana RPC client.

use std::{
    collections::BTreeMap,
    net::IpAddr,
    str::FromStr as _,
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde_json::Value;
use solana_client::{
    client_error::{reqwest, Result as SolanaResult},
    nonblocking::rpc_client::RpcClient,
    rpc_client::RpcClientConfig,
    rpc_custom_error,
    rpc_request::{RpcError, RpcRequest, RpcResponseErrorData},
    rpc_response::RpcSimulateTransactionResult,
    rpc_sender::{RpcSender, RpcTransportStats},
};
use solana_rpc_client::http_sender::HttpSender;
use tracing::Instrument as _;

use crate::{apm::inject_current_trace_context, Error, Result};

pub(crate) struct SolanaRpcClient;

const RATE_LIMIT_RETRIES: u8 = 5;

struct BoundedHttpSender {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    timeout: Duration,
    max_response_bytes: usize,
    request_id: AtomicU64,
    stats: RwLock<RpcTransportStats>,
}

impl BoundedHttpSender {
    async fn send_inner(
        &self,
        request: RpcRequest,
        params: Value,
        rate_limited_time: &mut Duration,
    ) -> SolanaResult<Value> {
        let request_id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let request_json = request.build_request_json(request_id, params).to_string();
        let mut retries = RATE_LIMIT_RETRIES;
        loop {
            let mut headers = self.headers.clone();
            inject_current_trace_context(&mut headers);
            let response = self
                .client
                .post(self.endpoint.clone())
                .headers(headers)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(request_json.clone())
                .send()
                .await?;
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && retries > 0 {
                let delay = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|seconds| *seconds < 120)
                    .map_or(Duration::from_millis(500), Duration::from_secs);
                retries = retries.saturating_sub(1);
                tokio::time::sleep(delay).await;
                *rate_limited_time = rate_limited_time.saturating_add(delay);
                continue;
            }
            if !response.status().is_success() {
                return Err(response.error_for_status().expect_err("non-success status").into());
            }
            let mut json: Value = serde_json::from_slice(&self.read_bounded(response).await?)?;
            if json["error"].is_object() {
                return Err(Self::rpc_error(&json["error"]).into());
            }
            return Ok(json["result"].take());
        }
    }

    async fn read_bounded(&self, mut response: reqwest::Response) -> SolanaResult<Vec<u8>> {
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(RpcError::RpcRequestError("Solana RPC response exceeds configured limit".to_owned()).into());
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(self.max_response_bytes),
        );
        while let Some(chunk) = response.chunk().await? {
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > self.max_response_bytes)
            {
                return Err(
                    RpcError::RpcRequestError("Solana RPC response exceeds configured limit".to_owned()).into(),
                );
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn rpc_error(error: &Value) -> RpcError {
        let Some(code) = error.get("code").and_then(Value::as_i64) else {
            return RpcError::RpcRequestError("Solana RPC returned a malformed error response".to_owned());
        };
        let Some(message) = error.get("message").and_then(Value::as_str) else {
            return RpcError::RpcRequestError("Solana RPC returned a malformed error response".to_owned());
        };
        let data = match code {
            rpc_custom_error::JSON_RPC_SERVER_ERROR_SEND_TRANSACTION_PREFLIGHT_FAILURE => error
                .get("data")
                .cloned()
                .and_then(|data| serde_json::from_value::<RpcSimulateTransactionResult>(data).ok())
                .map_or(
                    RpcResponseErrorData::Empty,
                    RpcResponseErrorData::SendTransactionPreflightFailure,
                ),
            rpc_custom_error::JSON_RPC_SERVER_ERROR_NODE_UNHEALTHY => error
                .get("data")
                .cloned()
                .and_then(|data| serde_json::from_value::<rpc_custom_error::NodeUnhealthyErrorData>(data).ok())
                .map_or(RpcResponseErrorData::Empty, |data| {
                    RpcResponseErrorData::NodeUnhealthy {
                        num_slots_behind: data.num_slots_behind,
                    }
                }),
            _ => RpcResponseErrorData::Empty,
        };
        RpcError::RpcResponseError {
            code,
            message: message.to_owned(),
            data,
        }
    }

    fn record_stats(&self, started: Instant, rate_limited_time: Duration) {
        let mut stats = self.stats.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        stats.request_count = stats.request_count.saturating_add(1);
        stats.elapsed_time = stats.elapsed_time.saturating_add(started.elapsed());
        stats.rate_limited_time = stats.rate_limited_time.saturating_add(rate_limited_time);
    }
}

#[async_trait]
impl RpcSender for BoundedHttpSender {
    async fn send(&self, request: RpcRequest, params: Value) -> SolanaResult<Value> {
        let started = Instant::now();
        let mut rate_limited_time = Duration::ZERO;
        let span = tracing::info_span!(
            "solana.rpc.request",
            otel.kind = "client",
            rpc.system = "jsonrpc",
            rpc.method = %request,
            server.address = self.endpoint.host_str().unwrap_or(""),
        );
        let result = tokio::time::timeout(
            self.timeout,
            self.send_inner(request, params, &mut rate_limited_time)
                .instrument(span),
        )
        .await
        .map_err(|_| RpcError::RpcRequestError("Solana RPC deadline exceeded".to_owned()).into())
        .and_then(std::convert::identity);
        self.record_stats(started, rate_limited_time);
        result
    }

    fn get_transport_stats(&self) -> RpcTransportStats {
        self.stats
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn url(&self) -> String {
        self.endpoint.to_string()
    }
}

impl SolanaRpcClient {
    pub(crate) fn build(
        endpoint: &str,
        timeout: Duration,
        max_response_bytes: usize,
        configured_headers: &BTreeMap<String, String>,
        allow_insecure_loopback: bool,
    ) -> Result<RpcClient> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| Error::Config("Solana RPC URL is invalid".to_owned()))?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "http" | "https")
            || (url.scheme() == "http"
                && (!allow_insecure_loopback || !url.host_str().is_some_and(SolanaRpcClient::is_loopback_host)))
            || timeout.is_zero()
            || max_response_bytes == 0
        {
            return Err(Error::Config("Solana RPC endpoint or timeout is unsafe".to_owned()));
        }

        let mut headers = HttpSender::default_headers();
        for (name, value) in configured_headers {
            let normalized = name.to_ascii_lowercase();
            if matches!(
                normalized.as_str(),
                "connection"
                    | "content-length"
                    | "content-type"
                    | "host"
                    | "traceparent"
                    | "tracestate"
                    | "transfer-encoding"
            ) {
                return Err(Error::Config("Solana RPC secret header name is reserved".to_owned()));
            }
            let name = solana_client::client_error::reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::Config("Solana RPC header name is invalid".to_owned()))?;
            let mut value = solana_client::client_error::reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| Error::Config("Solana RPC header value is invalid".to_owned()))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .pool_idle_timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Error::Config(format!("build Solana RPC client: {error}")))?;
        Ok(RpcClient::new_sender(
            BoundedHttpSender {
                client: http,
                endpoint: url,
                headers,
                timeout,
                max_response_bytes,
                request_id: AtomicU64::new(0),
                stats: RwLock::new(RpcTransportStats::default()),
            },
            RpcClientConfig::default(),
        ))
    }

    fn is_loopback_host(host: &str) -> bool {
        host.eq_ignore_ascii_case("localhost") || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
    }
}

#[cfg(test)]
#[path = "../../tests/utils/solana_rpc_test.rs"]
mod tests;
