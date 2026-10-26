use crate::A2aCall;
use std::collections::BTreeMap;

use mpp::{
    error::PaymentError,
    format_authorization, format_receipt, parse_authorization, parse_receipt,
    protocol::core::{extract_payment_scheme, with_private_cache_control, Receipt as MppReceipt},
};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{
    Error, HttpCredential, MerchantEvidence, PaymentChallenge, PaymentCredential, PaymentProtocol,
    ProtocolHttpResponse, ProtocolMcpMetadata, ProtocolMcpPaymentRequired, Result, SettledIncomingPayment,
};

pub struct MppAdapter;

impl MppAdapter {
    fn tempo_charge_credential(value: &str, label: &str) -> Result<mpp::PaymentCredential> {
        let credential = parse_authorization(value)
            .map_err(|error| Error::InvalidArgument(format!("{label} is invalid: {error}")))?;
        if credential.challenge.method.as_str() != "tempo" || credential.challenge.intent.as_str() != "charge" {
            return Err(Error::InvalidArgument(
                "MPP supports only the implemented Tempo charge flow".to_owned(),
            ));
        }
        Ok(credential)
    }

    fn require_delivery_header(header: Option<&str>, actual: &str) -> Result<()> {
        let expected = header.unwrap_or("Authorization");
        if !expected.eq_ignore_ascii_case(actual) {
            return Err(Error::InvalidArgument(
                "MPP credential delivery does not match its challenged header".to_owned(),
            ));
        }
        Ok(())
    }

    fn bounded_json(value: &Value, limit: usize, label: &str) -> Result<Vec<u8>> {
        let encoded = serde_json::to_vec(value).map_err(|error| Error::Encode(error.to_string()))?;
        if encoded.is_empty() || encoded.len() > limit {
            return Err(Error::InvalidArgument(format!("{label} is empty or oversized")));
        }
        Ok(encoded)
    }

    fn payment_error_data(value: &Value, label: &str) -> Result<mpp::mcp::McpPaymentErrorData> {
        let data = serde_json::from_value::<mpp::mcp::McpPaymentErrorData>(value.clone())
            .map_err(|error| Error::InvalidArgument(format!("{label} is invalid: {error}")))?;
        if data.http_status != 402 || data.challenges.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "{label} must carry HTTP 402 and at least one challenge"
            )));
        }
        Ok(data)
    }

    fn selected_mcp_challenge(message: &Value) -> Result<Option<mpp::PaymentChallenge>> {
        let from_error = message
            .get("error")
            .and_then(|value| mpp::mcp::is_payment_required(value).then_some(value));
        let from_error = from_error
            .map(|value| {
                let error = serde_json::from_value::<mpp::mcp::McpPaymentError>(value.clone())
                    .map_err(|error| Error::InvalidArgument(format!("MPP MCP error is invalid: {error}")))?;
                let data = error
                    .data
                    .ok_or_else(|| Error::InvalidArgument("MPP MCP error omitted challenge data".to_owned()))?;
                if data.http_status != 402 || data.challenges.is_empty() {
                    return Err(Error::InvalidArgument(
                        "MPP MCP error must carry HTTP 402 and at least one challenge".to_owned(),
                    ));
                }
                Ok(data.challenges)
            })
            .transpose()?;

        let result_meta = message.get("result").and_then(|result| result.get("_meta"));
        let from_result = result_meta
            .and_then(|meta| meta.get(mpp::mcp::PAYMENT_REQUIRED_META_KEY))
            .map(|value| Self::payment_error_data(value, "MPP MCP result challenge"))
            .transpose()?
            .map(|data| data.challenges);

        let challenges = match (from_error, from_result) {
            (Some(left), Some(right)) => {
                let left_value = serde_json::to_value(&left).map_err(|error| Error::Encode(error.to_string()))?;
                let right_value = serde_json::to_value(&right).map_err(|error| Error::Encode(error.to_string()))?;
                if left_value != right_value {
                    return Err(Error::Decode(
                        "MPP MCP error challenge conflicts with result metadata".to_owned(),
                    ));
                }
                left
            }
            (Some(value), None) | (None, Some(value)) => value,
            (None, None) => return Ok(None),
        };
        challenges
            .into_iter()
            .find(|challenge| challenge.method.as_str() == "tempo" && challenge.intent.as_str() == "charge")
            .map(Some)
            .ok_or_else(|| Error::InvalidArgument("MPP MCP challenge has no supported Tempo charge".to_owned()))
    }

    fn receipt_value(payment: &SettledIncomingPayment) -> Result<MppReceipt> {
        let receipt = &payment.receipt;
        let transaction = receipt
            .tx_hash
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Decode("confirmed MPP receipt has no transaction".to_owned()))?;
        if receipt.protocol != "mpp" || receipt.confirmed_at <= 0 {
            return Err(Error::Decode(
                "confirmed MPP receipt omitted transaction or confirmation time".to_owned(),
            ));
        }
        let timestamp = OffsetDateTime::from_unix_timestamp(receipt.confirmed_at)
            .map_err(|_| Error::Decode("MPP confirmation time is invalid".to_owned()))?
            .format(&Rfc3339)
            .map_err(|_| Error::Decode("MPP confirmation time cannot be encoded".to_owned()))?;
        let mut value = MppReceipt::success("tempo", transaction).with_external_id(payment.intent.id.clone());
        value.timestamp = timestamp;
        Ok(value)
    }

    fn stored_mcp_challenge(challenge: &PaymentChallenge) -> Result<mpp::PaymentChallenge> {
        if challenge.protocol != "mpp" {
            return Err(Error::Decode("MPP challenge protocol mismatch".to_owned()));
        }
        let value = challenge
            .payload
            .get("mcp_challenge")
            .cloned()
            .ok_or_else(|| Error::Decode("MPP challenge omitted MCP wire projection".to_owned()))?;
        let data = json!({"httpStatus": 402, "challenges": [value]});
        let parsed = Self::payment_error_data(&data, "stored MPP MCP challenge")?;
        let payment = parsed
            .challenges
            .into_iter()
            .next()
            .ok_or_else(|| Error::Decode("stored MPP MCP challenge is empty".to_owned()))?;
        if payment.method.as_str() != "tempo" || payment.intent.as_str() != "charge" {
            return Err(Error::InvalidArgument(
                "MPP supports only the implemented Tempo charge flow".to_owned(),
            ));
        }
        Ok(payment)
    }
}

impl PaymentProtocol for MppAdapter {
    fn a2a_challenge(&self, message: &Value, limit: usize) -> Result<Option<ProtocolHttpResponse>> {
        if message.get("error").is_some() {
            self.mcp_challenge(message, limit)
        } else {
            Ok(None)
        }
    }
    fn presented_a2a_credential(&self, message: &Value, limit: usize) -> Result<Option<Value>> {
        let key = self
            .mcp_request_metadata_key()
            .ok_or_else(|| A2aCall::invalid("MPP key missing"))?;
        if message["params"]["_meta"].get(key).is_some() || message["params"]["message"]["metadata"].get(key).is_some()
        {
            return Err(A2aCall::invalid("MPP A2A credential requires root _meta"));
        }
        self.presented_mcp_credential(
            &message.get("_meta").cloned().unwrap_or_else(|| serde_json::json!({})),
            limit,
        )
    }
    fn a2a_strip_credential(&self, message: &mut Value) -> Result<()> {
        A2aCall::metadata(message, true)?.remove("org.paymentauth/credential");
        Ok(())
    }
    fn a2a_apply_credential(&self, message: &mut Value, credential: &PaymentCredential) -> Result<()> {
        let value = self.mcp_credential(credential)?;
        let meta = A2aCall::metadata(message, true)?;
        if meta.contains_key(value.key) {
            return Err(A2aCall::invalid("existing MPP credential"));
        }
        meta.insert(value.key.into(), value.value);
        Ok(())
    }
    fn a2a_payment_required(
        &self,
        challenge: &PaymentChallenge,
        _task: &a2a_types::Task,
        rejected: bool,
    ) -> Result<ProtocolMcpPaymentRequired> {
        if rejected {
            self.mcp_payment_rejected(challenge, "Payment verification failed")
        } else {
            self.mcp_payment_required(challenge)
        }
    }
    fn a2a_evidence(&self, message: &Value, limit: usize) -> Result<Option<MerchantEvidence>> {
        self.mcp_merchant_evidence(
            &message.get("_meta").cloned().unwrap_or_else(|| serde_json::json!({})),
            limit,
        )
    }
    fn a2a_inject_receipt(
        &self,
        message: &mut Value,
        challenge: &PaymentChallenge,
        payment: &SettledIncomingPayment,
    ) -> Result<()> {
        let value = self.mcp_receipt(challenge, payment)?;
        let meta = A2aCall::metadata(message, true)?;
        if meta.contains_key(value.key) {
            return Err(A2aCall::invalid("handler supplied MPP receipt"));
        }
        meta.insert(value.key.into(), value.value);
        Ok(())
    }
    fn name(&self) -> &'static str {
        "mpp"
    }

    fn credential_header_names(&self) -> &'static [&'static str] {
        &["payment-authorization", "authorization"]
    }

    fn merchant_evidence_header_names(&self) -> &'static [&'static str] {
        &["payment-receipt"]
    }

    fn credential(&self, credential: &PaymentCredential) -> Result<HttpCredential> {
        let delivery = credential.http.as_ref().ok_or_else(|| {
            Error::UnsafeRetry("MPP credential has no allowed Payment authorization delivery".to_owned())
        })?;
        let allowed = delivery.header_name.eq_ignore_ascii_case("authorization")
            || delivery.header_name.eq_ignore_ascii_case("payment-authorization");
        if credential.protocol != self.name() || !allowed {
            return Err(Error::UnsafeRetry(
                "MPP credential has no allowed Payment authorization delivery".to_owned(),
            ));
        }
        let parsed = Self::tempo_charge_credential(&delivery.header_value, "MPP credential")
            .map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        Self::require_delivery_header(parsed.challenge.header.as_deref(), &delivery.header_name)
            .map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        Ok(delivery.clone())
    }

    fn merchant_evidence(&self, headers: &BTreeMap<String, String>) -> Result<Option<MerchantEvidence>> {
        let Some(value) = self.header(headers, mpp::PAYMENT_RECEIPT_HEADER) else {
            return Ok(None);
        };
        parse_receipt(value)
            .map_err(|error| Error::InvalidArgument(format!("MPP Payment-Receipt is invalid: {error}")))?;
        Ok(Some(MerchantEvidence {
            header_name: mpp::PAYMENT_RECEIPT_HEADER.to_owned(),
            header_value: value.to_owned(),
        }))
    }

    fn presented_credential(&self, headers: &BTreeMap<String, String>, limit: usize) -> Result<Option<Value>> {
        let dedicated = self.header(headers, "payment-authorization").unwrap_or_default();
        let authorization = self.header(headers, "authorization").unwrap_or_default();
        if !dedicated.is_empty() && extract_payment_scheme(authorization).is_some() {
            return Err(Error::InvalidArgument(
                "multiple MPP payment credentials were supplied".to_owned(),
            ));
        }
        let value = if !dedicated.is_empty() {
            dedicated
        } else if let Some(payment) = extract_payment_scheme(authorization) {
            payment
        } else {
            return Ok(None);
        };
        if value.len() > limit || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
            return Err(Error::InvalidArgument(
                "MPP Payment authorization is invalid or oversized".to_owned(),
            ));
        }
        let parsed = Self::tempo_charge_credential(value, "MPP Payment authorization")?;
        let actual = if dedicated.is_empty() {
            "Authorization"
        } else {
            "Payment-Authorization"
        };
        Self::require_delivery_header(parsed.challenge.header.as_deref(), actual)?;
        Ok(Some(json!({"authorization": value})))
    }

    fn challenge(&self, challenge: &PaymentChallenge) -> Result<ProtocolHttpResponse> {
        let value = challenge
            .payload
            .get("www_authenticate")
            .and_then(Value::as_str)
            .filter(|_| challenge.protocol == self.name())
            .ok_or_else(|| Error::Decode("MPP challenge has no valid WWW-Authenticate value".to_owned()))?;
        let parsed = mpp::parse_www_authenticate(value)
            .map_err(|error| Error::Decode(format!("MPP challenge is invalid: {error}")))?;
        if parsed.method.as_str() != "tempo" || parsed.intent.as_str() != "charge" {
            return Err(Error::InvalidArgument(
                "MPP supports only the implemented Tempo charge flow".to_owned(),
            ));
        }
        Ok(ProtocolHttpResponse {
            headers: BTreeMap::from([(mpp::WWW_AUTHENTICATE_HEADER.to_owned(), value.to_owned())]),
            content_type: Some("application/json".to_owned()),
            body: br#"{"error":{"code":"PAYMENT_REQUIRED"}}"#.to_vec(),
        })
    }

    fn receipt(&self, payment: &SettledIncomingPayment) -> Result<ProtocolHttpResponse> {
        let value = format_receipt(&Self::receipt_value(payment)?)
            .map_err(|error| Error::Encode(format!("encode MPP receipt: {error}")))?;
        Ok(ProtocolHttpResponse {
            headers: BTreeMap::from([
                ("cache-control".to_owned(), with_private_cache_control(None)),
                (mpp::PAYMENT_RECEIPT_HEADER.to_owned(), value),
            ]),
            content_type: None,
            body: Vec::new(),
        })
    }

    fn mcp_challenge(&self, message: &Value, limit: usize) -> Result<Option<ProtocolHttpResponse>> {
        let Some(challenge) = Self::selected_mcp_challenge(message)? else {
            return Ok(None);
        };
        Self::bounded_json(
            &serde_json::to_value(&challenge).map_err(|error| Error::Encode(error.to_string()))?,
            limit,
            "MPP MCP challenge",
        )?;
        let header = mpp::format_www_authenticate(&challenge)
            .map_err(|error| Error::InvalidArgument(format!("MPP MCP challenge is invalid: {error}")))?;
        Ok(Some(ProtocolHttpResponse {
            headers: BTreeMap::from([(mpp::WWW_AUTHENTICATE_HEADER.to_owned(), header)]),
            content_type: Some("application/json".to_owned()),
            body: br#"{"error":{"code":"PAYMENT_REQUIRED"}}"#.to_vec(),
        }))
    }

    fn mcp_credential(&self, credential: &PaymentCredential) -> Result<ProtocolMcpMetadata> {
        let delivery = self.credential(credential)?;
        let parsed = Self::tempo_charge_credential(&delivery.header_value, "MPP credential")
            .map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        let value = mpp::mcp::credential_value(&parsed)
            .map_err(|error| Error::UnsafeRetry(format!("encode MPP MCP credential: {error}")))?;
        Ok(ProtocolMcpMetadata {
            key: mpp::mcp::CREDENTIAL_META_KEY,
            value,
        })
    }

    fn mcp_request_metadata_key(&self) -> Option<&'static str> {
        Some(mpp::mcp::CREDENTIAL_META_KEY)
    }

    fn mcp_root_request_metadata_key(&self) -> Option<&'static str> {
        Some(mpp::mcp::CREDENTIAL_META_KEY)
    }

    fn presented_mcp_credential(&self, meta: &Value, limit: usize) -> Result<Option<Value>> {
        let Some(raw) = meta.get(mpp::mcp::CREDENTIAL_META_KEY) else {
            return Ok(None);
        };
        Self::bounded_json(raw, limit, "MPP MCP credential")?;
        let credential = mpp::mcp::extract_credential(meta)
            .ok_or_else(|| Error::InvalidArgument("MPP MCP credential is invalid".to_owned()))?;
        let authorization = format_authorization(&credential)
            .map_err(|error| Error::InvalidArgument(format!("encode MPP authorization: {error}")))?;
        Self::tempo_charge_credential(&authorization, "MPP MCP credential")?;
        Ok(Some(json!({"authorization": authorization})))
    }

    fn mcp_payment_required(&self, challenge: &PaymentChallenge) -> Result<ProtocolMcpPaymentRequired> {
        let payment = Self::stored_mcp_challenge(challenge)?;
        let error = mpp::mcp::payment_required_error(&payment);
        Ok(ProtocolMcpPaymentRequired::Error(
            serde_json::to_value(error).map_err(|error| Error::Encode(error.to_string()))?,
        ))
    }

    fn mcp_payment_rejected(&self, challenge: &PaymentChallenge, message: &str) -> Result<ProtocolMcpPaymentRequired> {
        let payment = Self::stored_mcp_challenge(challenge)?;
        let problem = mpp::MppError::verification_failed(message).to_problem_details(Some(&payment.id));
        let error = mpp::mcp::payment_required_error_with_problem(&payment, problem);
        Ok(ProtocolMcpPaymentRequired::Error(
            serde_json::to_value(error).map_err(|error| Error::Encode(error.to_string()))?,
        ))
    }

    fn mcp_receipt(
        &self,
        challenge: &PaymentChallenge,
        payment: &SettledIncomingPayment,
    ) -> Result<ProtocolMcpMetadata> {
        if challenge.protocol != self.name()
            || challenge.challenge_hash != payment.intent.challenge_hash.as_deref().unwrap_or_default()
        {
            return Err(Error::Decode("MPP MCP receipt challenge binding is invalid".to_owned()));
        }
        let value = serde_json::to_value(mpp::mcp::McpReceipt {
            receipt: Self::receipt_value(payment)?,
            challenge_id: challenge.nonce.clone(),
        })
        .map_err(|error| Error::Encode(error.to_string()))?;
        Ok(ProtocolMcpMetadata {
            key: mpp::mcp::RECEIPT_META_KEY,
            value,
        })
    }

    fn mcp_receipt_metadata_key(&self) -> Option<&'static str> {
        Some(mpp::mcp::RECEIPT_META_KEY)
    }

    fn mcp_merchant_evidence(&self, meta: &Value, limit: usize) -> Result<Option<MerchantEvidence>> {
        let Some(value) = meta.get(mpp::mcp::RECEIPT_META_KEY) else {
            return Ok(None);
        };
        Self::bounded_json(value, limit, "MPP MCP receipt metadata")?;
        let receipt = serde_json::from_value::<mpp::mcp::McpReceipt>(value.clone())
            .map_err(|error| Error::InvalidArgument(format!("MPP MCP receipt is invalid: {error}")))?;
        if receipt.challenge_id.is_empty() {
            return Err(Error::InvalidArgument("MPP MCP receipt omitted challengeId".to_owned()));
        }
        let encoded = format_receipt(&receipt.receipt)
            .map_err(|error| Error::InvalidArgument(format!("encode MPP receipt: {error}")))?;
        Ok(Some(MerchantEvidence {
            header_name: mpp::PAYMENT_RECEIPT_HEADER.to_owned(),
            header_value: encoded,
        }))
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/mpp_test.rs"]
mod tests;
