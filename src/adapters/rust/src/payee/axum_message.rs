//! Bounded HTTP/MCP/A2A message projection for the Axum adapter.
use super::*;
pub(super) struct McpCall {
    pub(super) a2a: Option<crate::A2aPaymentCall>,
    pub(super) id: Value,
    pub(super) meta: Value,
    pub(super) canonical_body: Vec<u8>,
}

impl McpCall {
    pub(super) async fn a2a<T>(
        core: &PayeeMiddleware<T>,
        body: &[u8],
        operation: &mut PayeeOperation,
        store: Arc<dyn crate::A2aTaskStore>,
        parts: &Parts,
    ) -> crate::Result<Option<Self>> {
        let headers = parts
            .headers
            .iter()
            .filter_map(|(name, v)| v.to_str().ok().map(|v| (name.to_string(), v.to_owned())))
            .collect();
        let Some(call) = core.prepare_a2a(body, operation, store, &headers).await? else {
            return Ok(None);
        };
        let canonical = call.call.body()?;
        operation.request_hash = AxumRequest::request_hash(&parts.method, parts.uri.path(), &canonical);
        operation.credential_payload.clone_from(&call.credential);
        Ok(Some(Self {
            id: call.call.wire["id"].clone(),
            meta: json!({}),
            canonical_body: canonical,
            a2a: Some(call),
        }))
    }

    pub(super) fn parse(
        body: &[u8],
        expected_tool: &str,
        payment_metadata_keys: &[&str],
        root_payment_metadata_keys: &[&str],
    ) -> std::result::Result<Option<Self>, OperationError> {
        let Ok(mut message) = serde_json::from_slice::<Value>(body) else {
            return Ok(None);
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || message.get("method").and_then(Value::as_str) != Some("tools/call")
            || message.pointer("/params/name").and_then(Value::as_str) != Some(expected_tool)
        {
            return Ok(None);
        }
        let id = message
            .get("id")
            .cloned()
            .filter(|value| value.is_string() || value.is_number())
            .ok_or_else(|| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "MCP_REQUEST_INVALID",
                message: "paid MCP tools/call requires a string or numeric JSON-RPC id".to_owned(),
            })?;
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "MCP_REQUEST_INVALID",
                message: "MCP tools/call params must be an object".to_owned(),
            })?;
        let nested_meta = params.get("_meta").cloned().unwrap_or_else(|| json!({}));
        let mut meta = nested_meta.as_object().cloned().ok_or_else(|| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "PAYMENT_INVALID",
            message: "MCP params._meta must be an object".to_owned(),
        })?;
        let root_meta = message.get("_meta").cloned().unwrap_or_else(|| json!({}));
        let root_meta = root_meta.as_object().ok_or_else(|| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "PAYMENT_INVALID",
            message: "JSON-RPC root _meta must be an object".to_owned(),
        })?;
        for key in root_payment_metadata_keys {
            if let Some(value) = root_meta.get(*key) {
                if meta.contains_key(*key) {
                    return Err(OperationError {
                        status: StatusCode::BAD_REQUEST,
                        code: "PAYMENT_INVALID",
                        message: "duplicate MCP payment credential metadata".to_owned(),
                    });
                }
                meta.insert((*key).to_owned(), value.clone());
            }
        }
        let params = message
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .expect("MCP params were validated as an object");
        if let Some(canonical_meta) = params.get_mut("_meta").and_then(Value::as_object_mut) {
            for key in payment_metadata_keys {
                canonical_meta.remove(*key);
            }
            if canonical_meta.is_empty() {
                params.remove("_meta");
            }
        }
        let remove_root_meta = if let Some(canonical_meta) = message.get_mut("_meta").and_then(Value::as_object_mut) {
            for key in root_payment_metadata_keys {
                canonical_meta.remove(*key);
            }
            canonical_meta.is_empty()
        } else {
            false
        };
        if remove_root_meta {
            message
                .as_object_mut()
                .expect("MCP message is an object")
                .remove("_meta");
        }
        Self::sort_object_keys(&mut message);
        message.sort_all_objects();
        let canonical_body = serde_json::to_vec(&message).map_err(|error| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "MCP_REQUEST_INVALID",
            message: format!("MCP request cannot be canonicalized: {error}"),
        })?;
        Ok(Some(Self {
            a2a: None,
            id,
            meta: Value::Object(meta),
            canonical_body,
        }))
    }

    pub(super) fn sort_object_keys(value: &mut Value) {
        match value {
            Value::Array(values) => values.iter_mut().for_each(Self::sort_object_keys),
            Value::Object(object) => {
                let mut fields = std::mem::take(object).into_iter().collect::<Vec<_>>();
                fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
                for (name, mut field) in fields {
                    Self::sort_object_keys(&mut field);
                    object.insert(name, field);
                }
            }
            _ => {}
        }
    }
}

pub(super) struct AxumProtocolResponse;

impl AxumProtocolResponse {
    pub(super) fn challenge<T>(core: &PayeeMiddleware<T>, challenge: &crate::PaymentChallenge) -> Response<Body> {
        let wire = match core.challenge_response(challenge) {
            Ok(wire) => wire,
            Err(error) => {
                return AxumRequest::error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error.to_string())
            }
        };
        let mut response = Response::new(Body::from(wire.body));
        *response.status_mut() = StatusCode::PAYMENT_REQUIRED;
        if let Err(error) = Self::apply_headers(response.headers_mut(), &wire.headers) {
            return AxumRequest::error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error);
        }
        if let Some(content_type) = wire.content_type {
            let Ok(content_type) = HeaderValue::from_str(&content_type) else {
                return AxumRequest::error_response(
                    StatusCode::BAD_GATEWAY,
                    "CHALLENGE_INVALID",
                    "protocol challenge content type is invalid",
                );
            };
            response.headers_mut().insert(header::CONTENT_TYPE, content_type);
        }
        if Self::apply_challenge_metadata(response.headers_mut(), challenge).is_err() {
            return AxumRequest::error_response(
                StatusCode::BAD_GATEWAY,
                "CHALLENGE_INVALID",
                "protocol challenge metadata is invalid",
            );
        }
        response
    }

    pub(super) async fn a2a_challenge<T>(
        core: &PayeeMiddleware<T>,
        call: &crate::A2aPaymentCall,
        challenge: &crate::PaymentChallenge,
        rejected: bool,
    ) -> Response<Body> {
        match core.a2a_challenge(call, challenge, rejected).await {
            Ok(wire) => {
                let mut response = Self::mcp_payment_response(wire, &call.call.wire["id"], challenge);
                if let Ok(extension) = core.a2a_extension(&challenge.protocol) {
                    if !extension.is_empty() {
                        if let Ok(value) = extension.parse() {
                            response.headers_mut().insert("x-a2a-extensions", value);
                        }
                    }
                }
                response
            }
            Err(e) => AxumRequest::sdk_error_response(e),
        }
    }
    pub(super) async fn a2a_receipt<T>(
        core: &PayeeMiddleware<T>,
        call: &crate::A2aPaymentCall,
        challenge: &crate::PaymentChallenge,
        payment: &crate::SettledIncomingPayment,
        response: Response<Body>,
        limit: usize,
    ) -> Response<Body> {
        if !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"))
        {
            return AxumRequest::sdk_error_response(crate::A2aCall::invalid("paid A2A handler must return JSON"));
        }
        let (mut parts, body) = response.into_parts();
        let Some(message) = to_bytes(body, limit)
            .await
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
        else {
            return AxumRequest::sdk_error_response(crate::A2aCall::invalid(
                "A2A response exceeds limit or invalid JSON",
            ));
        };
        let message = match core.a2a_receipt(call, message, challenge, payment).await {
            Ok(v) => v,
            Err(e) => return AxumRequest::sdk_error_response(e),
        };
        let body = match serde_json::to_vec(&message) {
            Ok(v) if v.len() <= limit => v,
            _ => return AxumRequest::sdk_error_response(crate::A2aCall::invalid("A2A response exceeds limit")),
        };
        parts.status = StatusCode::OK;
        parts.headers.remove(header::CONTENT_LENGTH);
        Response::from_parts(parts, Body::from(body))
    }

    pub(super) fn mcp_challenge<T>(
        core: &PayeeMiddleware<T>,
        challenge: &crate::PaymentChallenge,
        id: &Value,
    ) -> Response<Body> {
        let wire = match core.mcp_challenge_response(challenge) {
            Ok(wire) => wire,
            Err(error) => {
                return AxumRequest::error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error.to_string())
            }
        };
        Self::mcp_payment_response(wire, id, challenge)
    }

    pub(super) fn mcp_rejection<T>(
        core: &PayeeMiddleware<T>,
        challenge: &crate::PaymentChallenge,
        id: &Value,
        message: &str,
    ) -> Response<Body> {
        let wire = match core.mcp_rejection_response(challenge, message) {
            Ok(wire) => wire,
            Err(error) => {
                return AxumRequest::error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error.to_string())
            }
        };
        Self::mcp_payment_response(wire, id, challenge)
    }

    pub(super) fn mcp_payment_response(
        wire: crate::ProtocolMcpPaymentRequired,
        id: &Value,
        challenge: &crate::PaymentChallenge,
    ) -> Response<Body> {
        let wire = match wire {
            crate::ProtocolMcpPaymentRequired::Result(result) => {
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            crate::ProtocolMcpPaymentRequired::Error(error) => {
                json!({"jsonrpc": "2.0", "id": id, "error": error})
            }
        };
        let Ok(body) = serde_json::to_vec(&wire) else {
            return AxumRequest::error_response(
                StatusCode::BAD_GATEWAY,
                "CHALLENGE_INVALID",
                "MCP challenge cannot be encoded",
            );
        };
        let mut response = Response::new(Body::from(body));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if Self::apply_challenge_metadata(response.headers_mut(), challenge).is_err() {
            return AxumRequest::error_response(
                StatusCode::BAD_GATEWAY,
                "CHALLENGE_INVALID",
                "protocol challenge metadata is invalid",
            );
        }
        response
    }

    pub(super) fn apply_challenge_metadata(
        headers: &mut axum::http::HeaderMap,
        challenge: &crate::PaymentChallenge,
    ) -> std::result::Result<(), ()> {
        for (name, value) in [
            ("payment-protocol", challenge.protocol.clone()),
            ("payment-expires-at", challenge.expires_at.to_string()),
        ] {
            let value = HeaderValue::from_str(&value).map_err(|_| ())?;
            headers.insert(HeaderName::from_static(name), value);
        }
        Ok(())
    }

    pub(super) async fn mcp_receipt<T>(
        core: &PayeeMiddleware<T>,
        challenge: &crate::PaymentChallenge,
        payment: &crate::SettledIncomingPayment,
        id: &Value,
        response: Response<Body>,
        limit: usize,
    ) -> Response<Body> {
        if !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
        {
            return AxumRequest::error_response(
                StatusCode::BAD_GATEWAY,
                "MCP_RESPONSE_INVALID",
                "paid MCP handlers must return a bounded application/json result",
            );
        }
        let (mut parts, body) = response.into_parts();
        let Ok(body) = to_bytes(body, limit).await else {
            return AxumRequest::error_response(
                StatusCode::BAD_GATEWAY,
                "MCP_RESPONSE_INVALID",
                "paid MCP response exceeds configured limit",
            );
        };
        let message = match Self::decode_mcp_handler_response(core, id, &body) {
            Ok(value) => value,
            Err(message) => {
                return AxumRequest::error_response(StatusCode::BAD_GATEWAY, "MCP_RESPONSE_INVALID", message)
            }
        };
        if !parts.status.is_success() {
            return Response::from_parts(parts, Body::from(body));
        }
        let metadata = match core.mcp_receipt(challenge, payment) {
            Ok(value) => value,
            Err(error) => {
                return AxumRequest::error_response(
                    StatusCode::BAD_GATEWAY,
                    "SETTLEMENT_EVIDENCE_INVALID",
                    &error.to_string(),
                )
            }
        };
        let body = match Self::encode_mcp_receipt(message, metadata, limit) {
            Ok(value) => value,
            Err(message) => {
                return AxumRequest::error_response(StatusCode::BAD_GATEWAY, "MCP_RESPONSE_INVALID", message)
            }
        };
        parts.headers.remove(header::CONTENT_LENGTH);
        parts
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Response::from_parts(parts, Body::from(body))
    }

    pub(super) fn decode_mcp_handler_response<T>(
        core: &PayeeMiddleware<T>,
        id: &Value,
        body: &[u8],
    ) -> std::result::Result<Value, &'static str> {
        let message = serde_json::from_slice::<Value>(body).map_err(|_| "paid MCP handler returned invalid JSON")?;
        if message.get("id") != Some(id) {
            return Err("paid MCP response id does not match its request");
        }
        let supplied_evidence = message
            .get("result")
            .and_then(|result| result.get("_meta"))
            .and_then(Value::as_object)
            .is_some_and(|meta| {
                core.mcp_receipt_metadata_keys()
                    .iter()
                    .any(|key| meta.contains_key(*key))
            });
        if supplied_evidence {
            return Err("paid MCP handler attempted to supply settlement metadata");
        }
        Ok(message)
    }

    pub(super) fn encode_mcp_receipt(
        mut message: Value,
        metadata: crate::ProtocolMcpMetadata,
        limit: usize,
    ) -> std::result::Result<Vec<u8>, &'static str> {
        let result = message
            .get_mut("result")
            .and_then(Value::as_object_mut)
            .ok_or("paid MCP handler must return a JSON-RPC result")?;
        let meta = result
            .entry("_meta")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or("paid MCP result._meta must be an object")?;
        if meta.contains_key(metadata.key) {
            return Err("paid MCP handler attempted to supply settlement metadata");
        }
        meta.insert(metadata.key.to_owned(), metadata.value);
        let body = serde_json::to_vec(&message).map_err(|_| "paid MCP response cannot be encoded")?;
        if body.len() > limit {
            return Err("paid MCP response exceeds configured limit");
        }
        Ok(body)
    }

    pub(super) fn apply_headers(
        target: &mut axum::http::HeaderMap,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "protocol response header name is invalid".to_owned())?;
            let value =
                HeaderValue::from_str(value).map_err(|_| "protocol response header value is invalid".to_owned())?;
            target.insert(name, value);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/payee/axum_message_test.rs"]
mod tests;
