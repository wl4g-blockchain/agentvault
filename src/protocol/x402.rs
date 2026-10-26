//! x402 v2 boundary for EVM EIP-3009 and Solana SPL `exact` payments.
//!
//! This module validates and translates the v2 wire DTO. EIP-3009 signing and
//! cryptographic verification are delegated to the signer and an external
//! facilitator; `AgentVault` does not duplicate a facilitator implementation.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{eip712_domain, SolStruct as _};
use async_trait::async_trait;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use rand_core::{OsRng, RngCore as _};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use sha3::Keccak256;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use solana_signature::Signature as SolanaSignature;
use solana_signer::{Signer as SolanaSigner, SignerError};
use solana_transaction::versioned::VersionedTransaction;
use x402_chain_eip155::{
    chain::{AssetTransferMethod, ChecksummedAddress, DecimalU256, Eip155ChainReference},
    v1_eip155_exact::{
        ExactEvmPayload as Eip3009Payload, ExactEvmPayloadAuthorization, ExactScheme, TransferWithAuthorization,
    },
    v2_eip155_exact::{
        ExactEvmPayload, PaymentPayload as OfficialEvmPaymentPayload, PaymentRequirements as EvmPaymentRequirements,
    },
};
use x402_chain_solana::{
    chain::{Address as SolanaAddress, SolanaChainReference},
    v1_solana_exact::{
        client::build_signed_transfer_transaction,
        types::{ExactSolanaPayload, SupportedPaymentKindExtra, TransactionInt},
    },
    v2_solana_exact::{
        PaymentPayload as OfficialSolanaPaymentPayload, PaymentRequirements as SolanaPaymentRequirements,
    },
};
use x402_types::{
    chain::ChainId,
    proto::v2::{
        PaymentRequired as OfficialPaymentRequired, ResourceInfo, SettleResponse, VerifyResponse, X402Version2,
    },
    proto::{util::U64String, OriginalJson},
    timestamp::UnixTimestamp,
    util::b64::Base64Bytes,
};

use crate::{
    model::{
        canonicalize_payment_recipient, canonicalize_wallet_address, payment_recipients_match, Amount, AssetIdentity,
        PaymentChallenge, PaymentCredential, PaymentIntent, PaymentPrice, PaymentReceipt, PaymentStatus,
        EVM_EOA_ALGORITHM, SOLANA_ED25519_ALGORITHM,
    },
    Error, Result,
};

use super::{
    PaymentProtocol, ProtocolHttpCredential, ProtocolMerchantEvidence, ProtocolSettlement, ProtocolSigningRequest,
    ProtocolValidation,
};

#[path = "x402_evm.rs"]
mod evm;
#[path = "x402_solana.rs"]
mod solana;
#[path = "x402_wire.rs"]
mod wire;
pub use evm::X402Evm;
pub(crate) use solana::X402Solana;
pub(crate) use wire::X402Wire;
use wire::{X402ChallengeIntegrity, X402PaymentPayload, X402Requirement};

const X402_PROTOCOL: &str = "x402";
const EXACT_SCHEME: &str = "exact";
const MAX_X402_HTTP_WIRE_BYTES: usize = 64 * 1024;
// Match the official x402 EVM client. RPC "latest" block time can lag wall
// clock time (notably on development chains that mine only on transactions),
// so a one-second backdating window is not sufficient for immediate
// facilitator simulation.
const EIP3009_VALID_AFTER_SKEW_SECONDS: u64 = 10 * 60;

type X402PaymentRequired = OfficialPaymentRequired<OriginalJson>;
type EvmPaymentPayload = OfficialEvmPaymentPayload<EvmPaymentRequirements>;
type SolanaPaymentPayload = OfficialSolanaPaymentPayload<SolanaPaymentRequirements>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X402Asset {
    pub network: String,
    pub contract: String,
    pub symbol: String,
    pub decimals: u8,
}

/// Operator-owned Solana data required by the official x402 client builder.
/// A merchant challenge can select only the matching configured network; it
/// can never supply an RPC endpoint.
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

struct X402SolanaRuntime {
    fee_payer: SolanaAddress,
    rpc: Arc<RpcClient>,
}

/// Registry-backed adapter. Asset decimals and token identity are local trusted
/// configuration, never values accepted from a merchant challenge.
pub struct X402Adapter {
    assets: Vec<X402Asset>,
    max_timeout_seconds: u64,
    solana: BTreeMap<String, X402SolanaRuntime>,
}

impl X402Adapter {
    /// Creates an EVM/USDC/exact adapter. Solana assets require
    /// [`Self::new_with_solana`] so their fixed RPC and facilitator fee payer
    /// cannot be inferred from untrusted challenge data.
    ///
    /// # Errors
    ///
    /// Rejects ambiguous assets and unsafe timeout bounds.
    pub fn new(assets: Vec<X402Asset>, max_timeout_seconds: u64) -> Result<Self> {
        Self::new_with_solana(assets, max_timeout_seconds, Vec::new())
    }

    /// Creates the x402 v2 adapter with operator-owned Solana runtime bindings.
    ///
    /// # Errors
    ///
    /// Rejects malformed/duplicate assets, missing or extra Solana bindings,
    /// unsafe limits, and a fee payer that is not a Solana address.
    pub fn new_with_solana(
        assets: Vec<X402Asset>,
        max_timeout_seconds: u64,
        solana: Vec<X402SolanaConfig>,
    ) -> Result<Self> {
        if assets.is_empty() || max_timeout_seconds == 0 || max_timeout_seconds > 3_600 {
            return Err(Error::Config("x402 asset registry or timeout is invalid".to_owned()));
        }
        for (index, asset) in assets.iter().enumerate() {
            let valid_asset = if X402Evm::valid_network(&asset.network) {
                X402Evm::valid_address(&asset.contract)
            } else if X402Solana::valid_network(&asset.network) {
                asset.contract.parse::<SolanaAddress>().is_ok()
            } else {
                false
            };
            if !valid_asset
                || !asset.symbol.eq_ignore_ascii_case("USDC")
                || asset.decimals != 6
                || assets[..index].iter().any(|prior| {
                    prior.network == asset.network
                        && payment_recipients_match(&asset.network, &prior.contract, &asset.contract)
                })
            {
                return Err(Error::Config(
                    "x402 v2 assets must be unique registered EVM or Solana USDC deployments".to_owned(),
                ));
            }
        }
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
            let rpc = crate::utils::solana_rpc::SolanaRpcClient::build(
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
        Ok(Self {
            assets,
            max_timeout_seconds,
            solana: solana_runtime,
        })
    }
}

#[async_trait]
impl PaymentProtocol for X402Adapter {
    fn name(&self) -> &'static str {
        X402_PROTOCOL
    }

    fn decode_http_challenge(
        &self,
        resource: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<Option<Value>> {
        let from_header = Self::header_value(headers, "payment-required")
            .map(Self::decode_payment_required_header)
            .transpose()?;
        let from_body = Self::decode_payment_required_body(body)?;
        let wire = match (from_header, from_body) {
            (Some(header), Some(body)) if header != body => {
                return Err(Error::PaymentConflict(
                    "x402 PAYMENT-REQUIRED header conflicts with the 402 body".to_owned(),
                ))
            }
            (Some(header), _) => Some(header),
            (None, body) => body,
        };
        if let Some(wire) = wire.as_ref() {
            self.decode_challenge_wire(resource, wire, None)?;
        }
        Ok(wire)
    }

    fn encode_http_credential(&self, credential: &PaymentCredential) -> Result<ProtocolHttpCredential> {
        if credential.protocol != X402_PROTOCOL {
            return Err(Error::PaymentProtocol("x402 credential protocol mismatch".to_owned()));
        }
        Ok(ProtocolHttpCredential {
            header_name: "payment-signature".to_owned(),
            header_value: Base64Bytes::encode(serde_json::to_vec(&credential.payload)?).to_string(),
        })
    }

    fn merchant_evidence_header(&self) -> &'static str {
        "payment-response"
    }

    fn decode_merchant_evidence(&self, encoded: &str) -> Result<ProtocolMerchantEvidence> {
        if encoded.is_empty()
            || encoded.len() > MAX_X402_HTTP_WIRE_BYTES
            || !encoded.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(Error::InvalidRequest(
                "x402 PAYMENT-RESPONSE header is invalid".to_owned(),
            ));
        }
        let decoded = Base64Bytes::from(encoded.as_bytes())
            .decode()
            .map_err(|_| Error::PaymentProtocol("x402 PAYMENT-RESPONSE is not valid base64".to_owned()))?;
        if decoded.is_empty() || decoded.len() > MAX_X402_HTTP_WIRE_BYTES {
            return Err(Error::InvalidRequest(
                "x402 PAYMENT-RESPONSE payload is too large".to_owned(),
            ));
        }
        match X402Wire::parse_settle_response(&decoded)? {
            SettleResponse::Success {
                payer,
                transaction,
                network,
            } => Ok(ProtocolMerchantEvidence {
                accepted: true,
                transaction: Some(transaction),
                network: Some(network),
                payer: Some(payer),
            }),
            SettleResponse::Error { .. } => Ok(ProtocolMerchantEvidence {
                accepted: false,
                transaction: None,
                network: None,
                payer: None,
            }),
        }
    }

    fn credential_payer(&self, challenge: &PaymentChallenge, credential: &PaymentCredential) -> Result<String> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != X402_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol("x402 credential binding mismatch".to_owned()));
        }
        let payload = X402Wire::decode_payment_payload(&credential.payload, &integrity)?;
        X402Wire::validate_payload(challenge, &integrity, &payload, None)?;
        canonicalize_payment_recipient(&challenge.asset.network, &payload.payer()?)
            .map_err(|_| Error::PaymentProtocol("x402 credential payer is invalid".to_owned()))
    }

    async fn parse_challenge(
        &self,
        scenario_id: &str,
        expected_resource: &str,
        wire: &Value,
        now: i64,
        advertised_expires_at: Option<i64>,
    ) -> Result<PaymentChallenge> {
        let integrity = self.decode_challenge_wire(expected_resource, wire, None)?;
        let timeout = i64::try_from(integrity.selected.max_timeout_seconds())
            .map_err(|_| Error::PaymentProtocol("x402 timeout overflows".to_owned()))?;
        let expires_at = advertised_expires_at
            .ok_or_else(|| Error::PaymentProtocol("x402 response is missing payment-expires-at".to_owned()))?;
        if expires_at <= now || expires_at > now.saturating_add(timeout) {
            return Err(Error::PaymentProtocol(
                "x402 payment-expires-at is outside the advertised timeout window".to_owned(),
            ));
        }
        let nonce = X402Wire::deterministic_nonce(expected_resource, expires_at, wire)?;
        let challenge_hash = X402Wire::challenge_hash(expected_resource, expires_at, wire, &nonce)?;
        let challenge = PaymentChallenge {
            scenario_id: scenario_id.to_owned(),
            price_id: None,
            resource: expected_resource.to_owned(),
            protocol: X402_PROTOCOL.to_owned(),
            asset: integrity.asset,
            amount: integrity.amount,
            recipient: integrity.recipient,
            nonce,
            challenge_hash,
            expires_at,
            payload: json!({
                "payment_required": wire,
                "selected": integrity.selected_wire,
            }),
        };
        self.validate_challenge_integrity(&challenge)?;
        Ok(challenge)
    }

    async fn validate_payment(&self, challenge: &PaymentChallenge, intent: &PaymentIntent) -> Result<()> {
        self.validate_challenge_integrity(challenge)?;
        if challenge.scenario_id != intent.scenario_id
            || challenge.resource != intent.resource
            || challenge.asset != intent.asset
            || challenge.amount != intent.amount
            || !payment_recipients_match(&intent.asset.network, &challenge.recipient, &intent.recipient)
            || intent.challenge_hash.as_deref() != Some(&challenge.challenge_hash)
        {
            return Err(Error::PaymentProtocol(
                "x402 challenge does not match the payment intent".to_owned(),
            ));
        }
        Ok(())
    }

    async fn build_credential(&self, challenge: &PaymentChallenge, signed_payload: Value) -> Result<PaymentCredential> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        let payload = X402Wire::decode_payment_payload(&signed_payload, &integrity)?;
        X402Wire::validate_payload(challenge, &integrity, &payload, None)?;
        let mut credential = PaymentCredential {
            protocol: X402_PROTOCOL.to_owned(),
            challenge_hash: challenge.challenge_hash.clone(),
            // Assigned below from the protocol-semantic identity. This value
            // is persisted under a global uniqueness constraint and therefore
            // must not depend on JSON member order, hex case, or equivalent
            // signature encodings.
            credential_hash: String::new(),
            payload: signed_payload,
        };
        credential.credential_hash = self.credential_fingerprint(challenge, &credential)?;
        Ok(credential)
    }

    async fn prepare_signing(
        &self,
        challenge: &PaymentChallenge,
        payer: &str,
        now: i64,
    ) -> Result<ProtocolSigningRequest> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if now < 0 || now >= challenge.expires_at {
            return Err(Error::PaymentProtocol(
                "x402 challenge expired before signing".to_owned(),
            ));
        }
        match &integrity.selected {
            X402Requirement::Evm(requirement) => {
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
                let signing_time = u64::try_from(now)
                    .map_err(|_| Error::PaymentProtocol("x402 signing time is invalid".to_owned()))?;
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
            X402Requirement::Solana(requirement) => self.prepare_solana_signing(&integrity, requirement, payer).await,
        }
    }

    async fn complete_signing(
        &self,
        challenge: &PaymentChallenge,
        mut request: ProtocolSigningRequest,
        signature: &[u8],
    ) -> Result<PaymentCredential> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        match &integrity.selected {
            X402Requirement::Evm(_) => {
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
            }
            X402Requirement::Solana(_) => {
                request.payload = X402Solana::complete_signing(&request, signature)?;
            }
        }
        self.build_credential(challenge, request.payload).await
    }

    async fn verify_receipt(&self, intent: &PaymentIntent, receipt: &PaymentReceipt) -> Result<()> {
        if intent.protocol != X402_PROTOCOL
            || receipt.protocol != X402_PROTOCOL
            || receipt.intent_id != intent.id
            || receipt.scenario_id != intent.scenario_id
            || receipt.asset != intent.asset
            || !payment_recipients_match(&intent.asset.network, &receipt.recipient, &intent.recipient)
            || receipt.requested_amount != intent.amount
            || receipt.status != PaymentStatus::Settled
        {
            return Err(Error::PaymentProtocol("x402 receipt binding mismatch".to_owned()));
        }
        receipt
            .validate()
            .map_err(|field| Error::PaymentProtocol(format!("invalid x402 receipt field: {field}")))
    }

    async fn build_challenge(&self, price: &PaymentPrice, recipient: &str, now: i64) -> Result<PaymentChallenge> {
        if !price.supports_protocol(X402_PROTOCOL) {
            return Err(Error::PaymentProtocol(
                "price cannot produce an x402 challenge".to_owned(),
            ));
        }
        let recipient = canonicalize_payment_recipient(&price.asset.network, recipient)
            .map_err(|_| Error::PaymentProtocol("price recipient is invalid for its network".to_owned()))?;
        X402Wire::validate_resource(&price.resource)?;
        let contract = price
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("x402 asset requires a token contract".to_owned()))?;
        let registered = self.registered_asset(&price.asset.network, contract)?;
        if registered != price.asset {
            return Err(Error::PaymentProtocol(
                "price asset is not the registered x402 asset".to_owned(),
            ));
        }
        let requirement = if X402Evm::valid_network(&price.asset.network) {
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
                max_timeout_seconds: self.max_timeout_seconds,
                extra: AssetTransferMethod::Eip3009 {
                    name: "USDC".to_owned(),
                    version: "2".to_owned(),
                },
            })
        } else {
            let runtime = self
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
                max_timeout_seconds: self.max_timeout_seconds,
                extra: SupportedPaymentKindExtra {
                    fee_payer: runtime.fee_payer.clone(),
                },
            })
        };
        let expires_at = now.saturating_add(i64::try_from(self.max_timeout_seconds).unwrap_or(i64::MAX));
        let selected_wire = requirement.to_value()?;
        let raw = OriginalJson(
            serde_json::value::RawValue::from_string(serde_json::to_string(&selected_wire)?)
                .map_err(|error| Error::PaymentProtocol(format!("encode x402 requirement: {error}")))?,
        );
        let required = X402PaymentRequired {
            x402_version: X402Version2,
            error: None,
            resource: Some(ResourceInfo {
                url: price.resource.clone(),
                description: None,
                mime_type: None,
            }),
            accepts: vec![raw],
        };
        let wire = serde_json::to_value(&required)?;
        let (nonce, challenge_hash) = X402Wire::server_identity(&price.resource, expires_at, &wire)?;
        let challenge = PaymentChallenge {
            scenario_id: price.scenario_id.clone(),
            price_id: Some(price.id.clone()),
            resource: price.resource.clone(),
            protocol: X402_PROTOCOL.to_owned(),
            asset: price.asset.clone(),
            amount: price.amount,
            recipient,
            nonce,
            challenge_hash,
            expires_at,
            payload: json!({"payment_required": wire, "selected": selected_wire}),
        };
        self.validate_challenge_integrity(&challenge)?;
        Ok(challenge)
    }

    async fn verify_credential(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
        now: i64,
    ) -> Result<ProtocolValidation> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != X402_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol(
                "x402 credential is bound to another challenge".to_owned(),
            ));
        }
        if now >= challenge.expires_at {
            return Err(Error::PaymentProtocol("x402 challenge expired".to_owned()));
        }
        let payload = X402Wire::decode_payment_payload(&credential.payload, &integrity)?;
        X402Wire::validate_payload(challenge, &integrity, &payload, Some(now))?;
        Ok(ProtocolValidation {
            payer: Some(payload.payer()?),
            requires_external_settlement: true,
        })
    }

    fn credential_fingerprint(&self, challenge: &PaymentChallenge, credential: &PaymentCredential) -> Result<String> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != X402_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol(
                "x402 credential fingerprint binding mismatch".to_owned(),
            ));
        }
        let payload = X402Wire::decode_payment_payload(&credential.payload, &integrity)?;
        X402Wire::validate_payload(challenge, &integrity, &payload, None)?;
        let contract = challenge
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("x402 asset has no token contract".to_owned()))?;
        let contract = canonicalize_payment_recipient(&challenge.asset.network, contract)
            .map_err(|_| Error::PaymentProtocol("x402 asset contract is invalid".to_owned()))?;
        match payload {
            X402PaymentPayload::Evm(payload) => {
                let eip3009 = X402Evm::authorization_payload(&payload)?;
                let payer = eip3009.authorization.from.to_string().to_ascii_lowercase();
                let nonce = eip3009.authorization.nonce.as_slice();
                Ok(X402Wire::hash_values(&[
                    b"agentvault.x402.replay.v1",
                    challenge.asset.network.as_bytes(),
                    contract.as_bytes(),
                    payer.as_bytes(),
                    nonce,
                ]))
            }
            X402PaymentPayload::Solana(payload) => {
                let transaction = X402Solana::decode_transaction(&payload.payload.transaction)?;
                let payer = X402Solana::payer(&transaction, &payload.accepted)?.to_string();
                let message = transaction.message.serialize();
                Ok(X402Wire::hash_values(&[
                    b"agentvault.x402.solana.replay.v1",
                    challenge.asset.network.as_bytes(),
                    contract.as_bytes(),
                    payer.as_bytes(),
                    &message,
                ]))
            }
        }
    }

    async fn settle_payment(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
    ) -> Result<ProtocolSettlement> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != X402_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol("x402 credential binding mismatch".to_owned()));
        }
        let payload = X402Wire::decode_payment_payload(&credential.payload, &integrity)?;
        X402Wire::validate_payload(challenge, &integrity, &payload, None)?;
        Ok(ProtocolSettlement {
            protocol: X402_PROTOCOL.to_owned(),
            payment_payload: credential.payload.clone(),
            payment_requirements: integrity.selected_wire,
        })
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/x402_test.rs"]
mod tests;
