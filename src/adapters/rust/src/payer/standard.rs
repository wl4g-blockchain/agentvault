use std::collections::BTreeMap;

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response, Url};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next, RequestBuilder};
use serde_json::Value;

use crate::payer_core::{PayerDelivery, PayerValidation};
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

    fn prepare_a2a_request(
        &self,
        request: &mut Request,
        context: &PayerRequestContext,
    ) -> reqwest_middleware::Result<Option<crate::A2aCall>> {
        let a2a = if request.method() == reqwest::Method::POST
            && request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/json"))
        {
            crate::A2aCall::parse(
                request.body().and_then(reqwest::Body::as_bytes).unwrap_or_default(),
                self.core.config().max_challenge_bytes,
            )
            .map_err(ReqwestWire::middleware_error)?
        } else {
            None
        };
        if a2a.is_some() {
            if request
                .headers()
                .get("x-run-id")
                .is_some_and(|v| v.as_bytes() != context.run_id.as_bytes())
            {
                return Err(ReqwestWire::middleware_error(crate::A2aCall::invalid(
                    "A2A run_id conflict",
                )));
            }
            request.headers_mut().insert(
                "x-run-id",
                context
                    .run_id
                    .parse()
                    .map_err(|_| ReqwestWire::middleware_error(crate::A2aCall::invalid("invalid run_id header")))?,
            );
            let extensions = self.core.a2a_extensions(
                request
                    .headers()
                    .get("x-a2a-extensions")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default(),
            );
            request.headers_mut().insert(
                "x-a2a-extensions",
                extensions.parse().map_err(|_| {
                    ReqwestWire::middleware_error(crate::A2aCall::invalid("invalid A2A extension header"))
                })?,
            );
        }
        Ok(a2a)
    }

    fn attach_payment(
        &self,
        paid_template: &mut Request,
        authorized: &crate::AuthorizedPayment,
        sdk_request: &PayerHttpRequest,
        first: &PayerHttpResponse,
        a2a: &mut Option<crate::A2aCall>,
        http_challenge: bool,
    ) -> reqwest_middleware::Result<()> {
        if http_challenge {
            let existing = ReqwestWire::request_headers(paid_template.headers())?;
            let (header_name, header_value) = self
                .core
                .credential_header(authorized, &sdk_request.origin, &existing)
                .map_err(ReqwestWire::middleware_error)?;
            let name = reqwest::header::HeaderName::from_bytes(header_name.as_bytes()).map_err(|_| {
                ReqwestWire::middleware_error(Error::UnsafeRetry("credential header name is invalid".to_owned()))
            })?;
            let value = reqwest::header::HeaderValue::from_str(&header_value).map_err(|_| {
                ReqwestWire::middleware_error(Error::UnsafeRetry("credential header value is invalid".to_owned()))
            })?;
            paid_template.headers_mut().insert(name, value);
        } else if let Some(call) = a2a.as_mut() {
            let challenge = serde_json::from_slice(&first.body)
                .map_err(|_| ReqwestWire::middleware_error(crate::A2aCall::invalid("invalid A2A challenge JSON")))?;
            let body = self
                .core
                .a2a_credential(authorized, call, &challenge, &sdk_request.origin)
                .map_err(ReqwestWire::middleware_error)?;
            *paid_template.body_mut() = Some(reqwest::Body::from(body));
            paid_template.headers_mut().remove(reqwest::header::CONTENT_LENGTH);
        } else {
            let body = self
                .core
                .mcp_credential(authorized, &sdk_request.body)
                .map_err(ReqwestWire::middleware_error)?;
            *paid_template.body_mut() = Some(reqwest::Body::from(body));
            paid_template.headers_mut().remove(reqwest::header::CONTENT_LENGTH);
        }
        Ok(())
    }

    fn replayable_mcp_tool_call_id(request: &Request) -> Option<Value> {
        if request.method() != reqwest::Method::POST
            || !request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
        {
            return None;
        }
        request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .and_then(|body| serde_json::from_slice::<Value>(body).ok())
            .filter(|message| {
                message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                    && message.get("method").and_then(Value::as_str) == Some("tools/call")
                    && message.get("params").is_some_and(Value::is_object)
            })
            .and_then(|message| message.get("id").cloned())
            .filter(|id| id.is_string() || id.is_number())
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
        mut request: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        let Some(context) = extensions.get::<PayerRequestContext>().cloned() else {
            return next.run(request, extensions).await;
        };
        let a2a = self.prepare_a2a_request(&mut request, &context)?;
        let original = request.try_clone().ok_or_else(|| {
            ReqwestWire::middleware_error(Error::UnsafeRetry(
                "paid requests must use a replayable, non-streaming body".to_owned(),
            ))
        })?;
        let mcp_call_id = a2a
            .as_ref()
            .map(|call| call.wire["id"].clone())
            .or_else(|| Self::replayable_mcp_tool_call_id(&original));
        let first = next.clone().run(request, extensions).await?;
        let http_challenge = first.status().as_u16() == 402;
        let message_challenge = mcp_call_id.is_some()
            && !http_challenge
            && first
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"));
        if !http_challenge && !message_challenge {
            return Ok(first);
        }
        let first = ReqwestDelivery::buffer(first, self.core.config().max_challenge_bytes).await?;
        if message_challenge
            && !ReqwestDelivery::bound_mcp_response(
                &first.view.body,
                mcp_call_id.as_ref().expect("message challenge has an MCP call id"),
                false,
            )
        {
            return Ok(first.response);
        }
        let sdk_request = ReqwestWire::sdk_request(&original)?;
        let authorized = if http_challenge {
            self.core.authorize(&sdk_request, &first.view, &context).await
        } else if a2a.is_some() {
            self.core.authorize_a2a(&sdk_request, &first.view, &context).await
        } else {
            self.core.authorize_mcp(&sdk_request, &first.view, &context).await
        }
        .map_err(ReqwestWire::middleware_error)?;
        let Some(authorized) = authorized else {
            return Ok(first.response);
        };
        tracing::info!(event.name = "agentvault.sdk.payer.payment_required", run.id = %context.run_id);
        tracing::info!(event.name = "agentvault.sdk.payer.signing_requested", run.id = %context.run_id);
        tracing::info!(
            event.name = "agentvault.sdk.payer.signed",
            run.id = %context.run_id,
            payment.intent_id = %authorized.intent.id
        );

        let mut paid_template = original;
        let mut a2a = a2a;
        self.attach_payment(
            &mut paid_template,
            &authorized,
            &sdk_request,
            &first.view,
            &mut a2a,
            http_challenge,
        )?;
        if http_challenge {
            // HTTP 402 selects header credentials and receipts even when the
            // protected application body happens to be an A2A message.
            a2a = None;
        }

        let mut delivery = ReqwestDelivery {
            template: paid_template,
            next,
            extensions,
            message_level: message_challenge,
            a2a,
            mcp_call_id,
            max_message_bytes: self.core.config().max_challenge_bytes,
        };
        let mut response = self
            .core
            .deliver(&context, &authorized, &mut delivery)
            .await
            .map_err(ReqwestWire::middleware_error)?
            .response;
        response.extensions_mut().insert(authorized);
        Ok(response)
    }
}

struct BufferedResponse {
    response: Response,
    view: PayerHttpResponse,
}

struct ReqwestDelivery<'next, 'extensions> {
    template: Request,
    next: Next<'next>,
    extensions: &'extensions mut Extensions,
    message_level: bool,
    a2a: Option<crate::A2aCall>,
    mcp_call_id: Option<Value>,
    max_message_bytes: usize,
}

impl ReqwestDelivery<'_, '_> {
    fn bound_mcp_response(body: &[u8], expected_id: &Value, require_result: bool) -> bool {
        serde_json::from_slice::<Value>(body).is_ok_and(|message| {
            message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && message.get("id") == Some(expected_id)
                && (!require_result || message.get("result").is_some_and(Value::is_object))
        })
    }

    fn response_view(response: &Response) -> PayerHttpResponse {
        let headers = ReqwestWire::response_headers(response.headers());
        PayerHttpResponse {
            status: response.status().as_u16(),
            headers,
            body: Vec::new(),
        }
    }

    async fn buffer(mut response: Response, limit: usize) -> reqwest_middleware::Result<BufferedResponse> {
        if response
            .content_length()
            .and_then(|value| usize::try_from(value).ok())
            .is_some_and(|value| value > limit)
        {
            return Err(ReqwestWire::middleware_error(Error::Decode(
                "MCP payment message exceeds configured limit".to_owned(),
            )));
        }
        let status = response.status();
        let version = response.version();
        let headers = response.headers().clone();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(reqwest_middleware::Error::Reqwest)? {
            if body.len().checked_add(chunk.len()).is_none_or(|length| length > limit) {
                return Err(ReqwestWire::middleware_error(Error::Decode(
                    "MCP payment message exceeds configured limit".to_owned(),
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let view = PayerHttpResponse {
            status: status.as_u16(),
            headers: ReqwestWire::response_headers(&headers),
            body: body.clone(),
        };
        let mut rebuilt = http::Response::builder()
            .status(status)
            .version(version)
            .body(reqwest::Body::from(body))
            .map_err(|error| {
                ReqwestWire::middleware_error(Error::Transport(format!("rebuild MCP response: {error}")))
            })?;
        *rebuilt.headers_mut() = headers;
        Ok(BufferedResponse {
            response: Response::from(rebuilt),
            view,
        })
    }
}

impl PayerDelivery for ReqwestDelivery<'_, '_> {
    type Response = BufferedResponse;
    fn a2a_call(&self) -> Option<&crate::A2aCall> {
        self.a2a.as_ref()
    }

    async fn send(&mut self, _attempt_no: u8) -> Result<Self::Response> {
        let paid = self
            .template
            .try_clone()
            .ok_or_else(|| Error::UnsafeRetry("paid request body became non-replayable".to_owned()))?;
        let response = self
            .next
            .clone()
            .run(paid, self.extensions)
            .await
            .map_err(|error| Error::Transport(format!("paid HTTP delivery failed: {error}")))?;
        if self.message_level {
            let buffered = Self::buffer(response, self.max_message_bytes)
                .await
                .map_err(|error| Error::Transport(format!("read paid MCP response: {error}")))?;
            if (200..300).contains(&buffered.view.status)
                && !Self::bound_mcp_response(
                    &buffered.view.body,
                    self.mcp_call_id
                        .as_ref()
                        .ok_or_else(|| Error::UnsafeRetry("MCP call identity is missing".to_owned()))?,
                    true,
                )
            {
                return Err(Error::UnsafeRetry(
                    "paid MCP response has no matching JSON-RPC result".to_owned(),
                ));
            }
            Ok(buffered)
        } else {
            Ok(BufferedResponse {
                view: Self::response_view(&response),
                response,
            })
        }
    }

    fn view(&self, response: &Self::Response) -> PayerHttpResponse {
        response.view.clone()
    }
}

#[cfg(test)]
#[path = "../../tests/payer/standard_test.rs"]
mod tests;

struct ReqwestWire;

impl ReqwestWire {
    fn middleware_error(error: Error) -> reqwest_middleware::Error {
        reqwest_middleware::Error::Middleware(anyhow::Error::new(error))
    }

    fn sdk_request(request: &Request) -> reqwest_middleware::Result<PayerHttpRequest> {
        let origin = ReqwestWire::canonical_origin(request.url()).map_err(ReqwestWire::middleware_error)?;
        PayerValidation::validate_url_origin(request.url().as_str(), &origin).map_err(ReqwestWire::middleware_error)?;
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .map_or_else(Vec::new, ToOwned::to_owned);
        Ok(PayerHttpRequest {
            method: request.method().as_str().to_owned(),
            url: request.url().as_str().to_owned(),
            origin,
            headers: ReqwestWire::request_headers(request.headers())?,
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
        ReqwestWire::text_headers(headers).map_err(|()| {
            ReqwestWire::middleware_error(Error::InvalidArgument("request contains a non-text header".to_owned()))
        })
    }

    fn text_headers(headers: &reqwest::header::HeaderMap) -> std::result::Result<BTreeMap<String, String>, ()> {
        let mut flattened = BTreeMap::new();
        for name in headers.keys() {
            let values = headers
                .get_all(name)
                .iter()
                .map(|value| value.to_str().map(str::to_owned).map_err(|_| ()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            flattened.insert(name.as_str().to_owned(), values.join(", "));
        }
        Ok(flattened)
    }

    fn response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
        let mut flattened = BTreeMap::new();
        for name in headers.keys() {
            let Ok(values) = headers
                .get_all(name)
                .iter()
                .map(|value| value.to_str().map(str::to_owned))
                .collect::<std::result::Result<Vec<_>, _>>()
            else {
                // One malformed value invalidates this complete logical header.
                // Other response headers, including settlement evidence, remain
                // available to the protocol adapter.
                continue;
            };
            flattened.insert(name.as_str().to_owned(), values.join(", "));
        }
        flattened
    }
}
