//! Official x402 EVM models, EIP-3009 digests and signature validation.
use super::*;

pub struct X402Evm;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eip3009AuthorizationIdentity {
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

    ///
    /// # Errors
    /// Returns a protocol or settlement error for malformed or incorrectly bound wire data.
    pub fn authorization_identity(payload: &Value) -> Result<Eip3009AuthorizationIdentity> {
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
    ///
    /// # Errors
    /// Returns a protocol or settlement error for malformed or incorrectly bound wire data.
    pub fn validate_payment_payload_wire(value: &Value) -> Result<()> {
        ProtocolWire::exact_object_fields(
            value,
            "x402",
            &["x402Version", "accepted", "payload"],
            &["x402Version", "accepted", "payload", "resource", "extensions"],
            "paymentPayload",
        )?;
        let accepted = value
            .get("accepted")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload accepted is missing".to_owned()))?;
        ProtocolWire::exact_object_fields(
            accepted,
            "x402",
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
        ProtocolWire::exact_object_fields(
            extra,
            "x402",
            &["name", "version"],
            &["assetTransferMethod", "name", "version"],
            "paymentPayload.accepted.extra",
        )?;
        let payload = value
            .get("payload")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload payload is missing".to_owned()))?;
        ProtocolWire::exact_object_fields(
            payload,
            "x402",
            &["signature", "authorization"],
            &["signature", "authorization"],
            "paymentPayload.payload",
        )?;
        let authorization = payload
            .get("authorization")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload authorization is missing".to_owned()))?;
        ProtocolWire::exact_object_fields(
            authorization,
            "x402",
            &["from", "to", "value", "validAfter", "validBefore", "nonce"],
            &["from", "to", "value", "validAfter", "validBefore", "nonce"],
            "paymentPayload.payload.authorization",
        )?;
        if let Some(resource) = value.get("resource") {
            ProtocolWire::exact_object_fields(
                resource,
                "x402",
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
#[path = "../tests/x402_evm_test.rs"]
mod tests;

impl X402Evm {
    pub(super) fn prepare_signing(
        challenge: &PaymentChallenge,
        integrity: &X402ChallengeIntegrity,
        requirement: &EvmPaymentRequirements,
        payer: &str,
        now: i64,
    ) -> Result<ProtocolSigningRequest> {
        let payer = canonicalize_wallet_address(EVM_EOA_ALGORITHM, payer)
            .map_err(|_| Error::PaymentProtocol("x402 payer is not a valid EVM address".to_owned()))?;
        let mut nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let nonce = B256::from(nonce);
        let from = payer
            .parse::<Address>()
            .map_err(|_| Error::PaymentProtocol("x402 payer is not a valid EVM address".to_owned()))?;
        let to = challenge
            .recipient
            .parse::<Address>()
            .map_err(|_| Error::PaymentProtocol("x402 recipient is not a valid EVM address".to_owned()))?;
        let signing_time =
            u64::try_from(now).map_err(|_| Error::PaymentProtocol("x402 signing time is invalid".to_owned()))?;
        let valid_after = signing_time.saturating_sub(EIP3009_VALID_AFTER_SKEW_SECONDS);
        let valid_before = u64::try_from(challenge.expires_at)
            .map_err(|_| Error::PaymentProtocol("x402 validBefore is invalid".to_owned()))?;
        let payload = serde_json::to_value(EvmPaymentPayload {
            accepted: requirement.clone(),
            payload: ExactEvmPayload::Eip3009(Eip3009Payload {
                signature: Bytes::from(vec![0_u8; 65]),
                authorization: ExactEvmPayloadAuthorization {
                    from,
                    to,
                    value: U256::from(challenge.amount.value()),
                    valid_after: UnixTimestamp::from_secs(valid_after),
                    valid_before: UnixTimestamp::from_secs(valid_before),
                    nonce,
                },
            }),
            resource: integrity.payment_required.resource.clone(),
            x402_version: X402Version2,
            extensions: None,
        })?;
        let digest = X402Evm::digest(&payload)?.to_vec();
        Ok(ProtocolSigningRequest { digest, payload })
    }
}

impl X402Evm {
    pub(super) fn requirement(
        price: &PaymentPrice,
        contract: &str,
        recipient: &str,
        maximum: u64,
    ) -> Result<X402Requirement> {
        Ok({
            X402Requirement::Evm(EvmPaymentRequirements {
                scheme: ExactScheme,
                network: price
                    .asset
                    .network
                    .parse()
                    .map_err(|_| Error::PaymentProtocol("x402 network is not a valid CAIP-2 ID".to_owned()))?,
                amount: DecimalU256(U256::from(price.amount.value())),
                asset: contract
                    .parse::<ChecksummedAddress>()
                    .map_err(|_| Error::PaymentProtocol("x402 token contract is invalid".to_owned()))?,
                pay_to: recipient
                    .parse::<ChecksummedAddress>()
                    .map_err(|_| Error::PaymentProtocol("x402 recipient is invalid".to_owned()))?,
                max_timeout_seconds: maximum,
                extra: AssetTransferMethod::Eip3009 {
                    name: "USDC".to_owned(),
                    version: "2".to_owned(),
                },
            })
        })
    }
}

impl X402Evm {
    pub(super) fn validate_binding(
        challenge: &PaymentChallenge,
        integrity: &X402ChallengeIntegrity,
        payload: &EvmPaymentPayload,
        requirement: &EvmPaymentRequirements,
        now: Option<i64>,
    ) -> Result<()> {
        let authorization = &X402Evm::authorization_payload(payload)?.authorization;
        let valid_after = i64::try_from(authorization.valid_after.as_secs())
            .map_err(|_| Error::PaymentProtocol("x402 validAfter is invalid".to_owned()))?;
        let valid_before = i64::try_from(authorization.valid_before.as_secs())
            .map_err(|_| Error::PaymentProtocol("x402 validBefore is invalid".to_owned()))?;
        if !X402Wire::payload_resource_matches(
            payload.resource.as_ref(),
            integrity.payment_required.resource.as_ref(),
            &challenge.resource,
        ) || payload.accepted != *requirement
            || !payment_recipients_match(
                &challenge.asset.network,
                &authorization.to.to_string(),
                &challenge.recipient,
            )
            || authorization.value != U256::from(challenge.amount.value())
            || valid_after >= valid_before
            || valid_before > challenge.expires_at
            || !X402Evm::valid_signature(&X402Evm::authorization_payload(payload)?.signature)
            || now.is_some_and(|now| now < valid_after || now >= valid_before)
        {
            return Err(Error::PaymentProtocol(
                "x402 EIP-3009 payload binding failed".to_owned(),
            ));
        }

        Ok(())
    }
}

impl X402Evm {
    pub(super) fn apply_signature(request: &mut ProtocolSigningRequest, signature: &[u8]) -> Result<()> {
        if request.digest.len() != 32 || signature.len() != 65 {
            return Err(Error::PaymentProtocol(
                "x402 signer returned invalid EIP-3009 material".to_owned(),
            ));
        }
        let expected_digest = X402Evm::digest(&request.payload)?;
        if request.digest.as_slice() != expected_digest {
            return Err(Error::PaymentProtocol(
                "x402 signing request digest no longer matches its payload".to_owned(),
            ));
        }
        let mut payload = X402Evm::decode_payment_payload(&request.payload)?;
        X402Evm::authorization_payload_mut(&mut payload)?.signature = Bytes::copy_from_slice(signature);
        request.payload = serde_json::to_value(payload)?;
        let recovered = X402Evm::verify_signature(&request.payload)?;
        if !recovered.eq_ignore_ascii_case(&X402Evm::payer(&request.payload)?) {
            return Err(Error::PaymentProtocol(
                "x402 signature was produced by a different wallet".to_owned(),
            ));
        }

        Ok(())
    }
}

impl X402Evm {
    pub(super) fn fingerprint(
        payload: &EvmPaymentPayload,
        challenge: &PaymentChallenge,
        contract: &str,
    ) -> Result<String> {
        let eip3009 = X402Evm::authorization_payload(payload)?;
        let payer = eip3009.authorization.from.to_string().to_ascii_lowercase();
        let nonce = eip3009.authorization.nonce.as_slice();
        Ok(ProtocolWire::hash_values(&[
            b"agentvault.x402.replay.v1",
            challenge.asset.network.as_bytes(),
            contract.as_bytes(),
            payer.as_bytes(),
            nonce,
        ]))
    }
}

impl X402Evm {
    pub(super) fn amount_from_u256(value: U256) -> Result<Amount> {
        let value = u128::try_from(value)
            .map_err(|_| Error::PaymentProtocol("payment amount exceeds supported precision".to_owned()))?;
        let amount = Amount::new(value);
        if amount.is_zero() {
            return Err(Error::PaymentProtocol(
                "payment amount must be a positive base-unit integer".to_owned(),
            ));
        }
        Ok(amount)
    }
}
