//! Hardened HTTP JSON transport shared by external payment providers.
//!
//! Provider URLs and operation paths are fixed by operator configuration.
//! Payment challenges can never influence the destination, redirects are not
//! followed, and response bodies are streamed through a strict byte limit.

use reqwest::{
    header::{HeaderMap, HeaderValue, ACCEPT},
    redirect::Policy as RedirectPolicy,
    Client, StatusCode, Url,
};
use serde::Serialize;

use crate::{Error, Result};

use super::settlement_handler::SettlementProviderConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderHttpEndpoint {
    Verify,
    Settle,
}

pub(crate) struct ProviderHttpResponse {
    pub status: StatusCode,
    pub body: Vec<u8>,
}

/// Fixed-authority, redirect-free HTTP JSON client.
pub(crate) struct FixedHttpJsonTransport {
    client: Client,
    verify_url: Url,
    settle_url: Url,
    headers: HeaderMap,
    max_response_bytes: usize,
}

impl FixedHttpJsonTransport {
    pub fn new(config: &SettlementProviderConfig, headers: HeaderMap) -> Result<Self> {
        let mut base = Url::parse(&config.base_url)
            .map_err(|error| Error::Config(format!("invalid settlement provider URL: {error}")))?;
        validate_provider_url(&base, config.allow_insecure_loopback)?;
        if base.query().is_some()
            || base.fragment().is_some()
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(Error::Config(
                "settlement provider URL must not contain credentials, query, or fragment".to_owned(),
            ));
        }
        if headers.contains_key("idempotency-key")
            || headers.contains_key("host")
            || headers.contains_key("content-length")
            || headers.contains_key("content-type")
        {
            return Err(Error::Config(
                "settlement provider secret headers contain a transport-owned header".to_owned(),
            ));
        }
        if !base.path().ends_with('/') {
            let normalized = format!("{}/", base.path());
            base.set_path(&normalized);
        }
        let verify_url = join_fixed_path(&base, &config.verify_path)?;
        let settle_url = join_fixed_path(&base, &config.settle_path)?;
        if config.timeout.is_zero() || config.max_response_bytes == 0 {
            return Err(Error::Config("settlement provider limits must be positive".to_owned()));
        }
        let client = Client::builder()
            .redirect(RedirectPolicy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|error| Error::Config(format!("build settlement HTTP client: {error}")))?;
        Ok(Self {
            client,
            verify_url,
            settle_url,
            headers,
            max_response_bytes: config.max_response_bytes,
        })
    }

    pub async fn post_json<T: Serialize + ?Sized>(
        &self,
        endpoint: ProviderHttpEndpoint,
        idempotency_key: &str,
        payload: &T,
    ) -> Result<ProviderHttpResponse> {
        let idempotency_key = HeaderValue::from_str(idempotency_key)
            .map_err(|_| Error::Settlement("provider idempotency key is invalid".to_owned()))?;
        if idempotency_key.is_empty() || idempotency_key.as_bytes().len() > 256 {
            return Err(Error::Settlement("provider idempotency key is invalid".to_owned()));
        }
        let url = match endpoint {
            ProviderHttpEndpoint::Verify => &self.verify_url,
            ProviderHttpEndpoint::Settle => &self.settle_url,
        };
        let mut response = self
            .client
            .post(url.clone())
            .headers(self.headers.clone())
            .header(ACCEPT, "application/json")
            .header("idempotency-key", idempotency_key)
            .json(payload)
            .send()
            .await
            .map_err(|error| Error::Settlement(format!("provider request failed: {error}")))?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(Error::Settlement(
                "provider response exceeds configured limit".to_owned(),
            ));
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(self.max_response_bytes),
        );
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| Error::Settlement(format!("read provider response: {error}")))?
        {
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > self.max_response_bytes)
            {
                return Err(Error::Settlement(
                    "provider response exceeds configured limit".to_owned(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(ProviderHttpResponse { status, body })
    }
}

fn validate_provider_url(url: &Url, allow_insecure_loopback: bool) -> Result<()> {
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_insecure_loopback && url.host_str().is_some_and(is_loopback_host) => Ok(()),
        _ => Err(Error::Config(
            "settlement provider must use HTTPS (HTTP is limited to explicit loopback development)".to_owned(),
        )),
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn join_fixed_path(base: &Url, path: &str) -> Result<Url> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(['?', '#', '\\'])
        || Url::parse(path).is_ok()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || encoded_dot_segment(part))
    {
        return Err(Error::Config("settlement provider operation path is unsafe".to_owned()));
    }
    let joined = base
        .join(path)
        .map_err(|error| Error::Config(format!("invalid settlement provider operation path: {error}")))?;
    let same_authority = joined.scheme() == base.scheme()
        && joined.host_str() == base.host_str()
        && joined.port_or_known_default() == base.port_or_known_default()
        && joined.username() == base.username()
        && joined.password() == base.password();
    if !same_authority || !joined.path().starts_with(base.path()) {
        return Err(Error::Config(
            "settlement provider operation path changes its fixed endpoint".to_owned(),
        ));
    }
    Ok(joined)
}

fn encoded_dot_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    lower == "%2e" || lower == "%2e%2e" || lower == ".%2e" || lower == "%2e."
}

#[cfg(test)]
#[path = "../../tests/handler/http_json_transport_test.rs"]
mod tests;
