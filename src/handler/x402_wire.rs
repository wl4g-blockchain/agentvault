//! Strict parsing helpers for official x402 facilitator response wires.

use serde_json::Value;
use x402_types::proto::v2::{SettleResponse, VerifyResponse};

use crate::{Error, Result};

pub(crate) fn parse_verify_response(response: &[u8]) -> Result<VerifyResponse> {
    let wire = strict_variant_wire(
        response,
        "isValid",
        &["isValid", "payer"],
        &["isValid", "payer"],
        &["isValid", "invalidReason"],
        &["isValid", "invalidReason", "payer"],
    )?;
    serde_json::from_value(wire).map_err(|error| Error::Settlement(format!("invalid x402 /verify response: {error}")))
}

pub(crate) fn parse_settle_response(response: &[u8]) -> Result<SettleResponse> {
    let wire = strict_variant_wire(
        response,
        "success",
        &["success", "payer", "transaction", "network"],
        &["success", "payer", "transaction", "network"],
        &["success", "errorReason", "network"],
        &["success", "errorReason", "network"],
    )?;
    serde_json::from_value(wire).map_err(|error| Error::Settlement(format!("invalid x402 /settle response: {error}")))
}

fn strict_variant_wire(
    response: &[u8],
    discriminant: &str,
    success_required: &[&str],
    success_allowed: &[&str],
    error_required: &[&str],
    error_allowed: &[&str],
) -> Result<Value> {
    let value: Value = serde_json::from_slice(response)
        .map_err(|error| Error::Settlement(format!("invalid x402 facilitator response: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::Settlement("x402 facilitator response must be an object".to_owned()))?;
    let success = object
        .get(discriminant)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::Settlement(format!("x402 facilitator response has no boolean {discriminant}")))?;
    let (required, allowed) = if success {
        (success_required, success_allowed)
    } else {
        (error_required, error_allowed)
    };
    if required.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !allowed.contains(&field.as_str()))
    {
        return Err(Error::Settlement(
            "x402 facilitator response fields do not match the official wire variant".to_owned(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
#[path = "../../tests/handler/x402_wire_test.rs"]
mod tests;
