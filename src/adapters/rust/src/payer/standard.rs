use std::collections::BTreeMap;

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response, Url};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next, RequestBuilder};

use crate::payer_core::validate_url_origin;
use crate::{Error, PayerHttpRequest, PayerHttpResponse, PayerMiddleware, PayerRequestContext, Result, VaultTransport};

/// Adds a stable payment operation to one request handled by a globally
/// registered [`ReqwestPayerMiddleware`]. Requests without this extension pass
/// through untouched, so one client can safely serve paid and free resources.
pub trait PayerRequestExt {
    #[must_use]
    fn with_payment(self, context: PayerRequestContext) -> Self;
}

impl PayerRequestExt for RequestBuilder {
    fn with_payment(self, context: PayerRequestContext) -> Self {
        self.with_extension(context)
    }
}

/// Reqwest adapter for [`PayerMiddleware`]. Register it once with
/// `reqwest_middleware::ClientBuilder`; it performs the initial request,
/// handles a supported 402, waits for signing/approval through the configured
/// Vault transport, and safely retries the exact request.
#[derive(Clone)]
pub struct ReqwestPayerMiddleware<T> {
    core: PayerMiddleware<T>,
}

impl<T> ReqwestPayerMiddleware<T> {
    pub(crate) const fn new(core: PayerMiddleware<T>) -> Self {
        Self { core }
    }

    /// Produces a Reqwest client with redirects disabled before this middleware
    /// is registered. Callers may alternatively register `self` on their own
    /// redirect-safe client builder.
    pub fn client(self) -> Result<ClientWithMiddleware>
    where
        T: VaultTransport + 'static,
    {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Error::Transport(format!("build payer HTTP client: {}", error.without_url())))?;
        Ok(ClientBuilder::new(client).with(self).build())
    }
}

#[async_trait]
impl<T> Middleware for ReqwestPayerMiddleware<T>
where
    T: VaultTransport + 'static,
{
    async fn handle(
        &self,
        request: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        let Some(context) = extensions.get::<PayerRequestContext>().cloned() else {
            return next.run(request, extensions).await;
        };
        let original = request.try_clone().ok_or_else(|| {
            middleware_error(Error::UnsafeRetry(
                "paid requests must use a replayable, non-streaming body".to_owned(),
            ))
        })?;
        let first = next.clone().run(request, extensions).await?;
        if first.status().as_u16() != 402 {
            return Ok(first);
        }
        let first = bounded_response(first, self.core.config().max_challenge_bytes).await?;
        let sdk_request = sdk_request(&original)?;
        let authorized = self
            .core
            .authorize(&sdk_request, &first, &context)
            .await
            .map_err(middleware_error)?
            .ok_or_else(|| middleware_error(Error::Decode("expected an HTTP 402 challenge".to_owned())))?;

        let existing = request_headers(original.headers())?;
        let (header_name, header_value) = self
            .core
            .credential_header(&authorized, &sdk_request.origin, &existing)
            .map_err(middleware_error)?;
        let mut paid_template = original;
        let name = reqwest::header::HeaderName::from_bytes(header_name.as_bytes())
            .map_err(|_| middleware_error(Error::UnsafeRetry("credential header name is invalid".to_owned())))?;
        let value = reqwest::header::HeaderValue::from_str(&header_value)
            .map_err(|_| middleware_error(Error::UnsafeRetry("credential header value is invalid".to_owned())))?;
        paid_template.headers_mut().insert(name, value);

        let attempts = usize::from(self.core.config().delivery_retries) + 1;
        for attempt in 0..attempts {
            let paid = paid_template.try_clone().ok_or_else(|| {
                middleware_error(Error::UnsafeRetry("paid request body became non-replayable".to_owned()))
            })?;
            let mut response = next.clone().run(paid, extensions).await?;
            let status = response.status().as_u16();
            let response_view = response_view(&response);
            self.core
                .report(&context, &authorized, &response_view)
                .await
                .map_err(middleware_error)?;
            if status != 503 || attempt + 1 == attempts {
                response.extensions_mut().insert(authorized.clone());
                return Ok(response);
            }
        }
        Err(middleware_error(Error::Transport(
            "payer retry loop ended unexpectedly".to_owned(),
        )))
    }
}

fn middleware_error(error: Error) -> reqwest_middleware::Error {
    reqwest_middleware::Error::Middleware(anyhow::Error::new(error))
}

fn sdk_request(request: &Request) -> reqwest_middleware::Result<PayerHttpRequest> {
    let origin = canonical_origin(request.url()).map_err(middleware_error)?;
    validate_url_origin(request.url().as_str(), &origin).map_err(middleware_error)?;
    let body = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .map_or_else(Vec::new, ToOwned::to_owned);
    Ok(PayerHttpRequest {
        method: request.method().as_str().to_owned(),
        url: request.url().as_str().to_owned(),
        origin,
        headers: request_headers(request.headers())?,
        body,
        follow_redirects: false,
    })
}

fn canonical_origin(url: &Url) -> Result<String> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() {
        return Err(Error::InvalidArgument(
            "paid request URL must be absolute HTTP(S) without userinfo".to_owned(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidArgument("paid request URL must include a canonical host".to_owned()))?;
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    Ok(format!("{}://{authority}", url.scheme()))
}

fn request_headers(headers: &reqwest::header::HeaderMap) -> reqwest_middleware::Result<BTreeMap<String, String>> {
    headers
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
                .map_err(|_| middleware_error(Error::InvalidArgument("request contains a non-text header".to_owned())))
        })
        .collect()
}

fn response_view(response: &Response) -> PayerHttpResponse {
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();
    PayerHttpResponse {
        status: response.status().as_u16(),
        headers,
        body: Vec::new(),
    }
}

async fn bounded_response(mut response: Response, limit: usize) -> reqwest_middleware::Result<PayerHttpResponse> {
    if response
        .content_length()
        .and_then(|value| usize::try_from(value).ok())
        .is_some_and(|value| value > limit)
    {
        return Err(middleware_error(Error::Decode(
            "payment challenge exceeds configured limit".to_owned(),
        )));
    }
    let status = response.status().as_u16();
    let headers = response_view(&response).headers;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(reqwest_middleware::Error::Reqwest)? {
        if body.len().checked_add(chunk.len()).is_none_or(|length| length > limit) {
            return Err(middleware_error(Error::Decode(
                "payment challenge exceeds configured limit".to_owned(),
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(PayerHttpResponse { status, headers, body })
}

#[cfg(test)]
#[path = "../../tests/payer/standard_test.rs"]
mod tests;
