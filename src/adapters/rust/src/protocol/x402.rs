use std::collections::BTreeMap;

use serde_json::{json, Value};
use x402_chain_eip155::v2_eip155_exact::{ExactEvmPayload, PaymentPayload as Eip155PaymentPayload};
use x402_chain_solana::v2_solana_exact::PaymentPayload as SolanaPaymentPayload;
use x402_types::{
    proto::v2::{PaymentPayload as X402PaymentPayload, PaymentRequired as X402PaymentRequired, PaymentRequirements},
    util::b64::Base64Bytes,
};

use crate::{
    Error, HttpCredential, MerchantEvidence, PaymentChallenge, PaymentCredential, PaymentProtocol,
    ProtocolHttpResponse, ProtocolMcpMetadata, ProtocolMcpPaymentRequired, Result, SettledIncomingPayment,
};

pub struct X402Adapter;

const MCP_PAYMENT_KEY: &str = "x402/payment";
const MCP_PAYMENT_RESPONSE_KEY: &str = "x402/payment-response";

impl X402Adapter {
    fn declared_payment_required(value: &Value) -> Result<Option<Value>> {
        let Some(object) = value.as_object() else {
            return Ok(None);
        };
        if !object.contains_key("x402Version") && !object.contains_key("accepts") {
            return Ok(None);
        }
        let parsed = serde_json::from_value::<X402PaymentRequired>(value.clone())
            .map_err(|error| Error::Decode(format!("MCP x402 PaymentRequired is not v2: {error}")))?;
        if parsed.accepts.is_empty() {
            return Err(Error::Decode("MCP x402 PaymentRequired has no accepts".to_owned()));
        }
        Ok(Some(value.clone()))
    }

    fn payment_required_from_message(message: &Value) -> Result<Option<Value>> {
        let Some(result) = message.get("result").and_then(Value::as_object) else {
            return Ok(None);
        };
        if result.get("isError").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let structured = result
            .get("structuredContent")
            .map(Self::declared_payment_required)
            .transpose()?
            .flatten();
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| {
                content.iter().find_map(|item| {
                    (item.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| item.get("text").and_then(Value::as_str))
                        .flatten()
                })
            })
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .map(|value| Self::declared_payment_required(&value))
            .transpose()?
            .flatten();
        match (structured, text) {
            (Some(left), Some(right)) if left != right => Err(Error::Decode(
                "MCP x402 structuredContent conflicts with content text".to_owned(),
            )),
            (Some(value), _) | (_, Some(value)) => Ok(Some(value)),
            (None, None) => Ok(None),
        }
    }

    fn bounded_json(value: &Value, limit: usize, label: &str) -> Result<Vec<u8>> {
        let encoded = serde_json::to_vec(value).map_err(|error| Error::Encode(error.to_string()))?;
        if encoded.is_empty() || encoded.len() > limit {
            return Err(Error::InvalidArgument(format!("{label} is empty or oversized")));
        }
        Ok(encoded)
    }

    fn payment_payload(value: Value, label: &str) -> Result<Value> {
        let payload = serde_json::from_value::<X402PaymentPayload<PaymentRequirements, Value>>(value.clone())
            .map_err(|error| Error::InvalidArgument(format!("{label} is not a valid x402 v2 payload: {error}")))?;
        let network = payload.accepted.network.to_string();
        if payload.accepted.scheme != "exact" {
            return Err(Error::InvalidArgument(format!(
                "{label} uses an unsupported x402 scheme"
            )));
        }
        if network.starts_with("eip155:") {
            if !Self::has_exact_keys(&payload.payload, &["signature", "authorization"])
                || !payload.payload.get("authorization").is_some_and(|authorization| {
                    Self::has_exact_keys(
                        authorization,
                        &["from", "to", "value", "validAfter", "validBefore", "nonce"],
                    )
                })
            {
                return Err(Error::InvalidArgument(format!(
                    "{label} is not an official EIP-3009 payload"
                )));
            }
            let typed = serde_json::from_value::<Eip155PaymentPayload>(value.clone()).map_err(|error| {
                Error::InvalidArgument(format!("{label} is not an official EIP-3009 payload: {error}"))
            })?;
            if !matches!(typed.payload, ExactEvmPayload::Eip3009(_)) {
                return Err(Error::InvalidArgument(format!(
                    "{label} is not an EIP-3009 payment payload"
                )));
            }
        } else if network.starts_with("solana:") {
            if !Self::has_exact_keys(&payload.payload, &["transaction"]) {
                return Err(Error::InvalidArgument(format!(
                    "{label} is not an official Solana exact payload"
                )));
            }
            let typed = serde_json::from_value::<SolanaPaymentPayload>(value.clone()).map_err(|error| {
                Error::InvalidArgument(format!("{label} is not an official Solana exact payload: {error}"))
            })?;
            let encoded = typed.payload.transaction;
            let decoded = Base64Bytes::from(encoded.as_bytes())
                .decode()
                .map_err(|_| Error::InvalidArgument(format!("{label} has no canonical base64 Solana transaction")))?;
            if decoded.is_empty() || Base64Bytes::encode(&decoded).to_string() != encoded {
                return Err(Error::InvalidArgument(format!(
                    "{label} has no canonical base64 Solana transaction"
                )));
            }
        } else {
            return Err(Error::InvalidArgument(format!(
                "{label} uses an unsupported x402 network"
            )));
        }
        Ok(value)
    }

    fn has_exact_keys(value: &Value, expected: &[&str]) -> bool {
        value.as_object().is_some_and(|object| {
            object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
        })
    }

    fn decode_base64_json(encoded: &str, limit: usize, label: &str) -> Result<Value> {
        if encoded.len() > limit.saturating_mul(4).div_ceil(3).saturating_add(4) {
            return Err(Error::InvalidArgument(format!("{label} exceeds configured limit")));
        }
        let decoded = Base64Bytes::from(encoded.as_bytes())
            .decode()
            .map_err(|_| Error::InvalidArgument(format!("{label} is not canonical base64")))?;
        if decoded.len() > limit || Base64Bytes::encode(&decoded).to_string() != encoded {
            return Err(Error::InvalidArgument(format!("{label} is non-canonical or oversized")));
        }
        serde_json::from_slice(&decoded)
            .map_err(|error| Error::InvalidArgument(format!("{label} is not valid JSON: {error}")))
    }

    fn settlement_value(payment: &SettledIncomingPayment) -> Result<Value> {
        let receipt = &payment.receipt;
        let transaction = receipt
            .tx_hash
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Decode("confirmed x402 receipt has no transaction".to_owned()))?;
        let payer = receipt
            .payer
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Decode("confirmed x402 receipt has no trusted payer".to_owned()))?;
        if receipt.protocol != "x402" || receipt.asset.network.is_empty() {
            return Err(Error::Decode("x402 receipt protocol or network is invalid".to_owned()));
        }
        serde_json::to_value(x402_types::proto::v2::SettleResponse::Success {
            payer: payer.to_owned(),
            transaction: transaction.to_owned(),
            network: receipt.asset.network.clone(),
        })
        .map_err(|error| Error::Encode(error.to_string()))
    }

    fn mcp_payment_result(required: &Value) -> Result<ProtocolMcpPaymentRequired> {
        Self::declared_payment_required(required)?
            .ok_or_else(|| Error::Decode("x402 MCP challenge is not a v2 PaymentRequired object".to_owned()))?;
        let text = serde_json::to_string(required).map_err(|error| Error::Encode(error.to_string()))?;
        Ok(ProtocolMcpPaymentRequired::Result(json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": required.clone(),
            "isError": true,
        })))
    }

    fn payment_required_from_http(
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<Option<X402PaymentRequired>> {
        let header = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("payment-required"))
            .map(|(_, value)| value.as_str())
            .map(|encoded| Self::decode_base64_json(encoded, 64 * 1024, "PAYMENT-REQUIRED"))
            .transpose()?;
        let body = (!body.trim().is_empty())
            .then(|| serde_json::from_str::<Value>(body))
            .transpose()
            .map_err(|error| Error::Decode(format!("x402 challenge body is invalid JSON: {error}")))?;
        let declared_body = body
            .as_ref()
            .map(Self::declared_payment_required)
            .transpose()?
            .flatten();
        let value = match (header, declared_body) {
            (Some(left), Some(right)) if left != right => {
                return Err(Error::Decode(
                    "x402 PAYMENT-REQUIRED header conflicts with challenge body".to_owned(),
                ))
            }
            (Some(value), _) | (_, Some(value)) => value,
            (None, None) => return Ok(None),
        };
        serde_json::from_value(value)
            .map(Some)
            .map_err(|error| Error::Decode(format!("x402 PaymentRequired is not v2: {error}")))
    }
}

impl PaymentProtocol for X402Adapter {
    fn name(&self) -> &'static str {
        "x402"
    }

    fn credential_header_names(&self) -> &'static [&'static str] {
        &["payment-signature"]
    }

    fn merchant_evidence_header_names(&self) -> &'static [&'static str] {
        &["payment-response"]
    }

    fn credential(&self, credential: &PaymentCredential) -> Result<HttpCredential> {
        let delivery = credential
            .http
            .as_ref()
            .ok_or_else(|| Error::UnsafeRetry("x402 credential has no PAYMENT-SIGNATURE delivery".to_owned()))?;
        if credential.protocol != self.name() || !delivery.header_name.eq_ignore_ascii_case("payment-signature") {
            return Err(Error::UnsafeRetry(
                "x402 credential has no PAYMENT-SIGNATURE delivery".to_owned(),
            ));
        }
        let value = Self::decode_base64_json(&delivery.header_value, 64 * 1024, "PAYMENT-SIGNATURE")
            .map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        Self::payment_payload(value, "PAYMENT-SIGNATURE").map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        Ok(delivery.clone())
    }

    fn merchant_evidence(&self, headers: &BTreeMap<String, String>) -> Result<Option<MerchantEvidence>> {
        let Some(value) = self.header(headers, "payment-response") else {
            return Ok(None);
        };
        let decoded = Self::decode_base64_json(value, 64 * 1024, "PAYMENT-RESPONSE")?;
        serde_json::from_value::<x402_types::proto::v2::SettleResponse>(decoded)
            .map_err(|error| Error::InvalidArgument(format!("PAYMENT-RESPONSE is invalid: {error}")))?;
        Ok(Some(MerchantEvidence {
            header_name: "payment-response".to_owned(),
            header_value: value.to_owned(),
        }))
    }

    fn presented_credential(&self, headers: &BTreeMap<String, String>, limit: usize) -> Result<Option<Value>> {
        let Some(encoded) = self.header(headers, "payment-signature") else {
            return Ok(None);
        };
        let payload = Self::decode_base64_json(encoded, limit, "PAYMENT-SIGNATURE")?;
        Ok(Some(Self::payment_payload(payload, "PAYMENT-SIGNATURE")?))
    }

    fn challenge(&self, challenge: &PaymentChallenge) -> Result<ProtocolHttpResponse> {
        let required = challenge
            .payload
            .get("payment_required")
            .filter(|_| challenge.protocol == self.name())
            .ok_or_else(|| Error::Decode("x402 challenge has no payment_required wire value".to_owned()))?;
        Self::declared_payment_required(required)?
            .ok_or_else(|| Error::Decode("x402 challenge is not an official v2 PaymentRequired value".to_owned()))?;
        let body = serde_json::to_vec(required).map_err(|error| Error::Encode(error.to_string()))?;
        Ok(ProtocolHttpResponse {
            headers: BTreeMap::from([("payment-required".to_owned(), Base64Bytes::encode(&body).to_string())]),
            content_type: Some("application/json".to_owned()),
            body,
        })
    }

    fn receipt(&self, payment: &SettledIncomingPayment) -> Result<ProtocolHttpResponse> {
        let body =
            serde_json::to_vec(&Self::settlement_value(payment)?).map_err(|error| Error::Encode(error.to_string()))?;
        Ok(ProtocolHttpResponse {
            headers: BTreeMap::from([("payment-response".to_owned(), Base64Bytes::encode(body).to_string())]),
            content_type: None,
            body: Vec::new(),
        })
    }

    fn challenge_expires_at(&self, headers: &BTreeMap<String, String>, body: &str) -> Result<Option<String>> {
        let Some(required) = Self::payment_required_from_http(headers, body)? else {
            return Ok(None);
        };
        let timeout = required
            .accepts
            .iter()
            .map(|requirements| requirements.max_timeout_seconds)
            .filter(|timeout| *timeout > 0)
            .min();
        let Some(timeout) = timeout else {
            return Ok(None);
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Decode("system time precedes Unix epoch".to_owned()))?
            .as_secs();
        Ok(now.checked_add(timeout).map(|value| value.to_string()))
    }

    fn mcp_challenge(&self, message: &Value, limit: usize) -> Result<Option<ProtocolHttpResponse>> {
        let Some(required) = Self::payment_required_from_message(message)? else {
            return Ok(None);
        };
        let body = Self::bounded_json(&required, limit, "MCP x402 PaymentRequired")?;
        Ok(Some(ProtocolHttpResponse {
            headers: BTreeMap::from([("payment-required".to_owned(), Base64Bytes::encode(&body).to_string())]),
            content_type: Some("application/json".to_owned()),
            body,
        }))
    }

    fn mcp_credential(&self, credential: &PaymentCredential) -> Result<ProtocolMcpMetadata> {
        let delivery = self.credential(credential)?;
        let value = Self::decode_base64_json(&delivery.header_value, 64 * 1024, "PAYMENT-SIGNATURE")
            .map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        let value =
            Self::payment_payload(value, "PAYMENT-SIGNATURE").map_err(|error| Error::UnsafeRetry(error.to_string()))?;
        Ok(ProtocolMcpMetadata {
            key: MCP_PAYMENT_KEY,
            value,
        })
    }

    fn mcp_request_metadata_key(&self) -> Option<&'static str> {
        Some(MCP_PAYMENT_KEY)
    }

    fn presented_mcp_credential(&self, meta: &Value, limit: usize) -> Result<Option<Value>> {
        let Some(value) = meta.get(MCP_PAYMENT_KEY) else {
            return Ok(None);
        };
        Self::bounded_json(value, limit, "MCP x402 payment metadata")?;
        Ok(Some(Self::payment_payload(value.clone(), "MCP x402 payment metadata")?))
    }

    fn mcp_payment_required(&self, challenge: &PaymentChallenge) -> Result<ProtocolMcpPaymentRequired> {
        let required = challenge
            .payload
            .get("payment_required")
            .filter(|_| challenge.protocol == self.name())
            .ok_or_else(|| Error::Decode("x402 challenge has no payment_required wire value".to_owned()))?
            .clone();
        Self::mcp_payment_result(&required)
    }

    fn mcp_payment_rejected(&self, challenge: &PaymentChallenge, message: &str) -> Result<ProtocolMcpPaymentRequired> {
        let value = challenge
            .payload
            .get("payment_required")
            .filter(|_| challenge.protocol == self.name())
            .ok_or_else(|| Error::Decode("x402 challenge has no payment_required wire value".to_owned()))?;
        let mut required = serde_json::from_value::<X402PaymentRequired>(value.clone())
            .map_err(|error| Error::Decode(format!("x402 PaymentRequired is not v2: {error}")))?;
        required.error = Some(message.to_owned());
        let required = serde_json::to_value(required).map_err(|error| Error::Encode(error.to_string()))?;
        Self::mcp_payment_result(&required)
    }

    fn mcp_receipt(
        &self,
        _challenge: &PaymentChallenge,
        payment: &SettledIncomingPayment,
    ) -> Result<ProtocolMcpMetadata> {
        Ok(ProtocolMcpMetadata {
            key: MCP_PAYMENT_RESPONSE_KEY,
            value: Self::settlement_value(payment)?,
        })
    }

    fn mcp_receipt_metadata_key(&self) -> Option<&'static str> {
        Some(MCP_PAYMENT_RESPONSE_KEY)
    }

    fn mcp_merchant_evidence(&self, meta: &Value, limit: usize) -> Result<Option<MerchantEvidence>> {
        let Some(value) = meta.get(MCP_PAYMENT_RESPONSE_KEY) else {
            return Ok(None);
        };
        serde_json::from_value::<x402_types::proto::v2::SettleResponse>(value.clone())
            .map_err(|error| Error::InvalidArgument(format!("MCP x402 settlement is invalid: {error}")))?;
        let encoded = Self::bounded_json(value, limit, "MCP x402 payment response metadata")?;
        Ok(Some(MerchantEvidence {
            header_name: "payment-response".to_owned(),
            header_value: Base64Bytes::encode(encoded).to_string(),
        }))
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/x402_test.rs"]
mod tests;
