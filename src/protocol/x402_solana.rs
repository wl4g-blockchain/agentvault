//! Official Solana transaction projection and deferred Vault signer bridge.
use super::*;

pub(crate) struct X402Solana;

impl X402Adapter {
    pub(super) async fn prepare_solana_signing(
        &self,
        integrity: &X402ChallengeIntegrity,
        requirement: &SolanaPaymentRequirements,
        payer: &str,
    ) -> Result<ProtocolSigningRequest> {
        let payer = canonicalize_wallet_address(SOLANA_ED25519_ALGORITHM, payer)
            .map_err(|_| Error::PaymentProtocol("x402 payer is not a valid Solana address".to_owned()))?;
        let payer = payer
            .parse::<Pubkey>()
            .map_err(|_| Error::PaymentProtocol("x402 payer is not a valid Solana address".to_owned()))?;
        let network = requirement.network.to_string();
        let runtime = self
            .solana
            .get(&network)
            .ok_or_else(|| Error::PaymentProtocol("x402 Solana runtime is not configured".to_owned()))?;
        if runtime.fee_payer != requirement.extra.fee_payer {
            return Err(Error::PaymentProtocol(
                "x402 Solana fee payer binding changed before signing".to_owned(),
            ));
        }
        let signer = DeferredSolanaSigner::new(payer);
        let fee_payer: Pubkey = runtime.fee_payer.clone().into();
        let transaction = build_signed_transfer_transaction(
            &signer,
            &runtime.rpc,
            &fee_payer,
            &requirement.pay_to,
            &requirement.asset,
            requirement.amount.inner(),
        )
        .await
        .map_err(|error| Error::PaymentProtocol(format!("build official x402 Solana transaction: {error}")))?;
        let digest = signer.message()?;
        let decoded = X402Solana::decode_transaction(&transaction)?;
        if decoded.message.serialize() != digest {
            return Err(Error::PaymentProtocol(
                "official x402 Solana builder signed unexpected message bytes".to_owned(),
            ));
        }
        X402Solana::unsigned_payer(&decoded, requirement, payer)?;
        let payload = serde_json::to_value(SolanaPaymentPayload {
            accepted: requirement.clone(),
            payload: ExactSolanaPayload { transaction },
            resource: integrity.payment_required.resource.clone(),
            x402_version: X402Version2,
            extensions: None,
        })?;
        Ok(ProtocolSigningRequest { digest, payload })
    }
}

#[derive(Clone)]
pub(super) struct DeferredSolanaSigner {
    pub(super) pubkey: Pubkey,
    pub(super) message: Arc<Mutex<Option<Vec<u8>>>>,
}

impl DeferredSolanaSigner {
    pub(super) fn new(pubkey: Pubkey) -> Self {
        Self {
            pubkey,
            message: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn message(&self) -> Result<Vec<u8>> {
        self.message
            .lock()
            .map_err(|_| Error::Crypto("Solana signing capture lock is poisoned".to_owned()))?
            .clone()
            .ok_or_else(|| {
                Error::PaymentProtocol("official x402 Solana builder did not request a signature".to_owned())
            })
    }
}

impl SolanaSigner for DeferredSolanaSigner {
    fn try_pubkey(&self) -> std::result::Result<Pubkey, SignerError> {
        Ok(self.pubkey)
    }

    fn try_sign_message(&self, message: &[u8]) -> std::result::Result<SolanaSignature, SignerError> {
        let mut captured = self
            .message
            .lock()
            .map_err(|_| SignerError::Custom("Solana signing capture lock is poisoned".to_owned()))?;
        if captured.as_deref().is_some_and(|prior| prior != message) {
            return Err(SignerError::Custom(
                "official x402 Solana builder requested multiple messages".to_owned(),
            ));
        }
        *captured = Some(message.to_vec());
        Ok(SolanaSignature::default())
    }

    fn is_interactive(&self) -> bool {
        false
    }
}

pub(super) struct FixedSolanaSigner {
    pub(super) pubkey: Pubkey,
    pub(super) message: Vec<u8>,
    pub(super) signature: SolanaSignature,
}

impl SolanaSigner for FixedSolanaSigner {
    fn try_pubkey(&self) -> std::result::Result<Pubkey, SignerError> {
        Ok(self.pubkey)
    }

    fn try_sign_message(&self, message: &[u8]) -> std::result::Result<SolanaSignature, SignerError> {
        if message != self.message || !self.signature.verify(self.pubkey.as_ref(), message) {
            return Err(SignerError::Custom(
                "Solana signature does not match the intent-bound transaction message".to_owned(),
            ));
        }
        Ok(self.signature)
    }

    fn is_interactive(&self) -> bool {
        false
    }
}

impl X402Solana {
    pub(super) fn complete_signing(request: &ProtocolSigningRequest, signature: &[u8]) -> Result<Value> {
        let mut payload = X402Solana::decode_payment_payload(&request.payload)?;
        let transaction = X402Solana::decode_transaction(&payload.payload.transaction)?;
        let message = transaction.message.serialize();
        if request.digest != message || signature.len() != 64 {
            return Err(Error::PaymentProtocol(
                "x402 signer returned invalid Solana transaction material".to_owned(),
            ));
        }
        let payer = X402Solana::unsigned_payer_from_requirement(&transaction, &payload.accepted)?;
        let signature: [u8; 64] = signature
            .try_into()
            .map_err(|_| Error::PaymentProtocol("x402 Solana signature is invalid".to_owned()))?;
        let signer = FixedSolanaSigner {
            pubkey: payer,
            message,
            signature: SolanaSignature::from(signature),
        };
        let transaction = TransactionInt::new(transaction)
            .sign_with_keypair(&signer)
            .map_err(|error| Error::PaymentProtocol(format!("complete official x402 Solana transaction: {error}")))?;
        payload.payload.transaction = transaction
            .as_base64()
            .map_err(|error| Error::PaymentProtocol(format!("encode official x402 Solana transaction: {error}")))?;
        let value = serde_json::to_value(payload)?;
        let completed = X402Solana::decode_payment_payload(&value)?;
        X402Solana::payer(
            &X402Solana::decode_transaction(&completed.payload.transaction)?,
            &completed.accepted,
        )?;
        Ok(value)
    }

    pub(super) fn decode_transaction(encoded: &str) -> Result<VersionedTransaction> {
        if encoded.is_empty() || encoded.len() > MAX_X402_HTTP_WIRE_BYTES {
            return Err(Error::PaymentProtocol(
                "x402 Solana transaction encoding is invalid".to_owned(),
            ));
        }
        let bytes = Base64Bytes::from(encoded.as_bytes())
            .decode()
            .map_err(|_| Error::PaymentProtocol("x402 Solana transaction is not valid base64".to_owned()))?;
        bincode::deserialize(&bytes)
            .map_err(|_| Error::PaymentProtocol("x402 Solana transaction is malformed".to_owned()))
    }

    pub(super) fn required_signers(transaction: &VersionedTransaction) -> Result<&[Pubkey]> {
        let required = usize::from(transaction.message.header().num_required_signatures);
        let keys = transaction.message.static_account_keys();
        if required == 0 || required > keys.len() || transaction.signatures.len() != required {
            return Err(Error::PaymentProtocol(
                "x402 Solana transaction signer layout is invalid".to_owned(),
            ));
        }
        Ok(&keys[..required])
    }

    pub(super) fn unsigned_payer(
        transaction: &VersionedTransaction,
        requirement: &SolanaPaymentRequirements,
        expected: Pubkey,
    ) -> Result<Pubkey> {
        let payer = X402Solana::unsigned_payer_from_requirement(transaction, requirement)?;
        if payer != expected {
            return Err(Error::PaymentProtocol(
                "official x402 Solana transaction is bound to a different payer".to_owned(),
            ));
        }
        Ok(payer)
    }

    pub(super) fn unsigned_payer_from_requirement(
        transaction: &VersionedTransaction,
        requirement: &SolanaPaymentRequirements,
    ) -> Result<Pubkey> {
        let signers = X402Solana::required_signers(transaction)?;
        let fee_payer: Pubkey = requirement.extra.fee_payer.clone().into();
        if signers.first() != Some(&fee_payer)
            || signers.len() != 2
            || transaction
                .signatures
                .iter()
                .any(|signature| *signature != SolanaSignature::default())
        {
            return Err(Error::PaymentProtocol(
                "unsigned x402 Solana transaction signer binding is invalid".to_owned(),
            ));
        }
        Ok(signers[1])
    }

    pub(super) fn payer(transaction: &VersionedTransaction, requirement: &SolanaPaymentRequirements) -> Result<Pubkey> {
        let signers = X402Solana::required_signers(transaction)?;
        let fee_payer: Pubkey = requirement.extra.fee_payer.clone().into();
        let message = transaction.message.serialize();
        if signers.first() != Some(&fee_payer)
            || signers.len() != 2
            || transaction.signatures[0] != SolanaSignature::default()
            || transaction.signatures[1] == SolanaSignature::default()
            || !transaction.signatures[1].verify(signers[1].as_ref(), &message)
        {
            return Err(Error::PaymentProtocol(
                "x402 Solana payer signature or fee-payer binding is invalid".to_owned(),
            ));
        }
        Ok(signers[1])
    }

    /// Returns the payer proven by an official x402 v2 Solana payload.
    ///
    /// This intentionally validates only the signed transaction envelope and its
    /// fee-payer layout. The official facilitator remains authoritative for SPL
    /// instruction simulation and transfer verification.
    pub(crate) fn payment_payer(payload: &Value) -> Result<String> {
        let payload = X402Solana::decode_payment_payload(payload)?;
        X402Solana::payer(
            &X402Solana::decode_transaction(&payload.payload.transaction)?,
            &payload.accepted,
        )
        .map(|payer| payer.to_string())
    }

    pub(super) fn decode_payment_payload(value: &Value) -> Result<SolanaPaymentPayload> {
        X402Solana::validate_payment_payload_wire(value)?;
        serde_json::from_value(value.clone())
            .map_err(|error| Error::PaymentProtocol(format!("invalid x402 Solana payment payload: {error}")))
    }

    /// Validates the exact official v2 Solana payload shape before typed
    /// deserialization. The actual requirement and transaction semantics remain
    /// delegated to the official x402 Solana types and verifier.
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
        X402Wire::exact_object_fields(extra, &["feePayer"], &["feePayer"], "paymentPayload.accepted.extra")?;
        let payload = value
            .get("payload")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload payload is missing".to_owned()))?;
        X402Wire::exact_object_fields(payload, &["transaction"], &["transaction"], "paymentPayload.payload")?;
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
            .parse::<ChainId>()
            .ok()
            .and_then(|chain| SolanaChainReference::try_from(chain).ok())
            .is_some()
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/x402_solana_test.rs"]
mod tests;
