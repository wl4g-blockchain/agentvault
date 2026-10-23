use serde_json::Value;
use std::collections::BTreeMap;

use crate::{
    percent_encode, ApiClient, AuthorizePaymentRequest, AuthorizePaymentResponse, AuthorizedPayment, BalanceQuery,
    Error, OperationContext, PayerConfig, PayerHttpRequest, PayerHttpResponse, PayerRequestContext,
    PaymentAuthorizationResult, PaymentBalance, PaymentCredential, PaymentIntent, PaymentProtocolFactory,
    PaymentReceipt, PaymentRequired, ProtocolRegistry, ReportOutboundResponseRequest, ReportOutboundResponseResponse,
    Result, VaultTransport,
};

/// Transport-neutral payer workflow. Ecosystem-specific adapters compose this
/// type instead of reimplementing challenge parsing, approval waiting, and
/// settlement evidence reporting.
pub struct PayerMiddleware<T> {
    api: ApiClient<T>,
    config: PayerConfig,
    protocols: ProtocolRegistry,
}

impl<T> Clone for PayerMiddleware<T> {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            config: self.config.clone(),
            protocols: self.protocols.clone(),
        }
    }
}

impl<T> PayerMiddleware<T> {
    pub fn new(transport: T, config: PayerConfig) -> Result<Self> {
        config.validate()?;
        let protocols = PaymentProtocolFactory::new().create(&config.protocols)?;
        Ok(Self {
            api: ApiClient::new(transport),
            config,
            protocols,
        })
    }

    pub(crate) fn from_api(api: ApiClient<T>, config: PayerConfig) -> Result<Self> {
        config.validate()?;
        let protocols = PaymentProtocolFactory::new().create(&config.protocols)?;
        Ok(Self { api, config, protocols })
    }

    #[must_use]
    pub const fn config(&self) -> &PayerConfig {
        &self.config
    }

    #[must_use]
    pub fn standard(self) -> crate::ReqwestPayerMiddleware<T> {
        crate::ReqwestPayerMiddleware::new(self)
    }

    pub fn credential_header(
        &self,
        payment: &AuthorizedPayment,
        origin: &str,
        existing: &BTreeMap<String, String>,
    ) -> Result<(String, String)> {
        if payment.intent.protocol != payment.credential.protocol {
            return Err(Error::UnsafeRetry(
                "outbound payment protocol binding is invalid".to_owned(),
            ));
        }
        let delivery = self
            .protocols
            .get(&payment.credential.protocol)?
            .credential(&payment.credential)?;
        validate_header_name(&delivery.header_name)?;
        if !delivery.bound_origin.eq_ignore_ascii_case(origin)
            || delivery.header_value.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
            || existing
                .keys()
                .any(|name| name.eq_ignore_ascii_case(&delivery.header_name))
        {
            return Err(Error::UnsafeRetry(
                "credential HTTP binding is unsafe or does not match the request".to_owned(),
            ));
        }
        Ok((delivery.header_name, delivery.header_value))
    }

    /// Inserts the authorized protocol credential into a replayable MCP
    /// `tools/call` request without changing unrelated application metadata.
    pub fn mcp_credential(&self, payment: &AuthorizedPayment, request: &[u8]) -> Result<Vec<u8>> {
        if payment.intent.protocol != payment.credential.protocol {
            return Err(Error::UnsafeRetry(
                "outbound payment protocol binding is invalid".to_owned(),
            ));
        }
        let adapter = self.protocols.get(&payment.credential.protocol)?;
        let metadata = adapter.mcp_credential(&payment.credential)?;
        let mut message = serde_json::from_slice::<Value>(request)
            .map_err(|_| Error::UnsafeRetry("paid MCP request is not valid JSON".to_owned()))?;
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || message.get("method").and_then(Value::as_str) != Some("tools/call")
            || !message
                .get("id")
                .is_some_and(|value| value.is_string() || value.is_number())
        {
            return Err(Error::UnsafeRetry(
                "message-level payment is limited to MCP tools/call".to_owned(),
            ));
        }
        if let Some(root_meta) = message.get("_meta") {
            let root_meta = root_meta
                .as_object()
                .ok_or_else(|| Error::UnsafeRetry("JSON-RPC root _meta must be an object".to_owned()))?;
            if self
                .protocols
                .mcp_root_request_metadata_keys()
                .iter()
                .any(|key| root_meta.contains_key(*key))
            {
                return Err(Error::UnsafeRetry(
                    "MCP request already contains root payment credential metadata".to_owned(),
                ));
            }
        }
        let params = message
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| Error::UnsafeRetry("MCP tools/call params must be an object".to_owned()))?;
        let meta = params
            .entry("_meta")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or_else(|| Error::UnsafeRetry("MCP params._meta must be an object".to_owned()))?;
        if self
            .protocols
            .mcp_request_metadata_keys()
            .iter()
            .any(|key| meta.contains_key(*key))
            || meta.contains_key(metadata.key)
        {
            return Err(Error::UnsafeRetry(
                "MCP request already contains payment credential metadata".to_owned(),
            ));
        }
        meta.insert(metadata.key.to_owned(), metadata.value);
        serde_json::to_vec(&message).map_err(|error| Error::Encode(error.to_string()))
    }
}

pub(crate) trait PayerDelivery {
    type Response;

    async fn send(&mut self, attempt_no: u8) -> Result<Self::Response>;
    fn view(&self, response: &Self::Response) -> PayerHttpResponse;
}

impl<T: VaultTransport> PayerMiddleware<T> {
    pub async fn authorize(
        &self,
        original: &PayerHttpRequest,
        first_response: &PayerHttpResponse,
        context: &PayerRequestContext,
    ) -> Result<Option<AuthorizedPayment>> {
        let Some(mut challenge) = payment_required(original, first_response, self.config.max_challenge_bytes)? else {
            return Ok(None);
        };
        let protocol_hint = challenge.protocol_hint.clone();
        if let Some(protocol) = protocol_hint.as_deref() {
            let adapter = self.protocols.get(protocol)?;
            if challenge.expires_at.is_none() {
                challenge.expires_at = adapter.challenge_expires_at(&challenge.headers, &challenge.body)?;
            }
        }
        let operation = OperationContext::new(self.config.scenario_id.clone(), context.run_id.clone())?;
        let response = self
            .authorize_until_terminal(&AuthorizePaymentRequest {
                context: operation,
                challenge,
            })
            .await?;
        match response.authorization {
            PaymentAuthorizationResult::Authorized { .. } => {
                let credential = response.credential.ok_or_else(|| {
                    Error::Decode("authorized AgentVault response contained no credential".to_owned())
                })?;
                self.protocols.get(&response.intent.protocol)?;
                validate_payment_binding(
                    original,
                    context,
                    &self.config,
                    protocol_hint.as_deref(),
                    &response.intent,
                    &credential,
                )?;
                Ok(Some(AuthorizedPayment {
                    intent: response.intent,
                    credential,
                }))
            }
            PaymentAuthorizationResult::Denied { reason, .. } => Err(Error::Api {
                status: 403,
                code: "PAYMENT_DENIED".to_owned(),
                message: reason,
            }),
            PaymentAuthorizationResult::RequiresApproval { .. } => {
                Err(Error::Decode("approval wait returned a non-terminal result".to_owned()))
            }
        }
    }

    /// Recognizes an official message-level x402/MPP challenge and reuses the
    /// same Vault authorization state machine as an HTTP 402 response.
    pub async fn authorize_mcp(
        &self,
        original: &PayerHttpRequest,
        first_response: &PayerHttpResponse,
        context: &PayerRequestContext,
    ) -> Result<Option<AuthorizedPayment>> {
        if first_response.body.len() > self.config.max_challenge_bytes {
            return Err(Error::Decode(
                "MCP payment challenge exceeds configured limit".to_owned(),
            ));
        }
        let Ok(message) = serde_json::from_slice::<Value>(&first_response.body) else {
            return Ok(None);
        };
        let Ok(request_message) = serde_json::from_slice::<Value>(&original.body) else {
            return Ok(None);
        };
        let request_id = request_message
            .get("id")
            .filter(|value| value.is_string() || value.is_number());
        if request_message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || request_message.get("method").and_then(Value::as_str) != Some("tools/call")
            || !request_message.get("params").is_some_and(Value::is_object)
            || request_id.is_none()
            || message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || message.get("id") != request_id
        {
            return Ok(None);
        }
        let Some((adapter, challenge)) = self
            .protocols
            .mcp_challenge(&message, self.config.max_challenge_bytes)?
        else {
            return Ok(None);
        };
        if header(&first_response.headers, "payment-required").is_some() {
            return Err(Error::Decode(
                "MCP payment challenge must not also declare PAYMENT-REQUIRED".to_owned(),
            ));
        }
        if header(&first_response.headers, "payment-protocol")
            .is_some_and(|protocol| !protocol.eq_ignore_ascii_case(adapter.name()))
        {
            return Err(Error::Decode(
                "MCP payment challenge protocol metadata conflicts with its JSON-RPC payload".to_owned(),
            ));
        }
        let mut headers = challenge.headers;
        headers.insert("payment-protocol".to_owned(), adapter.name().to_owned());
        if let Some(expires_at) = header(&first_response.headers, "payment-expires-at") {
            headers.insert("payment-expires-at".to_owned(), expires_at.to_owned());
        }
        if let Some(content_type) = challenge.content_type {
            headers.insert("content-type".to_owned(), content_type);
        }
        self.authorize(
            original,
            &PayerHttpResponse {
                status: 402,
                headers,
                body: challenge.body,
            },
            context,
        )
        .await
    }

    pub async fn report(
        &self,
        context: &PayerRequestContext,
        authorized: &AuthorizedPayment,
        response: &PayerHttpResponse,
    ) -> Result<()> {
        if authorized.intent.protocol != authorized.credential.protocol {
            return Err(Error::UnsafeRetry(
                "outbound payment protocol binding is invalid".to_owned(),
            ));
        }
        let expected_protocol = authorized.credential.protocol.as_str();
        let adapter = self.protocols.get(expected_protocol)?;
        let http_evidence = self.protocols.merchant_evidence(&response.headers)?;
        if http_evidence
            .as_ref()
            .is_some_and(|(protocol, _)| protocol != expected_protocol)
        {
            return Err(Error::UnsafeRetry(
                "HTTP settlement receipt protocol does not match the paid credential".to_owned(),
            ));
        }
        let mcp_evidence = if response.body.is_empty() {
            None
        } else if let Ok(message) = serde_json::from_slice::<Value>(&response.body) {
            let empty_meta = Value::Null;
            let meta = message
                .get("result")
                .and_then(|result| result.get("_meta"))
                .unwrap_or(&empty_meta);
            self.protocols
                .mcp_merchant_evidence(meta, self.config.max_challenge_bytes)?
        } else {
            None
        };
        if mcp_evidence
            .as_ref()
            .is_some_and(|(protocol, _)| protocol != expected_protocol)
        {
            return Err(Error::UnsafeRetry(
                "MCP settlement receipt protocol does not match the paid credential".to_owned(),
            ));
        }
        let http_evidence = http_evidence.map(|(_, evidence)| evidence);
        let mcp_evidence = mcp_evidence.map(|(_, evidence)| evidence);
        if http_evidence.is_some() && mcp_evidence.is_some() && http_evidence != mcp_evidence {
            return Err(Error::UnsafeRetry(
                "HTTP and MCP settlement evidence conflict".to_owned(),
            ));
        }
        if let Some(evidence) = http_evidence.or(mcp_evidence) {
            let operation = OperationContext::new(self.config.scenario_id.clone(), context.run_id.clone())?;
            self.report_outbound_response(
                &authorized.intent.id,
                &ReportOutboundResponseRequest::protocol(
                    operation,
                    adapter.name(),
                    evidence.header_name,
                    evidence.header_value,
                ),
            )
            .await?;
        } else if (200..300).contains(&response.status) {
            return Err(Error::UnsafeRetry(format!(
                "successful {} response has no settlement evidence; payment remains reserved",
                adapter.name()
            )));
        }
        Ok(())
    }

    pub(crate) async fn deliver<D>(
        &self,
        context: &PayerRequestContext,
        authorized: &AuthorizedPayment,
        delivery: &mut D,
    ) -> Result<D::Response>
    where
        D: PayerDelivery,
    {
        for attempt in 0..=self.config.delivery_retries {
            let attempt_no = attempt + 1;
            tracing::info!(
                event.name = "agentvault.sdk.payer.retrying",
                run.id = %context.run_id,
                attempt.no = attempt_no,
                payment.intent_id = %authorized.intent.id
            );
            let response = delivery.send(attempt_no).await?;
            let view = delivery.view(&response);
            self.report(context, authorized, &view).await?;
            if view.status != 503 || attempt == self.config.delivery_retries {
                return Ok(response);
            }
        }
        Err(Error::Transport("payer retry loop ended unexpectedly".to_owned()))
    }

    async fn authorize_payment(&self, request: &AuthorizePaymentRequest) -> Result<AuthorizePaymentResponse> {
        request.validate()?;
        self.api.post(crate::paths::AUTHORIZE_PAYMENT.to_owned(), request).await
    }

    async fn authorize_until_terminal(&self, request: &AuthorizePaymentRequest) -> Result<AuthorizePaymentResponse> {
        let deadline = tokio::time::Instant::now() + self.config.approval_timeout;
        let mut expected_intent = None::<String>;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Transport(
                    "payment approval did not complete before the total timeout".to_owned(),
                ));
            }
            let response = tokio::time::timeout(remaining, self.authorize_payment(request))
                .await
                .map_err(|_| {
                    Error::Transport("payment approval did not complete before the total timeout".to_owned())
                })??;
            let intent_id = match &response.authorization {
                PaymentAuthorizationResult::Denied { intent_id, .. }
                | PaymentAuthorizationResult::RequiresApproval { intent_id, .. }
                | PaymentAuthorizationResult::Authorized { intent_id, .. } => intent_id,
            };
            if intent_id != &response.intent.id {
                return Err(Error::Decode(
                    "authorization result and payment intent do not match".to_owned(),
                ));
            }
            if let Some(expected) = &expected_intent {
                if expected != intent_id {
                    return Err(Error::Conflict {
                        code: "RUN_CONFLICT".to_owned(),
                        message: "replayed run changed payment intent".to_owned(),
                    });
                }
            } else {
                expected_intent = Some(intent_id.clone());
            }
            if !matches!(
                response.authorization,
                PaymentAuthorizationResult::RequiresApproval { .. }
            ) {
                return Ok(response);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Transport(
                    "payment approval did not complete before the total timeout".to_owned(),
                ));
            }
            tokio::time::sleep(self.config.approval_poll_interval.min(remaining)).await;
        }
    }

    pub async fn get_payment(&self, scenario_id: &str, intent_id: &str) -> Result<PaymentIntent> {
        crate::model_base::validate_required("scenario_id", scenario_id)?;
        crate::model_base::validate_required("intent_id", intent_id)?;
        self.api
            .get(format!(
                "{}/{intent_id}?scenario_id={scenario_id}",
                crate::paths::PAYMENTS,
                intent_id = percent_encode(intent_id),
                scenario_id = percent_encode(scenario_id)
            ))
            .await
    }

    pub async fn get_receipt(&self, scenario_id: &str, intent_id: &str) -> Result<PaymentReceipt> {
        crate::model_base::validate_required("scenario_id", scenario_id)?;
        crate::model_base::validate_required("intent_id", intent_id)?;
        self.api
            .get(format!(
                "{}/{intent_id}/receipt?scenario_id={scenario_id}",
                crate::paths::PAYMENTS,
                intent_id = percent_encode(intent_id),
                scenario_id = percent_encode(scenario_id)
            ))
            .await
    }

    async fn report_outbound_response(
        &self,
        intent_id: &str,
        request: &ReportOutboundResponseRequest,
    ) -> Result<ReportOutboundResponseResponse> {
        crate::model_base::validate_required("intent_id", intent_id)?;
        request.validate()?;
        self.api
            .post(
                format!(
                    "{}/{}/outbound-response",
                    crate::paths::PAYMENTS,
                    percent_encode(intent_id)
                ),
                request,
            )
            .await
    }

    pub async fn get_balance(&self, query: &BalanceQuery) -> Result<PaymentBalance> {
        crate::model_base::validate_required("scenario_id", &query.scenario_id)?;
        crate::model_base::validate_required("wallet_id", &query.wallet_id)?;
        crate::model_base::validate_required("asset.network", &query.asset.network)?;
        crate::model_base::validate_required("asset.asset", &query.asset.asset)?;
        let mut path = format!(
            "{}?scenario_id={}&wallet_id={}&network={}&decimals={}",
            crate::paths::PAYMENT_BALANCE,
            percent_encode(&query.scenario_id),
            percent_encode(&query.wallet_id),
            percent_encode(&query.asset.network),
            query.asset.decimals
        );
        if let Some(contract) = &query.asset.token_contract {
            path.push_str("&token_contract=");
            path.push_str(&percent_encode(contract));
        }
        path.push_str("&asset=");
        path.push_str(&percent_encode(&query.asset.asset));
        self.api.get(path).await
    }
}

pub(crate) fn payment_required(
    request: &PayerHttpRequest,
    response: &PayerHttpResponse,
    max_challenge_bytes: usize,
) -> Result<Option<PaymentRequired>> {
    if response.status != 402 {
        return Ok(None);
    }
    if request.follow_redirects {
        return Err(Error::UnsafeRetry("paid requests must disable redirects".to_owned()));
    }
    if response.body.len() > max_challenge_bytes {
        return Err(Error::Decode("payment challenge exceeds configured limit".to_owned()));
    }
    if request.origin.trim().is_empty() || request.url.trim().is_empty() {
        return Err(Error::InvalidArgument(
            "the original request must have a canonical origin and URL".to_owned(),
        ));
    }
    validate_url_origin(&request.url, &request.origin)?;
    let body = String::from_utf8(response.body.clone()).map_err(|_| {
        Error::Decode("a 402 challenge body must be UTF-8; encode binary protocol data in JSON".to_owned())
    })?;
    let expires_at = header(&response.headers, "payment-expires-at").map(str::to_owned);
    Ok(Some(PaymentRequired {
        protocol_hint: header(&response.headers, "payment-protocol").map(str::to_owned),
        origin: request.origin.clone(),
        resource: request.url.clone(),
        content_type: header(&response.headers, "content-type")
            .unwrap_or("application/json")
            .to_owned(),
        headers: response.headers.clone(),
        body,
        expires_at,
    }))
}

pub(crate) fn validate_payment_binding(
    original: &PayerHttpRequest,
    context: &PayerRequestContext,
    config: &PayerConfig,
    protocol_hint: Option<&str>,
    intent: &PaymentIntent,
    credential: &PaymentCredential,
) -> Result<()> {
    if intent.id.is_empty()
        || intent.scenario_id != config.scenario_id
        || intent.run_id != context.run_id
        || intent.wallet_id.as_deref().is_none_or(str::is_empty)
        || intent.policy_id.as_deref().is_none_or(str::is_empty)
        || intent.direction != crate::PaymentDirection::Outbound
        || protocol_hint.is_some_and(|expected| intent.protocol != expected)
        || intent.protocol != credential.protocol
        || intent.resource != original.url
        || intent.challenge_hash.as_deref() != Some(credential.challenge_hash.as_str())
    {
        return Err(Error::UnsafeRetry(
            "credential, outbound intent, and paid resource are not bound to the same payment".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_url_origin(url: &str, origin: &str) -> Result<()> {
    if crate::model_payer::http_url_matches_origin(url, origin) {
        Ok(())
    } else {
        Err(Error::UnsafeRetry(
            "request URL does not match its canonical HTTP(S) origin".to_owned(),
        ))
    }
}

pub(crate) fn header<'a>(headers: &'a std::collections::BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.as_str())
}

pub(crate) fn validate_header_name(name: &str) -> Result<()> {
    const RESERVED: [&str; 11] = [
        "host",
        "content-length",
        "connection",
        "transfer-encoding",
        "upgrade",
        "proxy-authorization",
        "proxy-authenticate",
        "te",
        "trailer",
        "accept",
        "content-type",
    ];
    let valid = !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        });
    if !valid || RESERVED.iter().any(|reserved| name.eq_ignore_ascii_case(reserved)) {
        return Err(Error::UnsafeRetry(
            "Vault returned an invalid or reserved credential header name".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/payer/payer_test.rs"]
mod tests;
