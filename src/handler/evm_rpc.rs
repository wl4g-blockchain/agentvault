//! Fixed-authority, read-only EVM JSON-RPC transport.
//!
//! Both balance checks and receipt evidence use this boundary so endpoint
//! validation, redirect refusal, secret-header handling, timeouts, and
//! streaming response limits cannot drift apart.

use std::{collections::BTreeMap, net::IpAddr, str::FromStr as _, time::Duration};

use reqwest::{header::HeaderMap, redirect::Policy as RedirectPolicy, Client, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::Instrument as _;

use crate::{apm::inject_current_trace_context, Error, Result};

pub(super) struct FixedEvmRpcClient {
    client: Client,
    endpoint: Url,
    headers: HeaderMap,
    max_response_bytes: usize,
}

impl FixedEvmRpcClient {
    pub(super) fn new(
        endpoint: &str,
        secret_headers: BTreeMap<String, String>,
        timeout: Duration,
        max_response_bytes: usize,
        allow_insecure_loopback: bool,
    ) -> Result<Self> {
        if timeout.is_zero() || max_response_bytes == 0 {
            return Err(Error::Config("EVM RPC limits must be positive".to_owned()));
        }
        let endpoint = Url::parse(endpoint).map_err(|_| Error::Config("EVM RPC URL is invalid".to_owned()))?;
        if endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.scheme(), "https" | "http")
            || (endpoint.scheme() == "http" && (!allow_insecure_loopback || !url_is_loopback(&endpoint)))
        {
            return Err(Error::Config("EVM RPC URL is unsafe".to_owned()));
        }
        let mut headers = HeaderMap::new();
        for (name, value) in secret_headers {
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
                return Err(Error::Config("EVM RPC secret header name is reserved".to_owned()));
            }
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::Config("EVM RPC secret header name is invalid".to_owned()))?;
            let mut value = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| Error::Config("EVM RPC secret header value is invalid".to_owned()))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let client = Client::builder()
            .redirect(RedirectPolicy::none())
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .build()
            .map_err(|error| Error::Config(format!("build EVM RPC client: {}", error.without_url())))?;
        Ok(Self {
            client,
            endpoint,
            headers,
            max_response_bytes,
        })
    }

    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value> {
        if method.is_empty()
            || method.len() > 128
            || !method.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(Error::Settlement("EVM RPC method is invalid".to_owned()));
        }
        let span = tracing::info_span!(
            "evm.rpc.request",
            otel.kind = "client",
            rpc.system = "jsonrpc",
            rpc.method = method,
            server.address = self.endpoint.host_str().unwrap_or(""),
            http.response.status_code = tracing::field::Empty,
        );
        let response = async {
            let mut headers = self.headers.clone();
            inject_current_trace_context(&mut headers);
            self.client
                .post(self.endpoint.clone())
                .headers(headers)
                .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
                .send()
                .await
                .map_err(|error| Error::Settlement(format!("EVM RPC request failed: {}", error.without_url())))
        }
        .instrument(span.clone())
        .await?;
        span.record("http.response.status_code", response.status().as_u16());
        if !response.status().is_success() {
            return Err(Error::Settlement(format!(
                "EVM RPC returned HTTP {}",
                response.status().as_u16()
            )));
        }
        let body = bounded_body(response, self.max_response_bytes).await?;
        let envelope: RpcResponse =
            serde_json::from_slice(&body).map_err(|_| Error::Settlement("EVM RPC returned invalid JSON".to_owned()))?;
        if envelope.jsonrpc != "2.0" || envelope.id != json!(1) {
            return Err(Error::Settlement(
                "EVM RPC response does not match the request".to_owned(),
            ));
        }
        if envelope.error.is_some() {
            return Err(Error::Settlement("EVM RPC returned an error".to_owned()));
        }
        Ok(envelope.result.unwrap_or(Value::Null))
    }
}

#[derive(Deserialize)]
struct RpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

async fn bounded_body(mut response: reqwest::Response, maximum: usize) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|length| length > maximum as u64) {
        return Err(Error::Settlement("EVM RPC response is too large".to_owned()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| Error::Settlement(format!("read EVM RPC response: {}", error.without_url())))?
    {
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > maximum)
        {
            return Err(Error::Settlement("EVM RPC response is too large".to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn url_is_loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
    })
}

#[cfg(test)]
#[path = "../../tests/handler/evm_rpc_test.rs"]
mod tests;
