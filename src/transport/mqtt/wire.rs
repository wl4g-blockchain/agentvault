//! Versioned MQTT envelope, topic and trace validation.
use super::*;
#[derive(Debug)]
pub(crate) struct DecisionParseError {
    pub(crate) status: u16,
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl DecisionParseError {
    pub(crate) const fn invalid(message: &'static str) -> Self {
        Self {
            status: 400,
            code: "INVALID_REQUEST",
            message,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CommandKind {
    Authorize,
    OutboundResponse,
    ResolvePrice,
    CreateChallenge,
    VerifyPayment,
    SettlePayment,
    ApprovalDecision,
}

impl CommandKind {
    pub(crate) const fn cache_scope(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::OutboundResponse => "outbound-response",
            Self::ResolvePrice => "resolve-offer",
            Self::CreateChallenge => "create-challenge",
            Self::VerifyPayment => "verify-payment",
            Self::SettlePayment => "settle-payment",
            Self::ApprovalDecision => "approval-decision",
        }
    }
}

pub(crate) enum CommandTopic {
    Authorize(String),
    OutboundResponse(String),
    ResolvePrice(String),
    CreateChallenge(String),
    VerifyPayment(String),
    SettlePayment(String),
    ApprovalDecision {
        client_id: String,
        approver_principal: String,
    },
}

impl CommandTopic {
    pub(crate) fn client_id(&self) -> &str {
        match self {
            Self::Authorize(client_id)
            | Self::OutboundResponse(client_id)
            | Self::ResolvePrice(client_id)
            | Self::CreateChallenge(client_id)
            | Self::VerifyPayment(client_id)
            | Self::SettlePayment(client_id)
            | Self::ApprovalDecision { client_id, .. } => client_id,
        }
    }

    pub(crate) fn approver_principal(&self) -> Option<&str> {
        match self {
            Self::ApprovalDecision { approver_principal, .. } => Some(approver_principal),
            Self::Authorize(_)
            | Self::OutboundResponse(_)
            | Self::ResolvePrice(_)
            | Self::CreateChallenge(_)
            | Self::VerifyPayment(_)
            | Self::SettlePayment(_) => None,
        }
    }

    pub(crate) const fn kind(&self) -> CommandKind {
        match self {
            Self::Authorize(_) => CommandKind::Authorize,
            Self::OutboundResponse(_) => CommandKind::OutboundResponse,
            Self::ResolvePrice(_) => CommandKind::ResolvePrice,
            Self::CreateChallenge(_) => CommandKind::CreateChallenge,
            Self::VerifyPayment(_) => CommandKind::VerifyPayment,
            Self::SettlePayment(_) => CommandKind::SettlePayment,
            Self::ApprovalDecision { .. } => CommandKind::ApprovalDecision,
        }
    }

    pub(crate) fn cache_scope(&self) -> String {
        match self {
            Self::ApprovalDecision {
                client_id,
                approver_principal,
            } => format!(
                "{client_id}:{}:{approver_principal}",
                CommandKind::ApprovalDecision.cache_scope()
            ),
            Self::Authorize(client_id) => {
                format!("{client_id}:{}", CommandKind::Authorize.cache_scope())
            }
            Self::OutboundResponse(client_id) => {
                format!("{client_id}:{}", CommandKind::OutboundResponse.cache_scope())
            }
            Self::ResolvePrice(client_id) => {
                format!("{client_id}:{}", CommandKind::ResolvePrice.cache_scope())
            }
            Self::CreateChallenge(client_id) => {
                format!("{client_id}:{}", CommandKind::CreateChallenge.cache_scope())
            }
            Self::VerifyPayment(client_id) => {
                format!("{client_id}:{}", CommandKind::VerifyPayment.cache_scope())
            }
            Self::SettlePayment(client_id) => {
                format!("{client_id}:{}", CommandKind::SettlePayment.cache_scope())
            }
        }
    }

    pub(crate) fn response_topic(&self, prefix: &str, request_span_id: &str) -> String {
        match self {
            Self::Authorize(client_id)
            | Self::OutboundResponse(client_id)
            | Self::ResolvePrice(client_id)
            | Self::CreateChallenge(client_id)
            | Self::VerifyPayment(client_id)
            | Self::SettlePayment(client_id) => {
                format!("{prefix}/{client_id}/results/{request_span_id}")
            }
            Self::ApprovalDecision {
                client_id,
                approver_principal,
            } => format!("{prefix}/{client_id}/approvers/{approver_principal}/results/{request_span_id}"),
        }
    }
}

pub(crate) struct MqttWireCodec;

impl MqttWireCodec {
    pub(crate) fn parse_authorize_command(payload: &[u8]) -> Result<AuthorizePaymentRequest, &'static str> {
        if payload.len() > MAX_ENVELOPE_BYTES {
            return Err("MQTT authorization envelope is too large");
        }
        let envelope: RequestEnvelope =
            serde_json::from_slice(payload).map_err(|_| "invalid MQTT authorization envelope")?;
        if envelope.message_type != "payment.authorize"
            || envelope.version != 1
            || !MqttWireCodec::valid_request_span_id(&envelope.request_span_id)
            || envelope.request.method != "POST"
            || envelope.request.path != AUTHORIZE_PATH
        {
            return Err("invalid MQTT authorization envelope contract");
        }
        MqttWireCodec::validate_authorize_headers(&envelope.request.headers, &envelope.request_span_id)?;
        let body = MqttWireCodec::decode_canonical_body(&envelope.request.body)?;
        let request: AuthorizePaymentRequest =
            serde_json::from_slice(&body).map_err(|_| "invalid MQTT authorization request body")?;
        Ok(request)
    }

    pub(crate) fn parse_outbound_response_command(payload: &[u8]) -> Result<OutboundResponseCommand, &'static str> {
        if payload.len() > MAX_ENVELOPE_BYTES {
            return Err("MQTT outbound response envelope is too large");
        }
        let envelope: RequestEnvelope =
            serde_json::from_slice(payload).map_err(|_| "invalid MQTT outbound response envelope")?;
        let intent_id = MqttWireCodec::outbound_intent_id_from_path(&envelope.request.path)
            .ok_or("invalid MQTT outbound response path")?
            .to_owned();
        if envelope.message_type != "payment.outbound_response"
            || envelope.version != 1
            || !MqttWireCodec::valid_request_span_id(&envelope.request_span_id)
            || envelope.request.method != "POST"
        {
            return Err("invalid MQTT outbound response envelope contract");
        }
        MqttWireCodec::validate_authorize_headers(&envelope.request.headers, &envelope.request_span_id)?;
        let body = MqttWireCodec::decode_canonical_body(&envelope.request.body)?;
        let request: ReportOutboundResponseRequest =
            serde_json::from_slice(&body).map_err(|_| "invalid MQTT outbound response body")?;
        Ok(OutboundResponseCommand { intent_id, request })
    }

    pub(crate) fn parse_payee_command<T>(payload: &[u8], message_type: &str, path: &str) -> Result<T, &'static str>
    where
        T: serde::de::DeserializeOwned,
    {
        if payload.len() > MAX_ENVELOPE_BYTES {
            return Err("MQTT payee envelope is too large");
        }
        let envelope: RequestEnvelope = serde_json::from_slice(payload).map_err(|_| "invalid MQTT payee envelope")?;
        if envelope.message_type != message_type
            || envelope.version != 1
            || !MqttWireCodec::valid_request_span_id(&envelope.request_span_id)
            || envelope.request.method != "POST"
            || envelope.request.path != path
        {
            return Err("invalid MQTT payee envelope contract");
        }
        MqttWireCodec::validate_wire_headers(&envelope.request.headers, &envelope.request_span_id)?;
        let body = MqttWireCodec::decode_canonical_body(&envelope.request.body)?;
        serde_json::from_slice(&body).map_err(|_| "invalid MQTT payee request body")
    }

    pub(crate) fn outbound_intent_id_from_path(path: &str) -> Option<&str> {
        let intent_id = path
            .strip_prefix("/api/v1/payments/")?
            .strip_suffix("/outbound-response")?;
        (!intent_id.contains('/') && MqttWireCodec::valid_identifier(intent_id)).then_some(intent_id)
    }

    pub(crate) fn parse_decision_command(
        payload: &[u8],
    ) -> std::result::Result<ApprovalDecisionCommand, DecisionParseError> {
        if payload.len() > MAX_ENVELOPE_BYTES {
            return Err(DecisionParseError::invalid("MQTT approval envelope is too large"));
        }
        let envelope: RequestEnvelope = serde_json::from_slice(payload)
            .map_err(|_| DecisionParseError::invalid("invalid MQTT approval envelope"))?;
        if envelope.message_type != "payment.approval_decision"
            || envelope.version != 1
            || !MqttWireCodec::valid_request_span_id(&envelope.request_span_id)
            || envelope.request.method != "POST"
        {
            return Err(DecisionParseError::invalid("invalid MQTT approval envelope contract"));
        }
        let path_approval_id = MqttWireCodec::approval_id_from_decision_path(&envelope.request.path)
            .ok_or_else(|| DecisionParseError::invalid("invalid MQTT approval decision path"))?;
        MqttWireCodec::validate_decision_headers(&envelope.request.headers, &envelope.request_span_id)
            .map_err(DecisionParseError::invalid)?;
        let body = MqttWireCodec::decode_canonical_body(&envelope.request.body).map_err(DecisionParseError::invalid)?;
        let request: SubmitApprovalDecisionRequest = serde_json::from_slice(&body)
            .map_err(|_| DecisionParseError::invalid("invalid MQTT approval decision body"))?;
        if path_approval_id != request.approval_id {
            return Err(DecisionParseError::invalid("approval path and body binding mismatch"));
        }
        Ok(ApprovalDecisionCommand { request })
    }

    pub(crate) fn approval_id_from_decision_path(path: &str) -> Option<&str> {
        let value = path.strip_prefix("/api/v1/approvals/")?.strip_suffix("/decision")?;
        (!value.is_empty()
            && !value.contains('/')
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
        .then_some(value)
    }

    pub(crate) fn validate_authorize_headers(
        headers: &BTreeMap<String, String>,
        request_span_id: &str,
    ) -> Result<(), &'static str> {
        MqttWireCodec::validate_wire_headers(headers, request_span_id)
    }

    pub(crate) fn validate_decision_headers(
        headers: &BTreeMap<String, String>,
        request_span_id: &str,
    ) -> Result<(), &'static str> {
        MqttWireCodec::validate_wire_headers(headers, request_span_id)
    }

    pub(crate) fn validate_wire_headers(
        headers: &BTreeMap<String, String>,
        request_span_id: &str,
    ) -> Result<(), &'static str> {
        if headers.is_empty() || headers.len() > MAX_HEADER_COUNT {
            return Err("invalid MQTT authorization headers");
        }
        for (name, value) in headers {
            if name.is_empty()
                || name.len() > MAX_HEADER_NAME_BYTES
                || value.len() > MAX_HEADER_VALUE_BYTES
                || !name.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'-')
                || value.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
                || !matches!(name.as_str(), "accept" | "content-type" | "traceparent")
            {
                return Err("invalid MQTT authorization headers");
            }
        }
        if headers.get("content-type").map(String::as_str) != Some("application/json") {
            return Err("content-type must be application/json");
        }
        if headers.get("accept").is_some_and(|value| value != "application/json") {
            return Err("accept must be application/json");
        }
        if MqttWireCodec::traceparent_span_id(headers.get("traceparent").map_or("", String::as_str))
            != Some(request_span_id)
        {
            return Err("requestSpanId must equal the W3C producer span ID");
        }
        Ok(())
    }

    pub(crate) fn decode_canonical_body(encoded: &str) -> Result<Vec<u8>, &'static str> {
        let max_encoded = MAX_BODY_BYTES.saturating_mul(4).div_ceil(3);
        if encoded.len() > max_encoded || !encoded.bytes().all(MqttWireCodec::is_base64url_byte) {
            return Err("invalid or oversized MQTT authorization body");
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "invalid MQTT authorization body encoding")?;
        if decoded.len() > MAX_BODY_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
            return Err("non-canonical or oversized MQTT authorization body");
        }
        Ok(decoded)
    }

    pub(crate) const fn is_base64url_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
    }

    pub(crate) fn probe_trace_context(payload: &[u8]) -> Option<(String, String, String)> {
        let value: Value = serde_json::from_slice(payload).ok()?;
        let request_span_id = value.get("requestSpanId")?.as_str()?;
        let traceparent = value.get("request")?.get("headers")?.get("traceparent")?.as_str()?;
        let trace_id = MqttWireCodec::traceparent_trace_id(traceparent)?;
        (MqttWireCodec::valid_request_span_id(request_span_id)
            && MqttWireCodec::traceparent_span_id(traceparent) == Some(request_span_id))
        .then(|| (request_span_id.to_owned(), traceparent.to_owned(), trace_id.to_owned()))
    }

    pub(crate) fn traceparent_trace_id(value: &str) -> Option<&str> {
        MqttWireCodec::valid_traceparent(value).map(|(_, trace_id, _, _)| trace_id)
    }

    pub(crate) fn traceparent_span_id(value: &str) -> Option<&str> {
        MqttWireCodec::valid_traceparent(value).map(|(_, _, span_id, _)| span_id)
    }

    pub(crate) fn valid_traceparent(value: &str) -> Option<(&str, &str, &str, &str)> {
        let mut fields = value.split('-');
        let result = (fields.next()?, fields.next()?, fields.next()?, fields.next()?);
        if fields.next().is_some()
            || result.0 != "00"
            || !MqttWireCodec::valid_lower_hex(result.1, 32)
            || result.1.bytes().all(|byte| byte == b'0')
            || !MqttWireCodec::valid_lower_hex(result.2, 16)
            || result.2.bytes().all(|byte| byte == b'0')
            || !MqttWireCodec::valid_lower_hex(result.3, 2)
        {
            return None;
        }
        Some(result)
    }

    pub(crate) fn valid_lower_hex(value: &str, length: usize) -> bool {
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }

    pub(crate) fn valid_request_span_id(value: &str) -> bool {
        value != "0000000000000000"
            && value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }

    pub(crate) fn valid_identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }

    pub(crate) fn command_topic(prefix: &str, topic: &str) -> Option<CommandTopic> {
        let relative = topic.strip_prefix(prefix)?.strip_prefix('/')?;
        let segments = relative.split('/').collect::<Vec<_>>();
        if let [client_id, APPROVER_SEGMENT, approver_principal, DECISION_SEGMENT] = segments.as_slice() {
            return (MqttWireCodec::valid_identifier(client_id) && MqttWireCodec::valid_identifier(approver_principal))
                .then(|| CommandTopic::ApprovalDecision {
                    client_id: (*client_id).to_owned(),
                    approver_principal: (*approver_principal).to_owned(),
                });
        }
        for (suffix, constructor) in [
            (COMMAND_SUFFIX, CommandTopic::Authorize as fn(String) -> CommandTopic),
            (
                OUTBOUND_RESPONSE_SUFFIX,
                CommandTopic::OutboundResponse as fn(String) -> CommandTopic,
            ),
            (
                RESOLVE_OFFER_SUFFIX,
                CommandTopic::ResolvePrice as fn(String) -> CommandTopic,
            ),
            (
                CREATE_CHALLENGE_SUFFIX,
                CommandTopic::CreateChallenge as fn(String) -> CommandTopic,
            ),
            (
                VERIFY_PAYMENT_SUFFIX,
                CommandTopic::VerifyPayment as fn(String) -> CommandTopic,
            ),
            (
                SETTLE_PAYMENT_SUFFIX,
                CommandTopic::SettlePayment as fn(String) -> CommandTopic,
            ),
        ] {
            if let Some(client_id) = relative.strip_suffix(suffix) {
                return (!client_id.contains('/') && MqttWireCodec::valid_identifier(client_id))
                    .then(|| constructor(client_id.to_owned()));
            }
        }
        None
    }

    pub(crate) fn success_response(
        message_type: &'static str,
        request_span_id: &str,
        response: &impl Serialize,
    ) -> ResponseEnvelope {
        match serde_json::to_vec(response) {
            Ok(body) => MqttWireCodec::encoded_response(message_type, request_span_id, 200, body),
            Err(_) => MqttWireCodec::error_response(
                request_span_id,
                500,
                "INTERNAL_ERROR",
                "the operation could not be completed",
            ),
        }
    }

    pub(crate) fn response_for_error(request_span_id: &str, error: Error) -> ResponseEnvelope {
        let (status, code, message) = match error {
            Error::InvalidRequest(_) | Error::PaymentProtocol(_) => (400, "INVALID_REQUEST", error.to_string()),
            Error::Unauthenticated => (
                401,
                "UNAUTHORIZED",
                "a valid authenticated MQTT identity is required".to_owned(),
            ),
            Error::Forbidden | Error::PolicyDenied(_) => (403, "FORBIDDEN", error.to_string()),
            Error::NotFound(_) => (404, "NOT_FOUND", error.to_string()),
            Error::PaymentConflict(_) | Error::StorageConflict(_) | Error::InvalidTransition(_) => {
                (409, "PAYMENT_CONFLICT", error.to_string())
            }
            other => {
                tracing::error!(error = %other, "MQTT payment operation failed");
                (500, "INTERNAL_ERROR", "the operation could not be completed".to_owned())
            }
        };
        MqttWireCodec::error_response(request_span_id, status, code, &message)
    }

    pub(crate) fn error_response(
        request_span_id: &str,
        status: u16,
        code: &'static str,
        message: &str,
    ) -> ResponseEnvelope {
        let body = serde_json::to_vec(&json!({"error": {"code": code, "message": message}})).unwrap_or_else(|_| {
            br#"{"error":{"code":"INTERNAL_ERROR","message":"the operation could not be completed"}}"#.to_vec()
        });
        MqttWireCodec::encoded_response("payment.error", request_span_id, status, body)
    }

    pub(crate) fn encoded_response(
        message_type: &'static str,
        request_span_id: &str,
        status: u16,
        body: Vec<u8>,
    ) -> ResponseEnvelope {
        ResponseEnvelope {
            message_type: message_type.to_owned(),
            version: 1,
            request_span_id: request_span_id.to_owned(),
            status,
            headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
            body: URL_SAFE_NO_PAD.encode(body),
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/transport/mqtt/wire_test.rs"]
mod tests;
