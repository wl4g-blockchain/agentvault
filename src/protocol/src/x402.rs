use crate::model::protocol::ProtocolWire;
// x402 v2 boundary for EVM EIP-3009 and Solana SPL `exact` payments.
//
// This module validates and translates the v2 wire DTO. EIP-3009 signing and
// cryptographic verification are delegated to the signer and an external
// facilitator; `AgentVault` does not duplicate a facilitator implementation.

use std::{collections::BTreeMap, fmt, sync::Arc, sync::Mutex, time::Duration};

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{eip712_domain, SolStruct as _};
use async_trait::async_trait;
use k256::{ecdsa::RecoveryId, ecdsa::Signature, ecdsa::VerifyingKey};
use rand_core::{OsRng, RngCore as _};
use serde_json::{json, Value};
use sha2::Digest as _;
use sha3::Keccak256;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_pubkey::Pubkey;
use solana_signature::Signature as SolanaSignature;
use solana_signer::{Signer as SolanaSigner, SignerError};
use solana_transaction::versioned::VersionedTransaction;
use x402_chain_eip155::{
    chain::AssetTransferMethod, chain::ChecksummedAddress, chain::DecimalU256, chain::Eip155ChainReference,
    v1_eip155_exact::ExactEvmPayload as Eip3009Payload, v1_eip155_exact::ExactEvmPayloadAuthorization,
    v1_eip155_exact::ExactScheme, v1_eip155_exact::TransferWithAuthorization, v2_eip155_exact::ExactEvmPayload,
    v2_eip155_exact::PaymentPayload as OfficialEvmPaymentPayload,
    v2_eip155_exact::PaymentRequirements as EvmPaymentRequirements,
};
use x402_chain_solana::{
    chain::Address as SolanaAddress, chain::SolanaChainReference,
    v1_solana_exact::client::build_signed_transfer_transaction, v1_solana_exact::types::ExactSolanaPayload,
    v1_solana_exact::types::SupportedPaymentKindExtra, v1_solana_exact::types::TransactionInt,
    v2_solana_exact::PaymentPayload as OfficialSolanaPaymentPayload,
    v2_solana_exact::PaymentRequirements as SolanaPaymentRequirements,
};
use x402_types::{
    chain::ChainId, proto::util::U64String, proto::v2::PaymentRequired as OfficialPaymentRequired,
    proto::v2::ResourceInfo, proto::v2::SettleResponse, proto::v2::X402Version2, proto::OriginalJson,
    timestamp::UnixTimestamp, util::b64::Base64Bytes,
};

use crate::{
    model::canonicalize_payment_recipient, model::canonicalize_wallet_address, model::payment_recipients_match,
    model::Amount, model::AssetIdentity, model::PaymentChallenge, model::PaymentCredential, model::PaymentIntent,
    model::PaymentPrice, model::PaymentReceipt, model::PaymentStatus, model::EVM_EOA_ALGORITHM,
    model::SOLANA_ED25519_ALGORITHM, Error, Result,
};

use super::{
    PaymentProtocol, ProtocolHttpCredential, ProtocolMerchantEvidence, ProtocolSettlement, ProtocolSigningRequest,
    ProtocolValidation,
};

#[path = "x402_evm.rs"]
mod evm;
#[path = "x402_solana.rs"]
mod solana;
pub use evm::X402Evm;
pub use solana::X402Solana;

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

pub type X402Asset = crate::config::X402AssetConfig;

/// Operator-owned Solana data required by the official x402 client builder.
/// A merchant challenge can select only the matching configured network; it
/// can never supply an RPC endpoint.
pub use solana::X402SolanaConfig;
use solana::X402SolanaRuntime;

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
        let solana_runtime = X402Solana::runtimes(&assets, solana)?;
        Ok(Self {
            assets,
            max_timeout_seconds,
            solana: solana_runtime,
        })
    }

    pub(super) fn registered_asset(&self, network: &str, contract: &str) -> Result<AssetIdentity> {
        let registered = self
            .assets
            .iter()
            .find(|asset| asset.network == network && payment_recipients_match(network, &asset.contract, contract))
            .ok_or_else(|| Error::PaymentProtocol("x402 asset or network is not registered".to_owned()))?;
        Ok(AssetIdentity {
            network: registered.network.clone(),
            asset: registered.symbol.clone(),
            token_contract: Some(registered.contract.clone()),
            decimals: registered.decimals,
        })
    }

    pub(super) fn decode_payment_required_header(encoded: &str) -> Result<Value> {
        if encoded.is_empty()
            || encoded.len() > MAX_X402_HTTP_WIRE_BYTES
            || !encoded.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(Error::PaymentProtocol(
                "x402 PAYMENT-REQUIRED header is invalid".to_owned(),
            ));
        }
        let decoded = Base64Bytes::from(encoded.as_bytes())
            .decode()
            .map_err(|_| Error::PaymentProtocol("x402 PAYMENT-REQUIRED is not valid base64".to_owned()))?;
        if decoded.is_empty() || decoded.len() > MAX_X402_HTTP_WIRE_BYTES {
            return Err(Error::PaymentProtocol(
                "x402 PAYMENT-REQUIRED payload is too large".to_owned(),
            ));
        }
        serde_json::from_slice(&decoded)
            .map_err(|error| Error::PaymentProtocol(format!("invalid x402 PAYMENT-REQUIRED JSON: {error}")))
    }

    pub(super) fn decode_payment_required_body(body: &str) -> Result<Option<Value>> {
        if body.trim().is_empty() {
            return Ok(None);
        }
        if body.len() > MAX_X402_HTTP_WIRE_BYTES {
            return Err(Error::PaymentProtocol("x402 402 body is too large".to_owned()));
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Ok(None);
        };
        let declared = value.as_object().is_some_and(|object| {
            object.contains_key("x402Version") && object.get("accepts").is_some_and(Value::is_array)
        });
        Ok(declared.then_some(value))
    }

    pub(super) fn requirement(&self, wire: &X402PaymentRequired) -> Result<(X402Requirement, Value)> {
        for requirement in &wire.accepts {
            let value: Value = serde_json::from_str(requirement.0.get())
                .map_err(|error| Error::PaymentProtocol(format!("invalid x402 requirement JSON: {error}")))?;
            if let Ok(requirement) = self.parse_requirement(&value) {
                return Ok((requirement, value));
            }
        }
        Err(Error::PaymentProtocol("no supported x402 exact requirement".to_owned()))
    }

    pub(super) fn parse_requirement(&self, wire: &Value) -> Result<X402Requirement> {
        let network = wire
            .get("network")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("x402 requirement network is missing".to_owned()))?;
        let requirement = if X402Evm::valid_network(network) {
            X402Requirement::Evm(
                serde_json::from_value(wire.clone())
                    .map_err(|error| Error::PaymentProtocol(format!("invalid x402 EVM requirement: {error}")))?,
            )
        } else if X402Solana::valid_network(network) {
            X402Requirement::Solana(
                serde_json::from_value(wire.clone())
                    .map_err(|error| Error::PaymentProtocol(format!("invalid x402 Solana requirement: {error}")))?,
            )
        } else {
            return Err(Error::PaymentProtocol(
                "unsupported x402 requirement network".to_owned(),
            ));
        };
        X402Wire::validate_requirement(&requirement, self.max_timeout_seconds)?;
        self.registered_asset(&requirement.network(), &requirement.asset())?;
        if let X402Requirement::Solana(requirement) = &requirement {
            let runtime = self
                .solana
                .get(requirement.network.to_string().as_str())
                .ok_or_else(|| Error::PaymentProtocol("x402 Solana runtime is not configured".to_owned()))?;
            if runtime.fee_payer != requirement.extra.fee_payer {
                return Err(Error::PaymentProtocol(
                    "x402 Solana requirement fee payer is not registered".to_owned(),
                ));
            }
        }
        Ok(requirement)
    }

    pub(super) fn decode_challenge_wire(
        &self,
        expected_resource: &str,
        raw_wire: &Value,
        stored_selected: Option<&Value>,
    ) -> Result<X402ChallengeIntegrity> {
        ProtocolWire::validate_resource(expected_resource)?;
        let payment_required: X402PaymentRequired = serde_json::from_value(raw_wire.clone())
            .map_err(|error| Error::PaymentProtocol(format!("invalid x402 v2 challenge: {error}")))?;
        let resource = payment_required
            .resource
            .as_ref()
            .ok_or_else(|| Error::PaymentProtocol("x402 resource metadata is required".to_owned()))?;
        if resource.url != expected_resource {
            return Err(Error::PaymentProtocol(
                "x402 version or resource binding does not match the request".to_owned(),
            ));
        }

        let (selected, selected_wire) = match stored_selected {
            Some(selected) => (self.parse_requirement(selected)?, selected.clone()),
            None => self.requirement(&payment_required)?,
        };
        let advertised = payment_required.accepts.iter().any(|requirement| {
            serde_json::from_str::<Value>(requirement.0.get()).is_ok_and(|requirement| {
                ProtocolWire::canonical_json(&requirement) == ProtocolWire::canonical_json(&selected_wire)
            })
        });
        if !advertised {
            return Err(Error::PaymentProtocol(
                "x402 selected requirement is not advertised by PaymentRequired".to_owned(),
            ));
        }
        X402Wire::validate_requirement(&selected, self.max_timeout_seconds)?;

        let network = selected.network();
        let asset = self.registered_asset(&network, &selected.asset())?;
        let amount = selected.amount()?;
        let recipient = canonicalize_payment_recipient(&network, &selected.pay_to())
            .map_err(|_| Error::PaymentProtocol("x402 recipient is invalid for its network".to_owned()))?;
        Ok(X402ChallengeIntegrity {
            payment_required,
            selected,
            selected_wire,
            asset,
            amount,
            recipient,
        })
    }

    /// Reconstructs every security-sensitive field from the stored wire value.
    /// The protocol-neutral challenge is a cache of those fields, never a
    /// second source of truth.
    pub(super) fn validate_challenge_integrity(&self, challenge: &PaymentChallenge) -> Result<X402ChallengeIntegrity> {
        ProtocolWire::ensure_protocol(challenge, X402_PROTOCOL)?;
        let raw_wire = challenge
            .payload
            .get("payment_required")
            .ok_or_else(|| Error::PaymentProtocol("stored x402 challenge is missing".to_owned()))?;
        let stored_selected = challenge
            .payload
            .get("selected")
            .ok_or_else(|| Error::PaymentProtocol("x402 selected requirement is missing".to_owned()))?;
        let integrity = self.decode_challenge_wire(&challenge.resource, raw_wire, Some(stored_selected))?;
        let expected_hash =
            X402Wire::challenge_hash(&challenge.resource, challenge.expires_at, raw_wire, &challenge.nonce)?;
        let outer_recipient = canonicalize_payment_recipient(&challenge.asset.network, &challenge.recipient)
            .map_err(|_| Error::PaymentProtocol("x402 outer recipient is invalid".to_owned()))?;

        if challenge.asset != integrity.asset
            || challenge.amount != integrity.amount
            || challenge.recipient != outer_recipient
            || challenge.recipient != integrity.recipient
            || challenge.challenge_hash != expected_hash
            || challenge.expires_at <= 0
        {
            return Err(Error::PaymentProtocol(
                "stored x402 challenge integrity check failed".to_owned(),
            ));
        }
        Ok(integrity)
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
        let from_header = ProtocolWire::header_value(headers, "payment-required")
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
                X402Evm::prepare_signing(challenge, &integrity, requirement, payer, now)
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
            X402Requirement::Evm(_) => X402Evm::apply_signature(&mut request, signature)?,
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
        ProtocolWire::validate_resource(&price.resource)?;
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
            X402Evm::requirement(price, contract, &recipient, self.max_timeout_seconds)?
        } else {
            X402Solana::requirement(self, price, contract, &recipient)?
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
            X402PaymentPayload::Evm(payload) => X402Evm::fingerprint(&payload, challenge, &contract),
            X402PaymentPayload::Solana(payload) => X402Solana::fingerprint(&payload, challenge, &contract),
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
#[path = "../tests/x402_test.rs"]
mod tests;

// Strict x402 v2 wire projections and durable challenge identity.

pub struct X402Wire;

pub(super) struct X402ChallengeIntegrity {
    pub(super) payment_required: X402PaymentRequired,
    pub(super) selected: X402Requirement,
    pub(super) selected_wire: Value,
    pub(super) asset: AssetIdentity,
    pub(super) amount: Amount,
    pub(super) recipient: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum X402Requirement {
    Evm(EvmPaymentRequirements),
    Solana(SolanaPaymentRequirements),
}

impl X402Requirement {
    pub(super) fn network(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.network.to_string(),
            Self::Solana(requirement) => requirement.network.to_string(),
        }
    }

    pub(super) fn asset(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.asset.to_string(),
            Self::Solana(requirement) => requirement.asset.to_string(),
        }
    }

    pub(super) fn pay_to(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.pay_to.to_string(),
            Self::Solana(requirement) => requirement.pay_to.to_string(),
        }
    }

    pub(super) fn max_timeout_seconds(&self) -> u64 {
        match self {
            Self::Evm(requirement) => requirement.max_timeout_seconds,
            Self::Solana(requirement) => requirement.max_timeout_seconds,
        }
    }

    pub(super) fn amount(&self) -> Result<Amount> {
        match self {
            Self::Evm(requirement) => X402Evm::amount_from_u256(requirement.amount.0),
            Self::Solana(requirement) => {
                let amount = Amount::new(u128::from(requirement.amount.inner()));
                if amount.is_zero() {
                    return Err(Error::PaymentProtocol(
                        "payment amount must be a positive base-unit integer".to_owned(),
                    ));
                }
                Ok(amount)
            }
        }
    }

    pub(super) fn to_value(&self) -> Result<Value> {
        match self {
            Self::Evm(requirement) => Ok(serde_json::to_value(requirement)?),
            Self::Solana(requirement) => Ok(serde_json::to_value(requirement)?),
        }
    }
}

pub(super) enum X402PaymentPayload {
    Evm(EvmPaymentPayload),
    Solana(SolanaPaymentPayload),
}

impl X402PaymentPayload {
    pub(super) fn payer(&self) -> Result<String> {
        match self {
            Self::Evm(payload) => Ok(X402Evm::authorization_payload(payload)?.authorization.from.to_string()),
            Self::Solana(payload) => Ok(X402Solana::payer(
                &X402Solana::decode_transaction(&payload.payload.transaction)?,
                &payload.accepted,
            )?
            .to_string()),
        }
    }
}

impl X402Wire {
    pub(super) fn validate_requirement(requirement: &X402Requirement, max_timeout: u64) -> Result<()> {
        let valid = match requirement {
            X402Requirement::Evm(requirement) => {
                requirement.scheme.as_ref() == EXACT_SCHEME
                    && X402Evm::valid_network(&requirement.network.to_string())
                    && matches!(
                        &requirement.extra,
                        AssetTransferMethod::Eip3009 { name, version } if name == "USDC" && version == "2"
                    )
                    && !requirement.amount.0.is_zero()
            }
            X402Requirement::Solana(requirement) => {
                requirement.scheme.as_ref() == EXACT_SCHEME
                    && X402Solana::valid_network(&requirement.network.to_string())
                    && requirement.amount.inner() > 0
            }
        };
        if !valid || requirement.max_timeout_seconds() == 0 || requirement.max_timeout_seconds() > max_timeout {
            return Err(Error::PaymentProtocol(
                "unsupported or unsafe x402 requirement".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn decode_payment_payload(
        value: &Value,
        integrity: &X402ChallengeIntegrity,
    ) -> Result<X402PaymentPayload> {
        match &integrity.selected {
            X402Requirement::Evm(_) => X402Evm::decode_payment_payload(value).map(X402PaymentPayload::Evm),
            X402Requirement::Solana(_) => X402Solana::decode_payment_payload(value).map(X402PaymentPayload::Solana),
        }
    }

    pub(super) fn validate_payload(
        challenge: &PaymentChallenge,
        integrity: &X402ChallengeIntegrity,
        payload: &X402PaymentPayload,
        now: Option<i64>,
    ) -> Result<()> {
        match (payload, &integrity.selected) {
            (X402PaymentPayload::Evm(payload), X402Requirement::Evm(requirement)) => {
                X402Evm::validate_binding(challenge, integrity, payload, requirement, now)?;
            }
            (X402PaymentPayload::Solana(payload), X402Requirement::Solana(requirement)) => {
                X402Solana::validate_binding(challenge, integrity, payload, requirement)?;
            }
            _ => {
                return Err(Error::PaymentProtocol(
                    "x402 payload does not match the selected chain scheme".to_owned(),
                ))
            }
        }
        Ok(())
    }

    pub(super) fn payload_resource_matches(
        resource: Option<&ResourceInfo>,
        expected: Option<&ResourceInfo>,
        url: &str,
    ) -> bool {
        resource.is_none_or(|resource| X402Wire::resource_matches(resource, expected) && resource.url == url)
    }

    pub(super) fn resource_matches(resource: &ResourceInfo, expected: Option<&ResourceInfo>) -> bool {
        expected.is_some_and(|expected| {
            resource.url == expected.url
                && resource.description == expected.description
                && resource.mime_type == expected.mime_type
        })
    }

    pub(super) fn deterministic_nonce(resource: &str, expires_at: i64, raw_wire: &Value) -> Result<String> {
        let expires_at = expires_at.to_string();
        let canonical_wire = ProtocolWire::canonical_json(raw_wire);
        Ok(ProtocolWire::hash_values(&[
            b"agentvault.x402.challenge.nonce.v2",
            resource.as_bytes(),
            expires_at.as_bytes(),
            &serde_json::to_vec(&canonical_wire)?,
        ]))
    }

    pub(super) fn server_identity(resource: &str, expires_at: i64, raw_wire: &Value) -> Result<(String, String)> {
        let mut nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let nonce = hex::encode(nonce);
        let challenge_hash = X402Wire::challenge_hash(resource, expires_at, raw_wire, &nonce)?;
        Ok((nonce, challenge_hash))
    }

    pub(super) fn challenge_hash(resource: &str, expires_at: i64, raw_wire: &Value, nonce: &str) -> Result<String> {
        if nonce.len() != 64
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::PaymentProtocol("x402 challenge nonce is invalid".to_owned()));
        }
        let expires_at = expires_at.to_string();
        let canonical_wire = ProtocolWire::canonical_json(raw_wire);
        Ok(ProtocolWire::hash_values(&[
            b"agentvault.x402.challenge.v2",
            resource.as_bytes(),
            expires_at.as_bytes(),
            &serde_json::to_vec(&canonical_wire)?,
            nonce.as_bytes(),
        ]))
    }
}

#[cfg(test)]
#[path = "../tests/x402_wire_test.rs"]
mod wire_tests;

impl X402Wire {
    ///
    /// # Errors
    /// Returns a protocol or settlement error for malformed or incorrectly bound wire data.
    pub fn parse_settle_response(response: &[u8]) -> Result<SettleResponse> {
        let wire = ProtocolWire::strict_variant_wire(
            response,
            "x402",
            "success",
            &["success", "payer", "transaction", "network"],
            &["success", "payer", "transaction", "network"],
            &["success", "errorReason", "network"],
            &["success", "errorReason", "network"],
        )?;
        serde_json::from_value(wire)
            .map_err(|error| Error::Settlement(format!("invalid x402 /settle response: {error}")))
    }
}
