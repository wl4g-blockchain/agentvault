//! Official Solana transaction projection and deferred Vault signer bridge.
use super::*;

pub struct X402Solana;

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
    ///
    /// # Errors
    /// Returns the validation, cryptographic or storage failure from the requested operation.
    pub fn payment_payer(payload: &Value) -> Result<String> {
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
            &["feePayer"],
            &["feePayer"],
            "paymentPayload.accepted.extra",
        )?;
        let payload = value
            .get("payload")
            .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload payload is missing".to_owned()))?;
        ProtocolWire::exact_object_fields(
            payload,
            "x402",
            &["transaction"],
            &["transaction"],
            "paymentPayload.payload",
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
            .parse::<ChainId>()
            .ok()
            .and_then(|chain| SolanaChainReference::try_from(chain).ok())
            .is_some()
    }
}

#[cfg(test)]
#[path = "../tests/x402_solana_test.rs"]
mod tests;

impl X402Solana {
    pub(super) fn requirement(
        adapter: &X402Adapter,
        price: &PaymentPrice,
        contract: &str,
        recipient: &str,
    ) -> Result<X402Requirement> {
        Ok({
            let runtime = adapter
                .solana
                .get(&price.asset.network)
                .ok_or_else(|| Error::PaymentProtocol("x402 Solana runtime is not configured".to_owned()))?;
            let amount = u64::try_from(price.amount.value())
                .map_err(|_| Error::PaymentProtocol("x402 Solana amount exceeds u64".to_owned()))?;
            X402Requirement::Solana(SolanaPaymentRequirements {
                scheme: x402_chain_solana::v2_solana_exact::ExactScheme,
                network: price
                    .asset
                    .network
                    .parse()
                    .map_err(|_| Error::PaymentProtocol("x402 network is not a valid CAIP-2 ID".to_owned()))?,
                amount: U64String::from(amount),
                asset: contract
                    .parse::<SolanaAddress>()
                    .map_err(|_| Error::PaymentProtocol("x402 token mint is invalid".to_owned()))?,
                pay_to: recipient
                    .parse::<SolanaAddress>()
                    .map_err(|_| Error::PaymentProtocol("x402 recipient is invalid".to_owned()))?,
                max_timeout_seconds: adapter.max_timeout_seconds,
                extra: SupportedPaymentKindExtra {
                    fee_payer: runtime.fee_payer.clone(),
                },
            })
        })
    }
}

impl X402Solana {
    pub(super) fn validate_binding(
        challenge: &PaymentChallenge,
        integrity: &X402ChallengeIntegrity,
        payload: &SolanaPaymentPayload,
        requirement: &SolanaPaymentRequirements,
    ) -> Result<()> {
        let transaction = X402Solana::decode_transaction(&payload.payload.transaction)?;
        let payer = X402Solana::payer(&transaction, &payload.accepted)?;
        if !X402Wire::payload_resource_matches(
            payload.resource.as_ref(),
            integrity.payment_required.resource.as_ref(),
            &challenge.resource,
        ) || payload.accepted != *requirement
            || !payment_recipients_match(
                &challenge.asset.network,
                &payload.accepted.pay_to.to_string(),
                &challenge.recipient,
            )
            || payload.accepted.amount.inner() != u64::try_from(challenge.amount.value()).unwrap_or(u64::MAX)
            || payload.accepted.asset.to_string() != challenge.asset.token_contract.as_deref().unwrap_or_default()
            || payer == *payload.accepted.extra.fee_payer.pubkey()
        {
            return Err(Error::PaymentProtocol("x402 Solana payload binding failed".to_owned()));
        }

        Ok(())
    }
}

impl X402Solana {
    pub(super) fn runtimes(
        assets: &[X402Asset],
        solana: Vec<X402SolanaConfig>,
    ) -> Result<BTreeMap<String, X402SolanaRuntime>> {
        let mut solana_runtime = BTreeMap::new();
        for config in solana {
            if !X402Solana::valid_network(&config.network)
                || !assets.iter().any(|asset| asset.network == config.network)
                || config.rpc_timeout.is_zero()
                || config.rpc_max_response_bytes == 0
            {
                return Err(Error::Config("x402 Solana runtime binding is invalid".to_owned()));
            }
            let fee_payer = config
                .fee_payer
                .parse::<SolanaAddress>()
                .map_err(|_| Error::Config("x402 Solana fee payer is invalid".to_owned()))?;
            let rpc = crate::util::solana_rpc::SolanaRpcClient::build(
                &config.rpc_url,
                config.rpc_timeout,
                config.rpc_max_response_bytes,
                &config.rpc_headers,
                config.allow_insecure_loopback,
            )?;
            let runtime = X402SolanaRuntime {
                fee_payer,
                rpc: Arc::new(rpc),
            };
            if solana_runtime.insert(config.network, runtime).is_some() {
                return Err(Error::Config("duplicate x402 Solana runtime binding".to_owned()));
            }
        }
        if assets
            .iter()
            .filter(|asset| X402Solana::valid_network(&asset.network))
            .any(|asset| !solana_runtime.contains_key(&asset.network))
        {
            return Err(Error::Config(
                "every x402 Solana asset requires a fixed runtime binding".to_owned(),
            ));
        }

        Ok(solana_runtime)
    }
}

impl X402Solana {
    pub(super) fn fingerprint(
        payload: &SolanaPaymentPayload,
        challenge: &PaymentChallenge,
        contract: &str,
    ) -> Result<String> {
        let transaction = X402Solana::decode_transaction(&payload.payload.transaction)?;
        let payer = X402Solana::payer(&transaction, &payload.accepted)?.to_string();
        let message = transaction.message.serialize();
        Ok(ProtocolWire::hash_values(&[
            b"agentvault.x402.solana.replay.v1",
            challenge.asset.network.as_bytes(),
            contract.as_bytes(),
            payer.as_bytes(),
            &message,
        ]))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct X402SolanaConfig {
    pub network: String,
    pub fee_payer: String,
    pub rpc_url: String,
    pub rpc_timeout: Duration,
    pub rpc_max_response_bytes: usize,
    pub rpc_headers: BTreeMap<String, String>,
    pub allow_insecure_loopback: bool,
}

impl fmt::Debug for X402SolanaConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("X402SolanaConfig")
            .field("network", &self.network)
            .field("fee_payer", &self.fee_payer)
            .field("rpc_url", &self.rpc_url)
            .field("rpc_timeout", &self.rpc_timeout)
            .field("rpc_max_response_bytes", &self.rpc_max_response_bytes)
            .field("rpc_header_count", &self.rpc_headers.len())
            .field("allow_insecure_loopback", &self.allow_insecure_loopback)
            .finish()
    }
}

pub(super) struct X402SolanaRuntime {
    pub(super) fee_payer: SolanaAddress,
    pub(super) rpc: Arc<RpcClient>,
}
