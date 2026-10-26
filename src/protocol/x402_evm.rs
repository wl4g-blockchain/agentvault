//! Official x402 EVM models, EIP-3009 digests and signature validation.
use super::*;

pub struct X402Evm;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Eip3009AuthorizationIdentity {
    pub payer: String,
    pub nonce: String,
    pub valid_before: i64,
}

impl X402Evm {
    pub(super) fn authorization_payload(payload: &EvmPaymentPayload) -> Result<&Eip3009Payload> {
        match &payload.payload {
            ExactEvmPayload::Eip3009(payload) => Ok(payload),
            ExactEvmPayload::Permit2(_) => Err(Error::PaymentProtocol(
                "x402 exact currently supports only EIP-3009".to_owned(),
            )),
        }
    }

    pub(super) fn authorization_payload_mut(payload: &mut EvmPaymentPayload) -> Result<&mut Eip3009Payload> {
        match &mut payload.payload {
            ExactEvmPayload::Eip3009(payload) => Ok(payload),
            ExactEvmPayload::Permit2(_) => Err(Error::PaymentProtocol(
                "x402 exact currently supports only EIP-3009".to_owned(),
            )),
        }
    }

    pub(crate) fn authorization_identity(payload: &Value) -> Result<Eip3009AuthorizationIdentity> {
        let payload = X402Evm::decode_payment_payload(payload)?;
        let authorization = &X402Evm::authorization_payload(&payload)?.authorization;
        let valid_before = i64::try_from(authorization.valid_before.as_secs())
            .map_err(|_| Error::PaymentProtocol("x402 validBefore is invalid".to_owned()))?;
        Ok(Eip3009AuthorizationIdentity {
            payer: authorization.from.to_string(),
            nonce: authorization.nonce.to_string(),
            valid_before,
        })
    }

    /// Computes the EIP-712 `TransferWithAuthorization` digest carried by an x402
    /// exact payment payload.
    ///
    /// # Errors
    ///
    /// Rejects malformed, unsupported, or incomplete EIP-3009 payloads.
    pub fn digest(payload: &Value) -> Result<[u8; 32]> {
        let payload = X402Evm::decode_payment_payload(payload)?;
        let requirement = &payload.accepted;
        X402Wire::validate_requirement(&X402Requirement::Evm(requirement.clone()), u64::MAX)?;
        let authorization = &X402Evm::authorization_payload(&payload)?.authorization;
        let chain_id = Eip155ChainReference::try_from(&requirement.network)
            .map_err(|_| Error::PaymentProtocol("x402 EIP-712 chain ID is invalid".to_owned()))?
            .inner();
        let (name, version) = match &requirement.extra {
            AssetTransferMethod::Eip3009 { name, version } => (name.clone(), version.clone()),
            AssetTransferMethod::Permit2 => {
                return Err(Error::PaymentProtocol(
                    "x402 exact currently supports only EIP-3009".to_owned(),
                ))
            }
        };
        let domain = eip712_domain! {
            name: name,
            version: version,
            chain_id: chain_id,
            verifying_contract: requirement.asset.0,
        };
        let transfer = TransferWithAuthorization {
            from: authorization.from,
            to: authorization.to,
            value: authorization.value,
            validAfter: U256::from(authorization.valid_after.as_secs()),
            validBefore: U256::from(authorization.valid_before.as_secs()),
            nonce: authorization.nonce,
        };
        Ok(transfer.eip712_signing_hash(&domain).into())
    }

    /// Recovers and returns the canonical EVM payer address from an x402
    /// EIP-3009 signature.
    ///
    /// # Errors
    ///
    /// Rejects malformed signatures and signatures that cannot recover a key.
    pub fn verify_signature(payload: &Value) -> Result<String> {
        let decoded = X402Evm::decode_payment_payload(payload)?;
        let bytes = X402Evm::authorization_payload(&decoded)?.signature.as_ref();
        if bytes.len() != 65 {
            return Err(Error::PaymentProtocol("x402 EIP-3009 signature is invalid".to_owned()));
        }
        let signature = Signature::try_from(&bytes[..64])
            .map_err(|_| Error::PaymentProtocol("x402 EIP-3009 signature is invalid".to_owned()))?;
        let recovery = match bytes[64] {
            0 | 1 => bytes[64],
            27 | 28 => bytes[64] - 27,
            _ => {
                return Err(Error::PaymentProtocol(
                    "x402 EIP-3009 recovery ID is invalid".to_owned(),
                ))
            }
        };
        let recovery = RecoveryId::try_from(recovery)
            .map_err(|_| Error::PaymentProtocol("x402 EIP-3009 recovery ID is invalid".to_owned()))?;
        let digest = X402Evm::digest(payload)?;
        let key = VerifyingKey::recover_from_prehash(&digest, &signature, recovery)
            .map_err(|_| Error::PaymentProtocol("x402 EIP-3009 signature recovery failed".to_owned()))?;
        let point = key.to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        Ok(format!("0x{}", hex::encode(&hash[12..])))
    }

    pub(super) fn payer(payload: &Value) -> Result<String> {
        let payload = X402Evm::decode_payment_payload(payload)?;
        Ok(X402Evm::authorization_payload(&payload)?.authorization.from.to_string())
    }

    pub(super) fn decode_payment_payload(value: &Value) -> Result<EvmPaymentPayload> {
        X402Evm::validate_payment_payload_wire(value)?;
        serde_json::from_value(value.clone())
            .map_err(|error| Error::PaymentProtocol(format!("invalid x402 payment payload: {error}")))
    }

    /// Validates the exact official v2 EIP-3009 payload shape before typed
    /// deserialization. The upstream official types intentionally tolerate
    /// unknown JSON fields, while a signed payment boundary must fail closed.
    pub(crate) fn validate_payment_payload_wire(value: &Value) -> Result<()> {
        X402Wire::exact_object_fields(
            value,
            &["x402Version", "accepted", "payload"],
            &["x402Version", "accepted", "payload", "resource", "extensions"],
            "paymentPayload",
        )?;
        let accepted = value
            .get("accepted")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload accepted is missing".to_owned()))?;
        X402Wire::exact_object_fields(
            accepted,
            &[
                "scheme",
                "network",
                "amount",
                "asset",
                "payTo",
                "maxTimeoutSeconds",
                "extra",
            ],
            &[
                "scheme",
                "network",
                "amount",
                "asset",
                "payTo",
                "maxTimeoutSeconds",
                "extra",
            ],
            "paymentPayload.accepted",
        )?;
        let extra = accepted
            .get("extra")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload accepted.extra is missing".to_owned()))?;
        X402Wire::exact_object_fields(
            extra,
            &["name", "version"],
            &["assetTransferMethod", "name", "version"],
            "paymentPayload.accepted.extra",
        )?;
        let payload = value
            .get("payload")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload payload is missing".to_owned()))?;
        X402Wire::exact_object_fields(
            payload,
            &["signature", "authorization"],
            &["signature", "authorization"],
            "paymentPayload.payload",
        )?;
        let authorization = payload
            .get("authorization")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload authorization is missing".to_owned()))?;
        X402Wire::exact_object_fields(
            authorization,
            &["from", "to", "value", "validAfter", "validBefore", "nonce"],
            &["from", "to", "value", "validAfter", "validBefore", "nonce"],
            "paymentPayload.payload.authorization",
        )?;
        if let Some(resource) = value.get("resource") {
            X402Wire::exact_object_fields(
                resource,
                &["url"],
                &["url", "description", "mimeType"],
                "paymentPayload.resource",
            )?;
        }
        Ok(())
    }

    pub(super) fn valid_network(network: &str) -> bool {
        network
            .strip_prefix("eip155:")
            .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
    }

    pub(super) fn valid_address(value: &str) -> bool {
        canonicalize_wallet_address(EVM_EOA_ALGORITHM, value).is_ok()
    }

    pub(super) fn valid_signature(value: &Bytes) -> bool {
        value.len() == 65
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/x402_evm_test.rs"]
mod tests;
