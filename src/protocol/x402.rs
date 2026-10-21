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
        PaymentChallenge, PaymentCredential, PaymentIntent, PaymentOffer, PaymentReceipt, PaymentStatus,
        EVM_EOA_ALGORITHM, SOLANA_ED25519_ALGORITHM,
    },
    Error, Result,
};

use super::{
    PaymentProtocol, ProtocolHttpCredential, ProtocolMerchantEvidence, ProtocolSettlement, ProtocolSigningRequest,
    ProtocolValidation,
};

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
            let valid_asset = if valid_evm_network(&asset.network) {
                valid_evm_address(&asset.contract)
            } else if valid_solana_network(&asset.network) {
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
            if !valid_solana_network(&config.network)
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
            let rpc = crate::utils::solana_rpc::client(
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
            .filter(|asset| valid_solana_network(&asset.network))
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

    fn registered_asset(&self, network: &str, contract: &str) -> Result<AssetIdentity> {
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

    fn header_value<'a>(headers: &'a BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
    }

    fn decode_payment_required_header(encoded: &str) -> Result<Value> {
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

    fn decode_payment_required_body(body: &str) -> Result<Option<Value>> {
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

    fn requirement(&self, wire: &X402PaymentRequired) -> Result<(X402Requirement, Value)> {
        for requirement in &wire.accepts {
            let value: Value = serde_json::from_str(requirement.0.get())
                .map_err(|error| Error::PaymentProtocol(format!("invalid x402 requirement JSON: {error}")))?;
            if let Ok(requirement) = self.parse_requirement(&value) {
                return Ok((requirement, value));
            }
        }
        Err(Error::PaymentProtocol("no supported x402 exact requirement".to_owned()))
    }

    fn parse_requirement(&self, wire: &Value) -> Result<X402Requirement> {
        let network = wire
            .get("network")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("x402 requirement network is missing".to_owned()))?;
        let requirement = if valid_evm_network(network) {
            X402Requirement::Evm(
                serde_json::from_value(wire.clone())
                    .map_err(|error| Error::PaymentProtocol(format!("invalid x402 EVM requirement: {error}")))?,
            )
        } else if valid_solana_network(network) {
            X402Requirement::Solana(
                serde_json::from_value(wire.clone())
                    .map_err(|error| Error::PaymentProtocol(format!("invalid x402 Solana requirement: {error}")))?,
            )
        } else {
            return Err(Error::PaymentProtocol(
                "unsupported x402 requirement network".to_owned(),
            ));
        };
        validate_requirement(&requirement, self.max_timeout_seconds)?;
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

    fn decode_challenge_wire(
        &self,
        expected_resource: &str,
        raw_wire: &Value,
        stored_selected: Option<&Value>,
    ) -> Result<X402ChallengeIntegrity> {
        validate_resource(expected_resource)?;
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
            serde_json::from_str::<Value>(requirement.0.get())
                .is_ok_and(|requirement| canonical_json(&requirement) == canonical_json(&selected_wire))
        });
        if !advertised {
            return Err(Error::PaymentProtocol(
                "x402 selected requirement is not advertised by PaymentRequired".to_owned(),
            ));
        }
        validate_requirement(&selected, self.max_timeout_seconds)?;

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
    fn validate_challenge_integrity(&self, challenge: &PaymentChallenge) -> Result<X402ChallengeIntegrity> {
        ensure_protocol(challenge, X402_PROTOCOL)?;
        let raw_wire = challenge
            .payload
            .get("payment_required")
            .ok_or_else(|| Error::PaymentProtocol("stored x402 challenge is missing".to_owned()))?;
        let stored_selected = challenge
            .payload
            .get("selected")
            .ok_or_else(|| Error::PaymentProtocol("x402 selected requirement is missing".to_owned()))?;
        let integrity = self.decode_challenge_wire(&challenge.resource, raw_wire, Some(stored_selected))?;
        let expected_hash = x402_challenge_hash(&challenge.resource, challenge.expires_at, raw_wire)?;
        let outer_recipient = canonicalize_payment_recipient(&challenge.asset.network, &challenge.recipient)
            .map_err(|_| Error::PaymentProtocol("x402 outer recipient is invalid".to_owned()))?;

        if challenge.asset != integrity.asset
            || challenge.amount != integrity.amount
            || challenge.recipient != outer_recipient
            || challenge.recipient != integrity.recipient
            || challenge.challenge_hash != expected_hash
            || challenge.nonce != expected_hash
            || challenge.expires_at <= 0
        {
            return Err(Error::PaymentProtocol(
                "stored x402 challenge integrity check failed".to_owned(),
            ));
        }
        Ok(integrity)
    }

    async fn prepare_solana_signing(
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
        let decoded = decode_solana_transaction(&transaction)?;
        if decoded.message.serialize() != digest {
            return Err(Error::PaymentProtocol(
                "official x402 Solana builder signed unexpected message bytes".to_owned(),
            ));
        }
        unsigned_solana_payer(&decoded, requirement, payer)?;
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
struct DeferredSolanaSigner {
    pubkey: Pubkey,
    message: Arc<Mutex<Option<Vec<u8>>>>,
}

impl DeferredSolanaSigner {
    fn new(pubkey: Pubkey) -> Self {
        Self {
            pubkey,
            message: Arc::new(Mutex::new(None)),
        }
    }

    fn message(&self) -> Result<Vec<u8>> {
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

struct FixedSolanaSigner {
    pubkey: Pubkey,
    message: Vec<u8>,
    signature: SolanaSignature,
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

struct X402ChallengeIntegrity {
    payment_required: X402PaymentRequired,
    selected: X402Requirement,
    selected_wire: Value,
    asset: AssetIdentity,
    amount: Amount,
    recipient: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum X402Requirement {
    Evm(EvmPaymentRequirements),
    Solana(SolanaPaymentRequirements),
}

impl X402Requirement {
    fn network(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.network.to_string(),
            Self::Solana(requirement) => requirement.network.to_string(),
        }
    }

    fn asset(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.asset.to_string(),
            Self::Solana(requirement) => requirement.asset.to_string(),
        }
    }

    fn pay_to(&self) -> String {
        match self {
            Self::Evm(requirement) => requirement.pay_to.to_string(),
            Self::Solana(requirement) => requirement.pay_to.to_string(),
        }
    }

    fn max_timeout_seconds(&self) -> u64 {
        match self {
            Self::Evm(requirement) => requirement.max_timeout_seconds,
            Self::Solana(requirement) => requirement.max_timeout_seconds,
        }
    }

    fn amount(&self) -> Result<Amount> {
        match self {
            Self::Evm(requirement) => amount_from_u256(requirement.amount.0),
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

    fn to_value(&self) -> Result<Value> {
        match self {
            Self::Evm(requirement) => Ok(serde_json::to_value(requirement)?),
            Self::Solana(requirement) => Ok(serde_json::to_value(requirement)?),
        }
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
        match parse_settle_response(&decoded)? {
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
        let payload = decode_payment_payload(&credential.payload, &integrity)?;
        validate_payload(challenge, &integrity, &payload, None)?;
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
        let challenge_hash = x402_challenge_hash(expected_resource, expires_at, wire)?;
        let challenge = PaymentChallenge {
            scenario_id: scenario_id.to_owned(),
            offer_id: None,
            resource: expected_resource.to_owned(),
            protocol: X402_PROTOCOL.to_owned(),
            asset: integrity.asset,
            amount: integrity.amount,
            recipient: integrity.recipient,
            nonce: challenge_hash.clone(),
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
        let payload = decode_payment_payload(&signed_payload, &integrity)?;
        validate_payload(challenge, &integrity, &payload, None)?;
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
                let digest = eip3009_digest(&payload)?.to_vec();
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
                let expected_digest = eip3009_digest(&request.payload)?;
                if request.digest.as_slice() != expected_digest {
                    return Err(Error::PaymentProtocol(
                        "x402 signing request digest no longer matches its payload".to_owned(),
                    ));
                }
                let mut payload = decode_evm_payment_payload(&request.payload)?;
                eip3009_payload_mut(&mut payload)?.signature = Bytes::copy_from_slice(signature);
                request.payload = serde_json::to_value(payload)?;
                let recovered = verify_eip3009_signature(&request.payload)?;
                if !recovered.eq_ignore_ascii_case(&payer_from_evm_payload(&request.payload)?) {
                    return Err(Error::PaymentProtocol(
                        "x402 signature was produced by a different wallet".to_owned(),
                    ));
                }
            }
            X402Requirement::Solana(_) => {
                request.payload = complete_solana_signing(&request, signature)?;
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

    async fn build_challenge(&self, offer: &PaymentOffer, recipient: &str, now: i64) -> Result<PaymentChallenge> {
        if !offer.supports_protocol(X402_PROTOCOL) {
            return Err(Error::PaymentProtocol(
                "offer cannot produce an x402 challenge".to_owned(),
            ));
        }
        let recipient = canonicalize_payment_recipient(&offer.asset.network, recipient)
            .map_err(|_| Error::PaymentProtocol("offer recipient is invalid for its network".to_owned()))?;
        validate_resource(&offer.resource)?;
        let contract = offer
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("x402 asset requires a token contract".to_owned()))?;
        let registered = self.registered_asset(&offer.asset.network, contract)?;
        if registered != offer.asset {
            return Err(Error::PaymentProtocol(
                "offer asset is not the registered x402 asset".to_owned(),
            ));
        }
        let requirement = if valid_evm_network(&offer.asset.network) {
            X402Requirement::Evm(EvmPaymentRequirements {
                scheme: ExactScheme,
                network: offer
                    .asset
                    .network
                    .parse()
                    .map_err(|_| Error::PaymentProtocol("x402 network is not a valid CAIP-2 ID".to_owned()))?,
                amount: DecimalU256(U256::from(offer.amount.value())),
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
                .get(&offer.asset.network)
                .ok_or_else(|| Error::PaymentProtocol("x402 Solana runtime is not configured".to_owned()))?;
            let amount = u64::try_from(offer.amount.value())
                .map_err(|_| Error::PaymentProtocol("x402 Solana amount exceeds u64".to_owned()))?;
            X402Requirement::Solana(SolanaPaymentRequirements {
                scheme: x402_chain_solana::v2_solana_exact::ExactScheme,
                network: offer
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
                url: offer.resource.clone(),
                description: None,
                mime_type: None,
            }),
            accepts: vec![raw],
        };
        let wire = serde_json::to_value(&required)?;
        let challenge_hash = x402_challenge_hash(&offer.resource, expires_at, &wire)?;
        let challenge = PaymentChallenge {
            scenario_id: offer.scenario_id.clone(),
            offer_id: Some(offer.id.clone()),
            resource: offer.resource.clone(),
            protocol: X402_PROTOCOL.to_owned(),
            asset: offer.asset.clone(),
            amount: offer.amount,
            recipient,
            nonce: challenge_hash.clone(),
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
        let payload = decode_payment_payload(&credential.payload, &integrity)?;
        validate_payload(challenge, &integrity, &payload, Some(now))?;
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
        let payload = decode_payment_payload(&credential.payload, &integrity)?;
        validate_payload(challenge, &integrity, &payload, None)?;
        let contract = challenge
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("x402 asset has no token contract".to_owned()))?;
        let contract = canonicalize_payment_recipient(&challenge.asset.network, contract)
            .map_err(|_| Error::PaymentProtocol("x402 asset contract is invalid".to_owned()))?;
        match payload {
            X402PaymentPayload::Evm(payload) => {
                let eip3009 = eip3009_payload(&payload)?;
                let payer = eip3009.authorization.from.to_string().to_ascii_lowercase();
                let nonce = eip3009.authorization.nonce.as_slice();
                Ok(hash_values(&[
                    b"agentvault.x402.replay.v1",
                    challenge.asset.network.as_bytes(),
                    contract.as_bytes(),
                    payer.as_bytes(),
                    nonce,
                ]))
            }
            X402PaymentPayload::Solana(payload) => {
                let transaction = decode_solana_transaction(&payload.payload.transaction)?;
                let payer = solana_payer(&transaction, &payload.accepted)?.to_string();
                let message = transaction.message.serialize();
                Ok(hash_values(&[
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
        let payload = decode_payment_payload(&credential.payload, &integrity)?;
        validate_payload(challenge, &integrity, &payload, None)?;
        Ok(ProtocolSettlement {
            protocol: X402_PROTOCOL.to_owned(),
            payment_payload: credential.payload.clone(),
            payment_requirements: integrity.selected_wire,
        })
    }
}

fn validate_requirement(requirement: &X402Requirement, max_timeout: u64) -> Result<()> {
    let valid = match requirement {
        X402Requirement::Evm(requirement) => {
            requirement.scheme.as_ref() == EXACT_SCHEME
                && valid_evm_network(&requirement.network.to_string())
                && matches!(
                    &requirement.extra,
                    AssetTransferMethod::Eip3009 { name, version } if name == "USDC" && version == "2"
                )
                && !requirement.amount.0.is_zero()
        }
        X402Requirement::Solana(requirement) => {
            requirement.scheme.as_ref() == EXACT_SCHEME
                && valid_solana_network(&requirement.network.to_string())
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

enum X402PaymentPayload {
    Evm(EvmPaymentPayload),
    Solana(SolanaPaymentPayload),
}

impl X402PaymentPayload {
    fn payer(&self) -> Result<String> {
        match self {
            Self::Evm(payload) => Ok(eip3009_payload(payload)?.authorization.from.to_string()),
            Self::Solana(payload) => Ok(solana_payer(
                &decode_solana_transaction(&payload.payload.transaction)?,
                &payload.accepted,
            )?
            .to_string()),
        }
    }
}

fn decode_payment_payload(value: &Value, integrity: &X402ChallengeIntegrity) -> Result<X402PaymentPayload> {
    match &integrity.selected {
        X402Requirement::Evm(_) => decode_evm_payment_payload(value).map(X402PaymentPayload::Evm),
        X402Requirement::Solana(_) => decode_solana_payment_payload(value).map(X402PaymentPayload::Solana),
    }
}

fn validate_payload(
    challenge: &PaymentChallenge,
    integrity: &X402ChallengeIntegrity,
    payload: &X402PaymentPayload,
    now: Option<i64>,
) -> Result<()> {
    match (payload, &integrity.selected) {
        (X402PaymentPayload::Evm(payload), X402Requirement::Evm(requirement)) => {
            let authorization = &eip3009_payload(payload)?.authorization;
            let valid_after = i64::try_from(authorization.valid_after.as_secs())
                .map_err(|_| Error::PaymentProtocol("x402 validAfter is invalid".to_owned()))?;
            let valid_before = i64::try_from(authorization.valid_before.as_secs())
                .map_err(|_| Error::PaymentProtocol("x402 validBefore is invalid".to_owned()))?;
            if !payload_resource_matches(
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
                || !valid_signature(&eip3009_payload(payload)?.signature)
                || now.is_some_and(|now| now < valid_after || now >= valid_before)
            {
                return Err(Error::PaymentProtocol(
                    "x402 EIP-3009 payload binding failed".to_owned(),
                ));
            }
        }
        (X402PaymentPayload::Solana(payload), X402Requirement::Solana(requirement)) => {
            let transaction = decode_solana_transaction(&payload.payload.transaction)?;
            let payer = solana_payer(&transaction, &payload.accepted)?;
            if !payload_resource_matches(
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
        }
        _ => {
            return Err(Error::PaymentProtocol(
                "x402 payload does not match the selected chain scheme".to_owned(),
            ))
        }
    }
    Ok(())
}

fn payload_resource_matches(resource: Option<&ResourceInfo>, expected: Option<&ResourceInfo>, url: &str) -> bool {
    resource.is_none_or(|resource| resource_matches(resource, expected) && resource.url == url)
}

fn resource_matches(resource: &ResourceInfo, expected: Option<&ResourceInfo>) -> bool {
    expected.is_some_and(|expected| {
        resource.url == expected.url
            && resource.description == expected.description
            && resource.mime_type == expected.mime_type
    })
}

fn eip3009_payload(payload: &EvmPaymentPayload) -> Result<&Eip3009Payload> {
    match &payload.payload {
        ExactEvmPayload::Eip3009(payload) => Ok(payload),
        ExactEvmPayload::Permit2(_) => Err(Error::PaymentProtocol(
            "x402 exact currently supports only EIP-3009".to_owned(),
        )),
    }
}

fn eip3009_payload_mut(payload: &mut EvmPaymentPayload) -> Result<&mut Eip3009Payload> {
    match &mut payload.payload {
        ExactEvmPayload::Eip3009(payload) => Ok(payload),
        ExactEvmPayload::Permit2(_) => Err(Error::PaymentProtocol(
            "x402 exact currently supports only EIP-3009".to_owned(),
        )),
    }
}

fn complete_solana_signing(request: &ProtocolSigningRequest, signature: &[u8]) -> Result<Value> {
    let mut payload = decode_solana_payment_payload(&request.payload)?;
    let transaction = decode_solana_transaction(&payload.payload.transaction)?;
    let message = transaction.message.serialize();
    if request.digest != message || signature.len() != 64 {
        return Err(Error::PaymentProtocol(
            "x402 signer returned invalid Solana transaction material".to_owned(),
        ));
    }
    let payer = unsigned_solana_payer_from_requirement(&transaction, &payload.accepted)?;
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
    let completed = decode_solana_payment_payload(&value)?;
    solana_payer(
        &decode_solana_transaction(&completed.payload.transaction)?,
        &completed.accepted,
    )?;
    Ok(value)
}

fn decode_solana_transaction(encoded: &str) -> Result<VersionedTransaction> {
    if encoded.is_empty() || encoded.len() > MAX_X402_HTTP_WIRE_BYTES {
        return Err(Error::PaymentProtocol(
            "x402 Solana transaction encoding is invalid".to_owned(),
        ));
    }
    let bytes = Base64Bytes::from(encoded.as_bytes())
        .decode()
        .map_err(|_| Error::PaymentProtocol("x402 Solana transaction is not valid base64".to_owned()))?;
    bincode::deserialize(&bytes).map_err(|_| Error::PaymentProtocol("x402 Solana transaction is malformed".to_owned()))
}

fn required_solana_signers(transaction: &VersionedTransaction) -> Result<&[Pubkey]> {
    let required = usize::from(transaction.message.header().num_required_signatures);
    let keys = transaction.message.static_account_keys();
    if required == 0 || required > keys.len() || transaction.signatures.len() != required {
        return Err(Error::PaymentProtocol(
            "x402 Solana transaction signer layout is invalid".to_owned(),
        ));
    }
    Ok(&keys[..required])
}

fn unsigned_solana_payer(
    transaction: &VersionedTransaction,
    requirement: &SolanaPaymentRequirements,
    expected: Pubkey,
) -> Result<Pubkey> {
    let payer = unsigned_solana_payer_from_requirement(transaction, requirement)?;
    if payer != expected {
        return Err(Error::PaymentProtocol(
            "official x402 Solana transaction is bound to a different payer".to_owned(),
        ));
    }
    Ok(payer)
}

fn unsigned_solana_payer_from_requirement(
    transaction: &VersionedTransaction,
    requirement: &SolanaPaymentRequirements,
) -> Result<Pubkey> {
    let signers = required_solana_signers(transaction)?;
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

fn solana_payer(transaction: &VersionedTransaction, requirement: &SolanaPaymentRequirements) -> Result<Pubkey> {
    let signers = required_solana_signers(transaction)?;
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
pub(crate) fn solana_payment_payer(payload: &Value) -> Result<String> {
    let payload = decode_solana_payment_payload(payload)?;
    solana_payer(
        &decode_solana_transaction(&payload.payload.transaction)?,
        &payload.accepted,
    )
    .map(|payer| payer.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Eip3009AuthorizationIdentity {
    pub payer: String,
    pub nonce: String,
    pub valid_before: i64,
}

pub(crate) fn eip3009_authorization_identity(payload: &Value) -> Result<Eip3009AuthorizationIdentity> {
    let payload = decode_evm_payment_payload(payload)?;
    let authorization = &eip3009_payload(&payload)?.authorization;
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
pub fn eip3009_digest(payload: &Value) -> Result<[u8; 32]> {
    let payload = decode_evm_payment_payload(payload)?;
    let requirement = &payload.accepted;
    validate_requirement(&X402Requirement::Evm(requirement.clone()), u64::MAX)?;
    let authorization = &eip3009_payload(&payload)?.authorization;
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
pub fn verify_eip3009_signature(payload: &Value) -> Result<String> {
    let decoded = decode_evm_payment_payload(payload)?;
    let bytes = eip3009_payload(&decoded)?.signature.as_ref();
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
    let digest = eip3009_digest(payload)?;
    let key = VerifyingKey::recover_from_prehash(&digest, &signature, recovery)
        .map_err(|_| Error::PaymentProtocol("x402 EIP-3009 signature recovery failed".to_owned()))?;
    let point = key.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

fn x402_challenge_hash(resource: &str, expires_at: i64, raw_wire: &Value) -> Result<String> {
    let expires_at = expires_at.to_string();
    let canonical_wire = canonical_json(raw_wire);
    Ok(hash_values(&[
        b"agentvault.x402.challenge.v2",
        resource.as_bytes(),
        expires_at.as_bytes(),
        &serde_json::to_vec(&canonical_wire)?,
    ]))
}

/// Produces a stable semantic representation across JSON parsers and SQL
/// `json`/`jsonb` round-trips. Arrays retain protocol order; object keys are
/// sorted recursively so storage-specific map ordering cannot invalidate a
/// legitimate challenge.
fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        Value::Object(fields) => {
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&fields[key]));
            }
            Value::Object(canonical)
        }
        scalar => scalar.clone(),
    }
}

fn ensure_protocol(challenge: &PaymentChallenge, expected: &str) -> Result<()> {
    if challenge.protocol == expected {
        Ok(())
    } else {
        Err(Error::PaymentProtocol("protocol adapter mismatch".to_owned()))
    }
}

fn amount_from_u256(value: U256) -> Result<Amount> {
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

fn payer_from_evm_payload(payload: &Value) -> Result<String> {
    let payload = decode_evm_payment_payload(payload)?;
    Ok(eip3009_payload(&payload)?.authorization.from.to_string())
}

fn decode_evm_payment_payload(value: &Value) -> Result<EvmPaymentPayload> {
    validate_evm_payment_payload_wire(value)?;
    serde_json::from_value(value.clone())
        .map_err(|error| Error::PaymentProtocol(format!("invalid x402 payment payload: {error}")))
}

fn decode_solana_payment_payload(value: &Value) -> Result<SolanaPaymentPayload> {
    validate_solana_payment_payload_wire(value)?;
    serde_json::from_value(value.clone())
        .map_err(|error| Error::PaymentProtocol(format!("invalid x402 Solana payment payload: {error}")))
}

/// Validates the exact official v2 EIP-3009 payload shape before typed
/// deserialization. The upstream official types intentionally tolerate
/// unknown JSON fields, while a signed payment boundary must fail closed.
pub(crate) fn validate_evm_payment_payload_wire(value: &Value) -> Result<()> {
    exact_object_fields(
        value,
        &["x402Version", "accepted", "payload"],
        &["x402Version", "accepted", "payload", "resource", "extensions"],
        "paymentPayload",
    )?;
    let accepted = value
        .get("accepted")
        .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload accepted is missing".to_owned()))?;
    exact_object_fields(
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
    exact_object_fields(
        extra,
        &["name", "version"],
        &["assetTransferMethod", "name", "version"],
        "paymentPayload.accepted.extra",
    )?;
    let payload = value
        .get("payload")
        .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload payload is missing".to_owned()))?;
    exact_object_fields(
        payload,
        &["signature", "authorization"],
        &["signature", "authorization"],
        "paymentPayload.payload",
    )?;
    let authorization = payload
        .get("authorization")
        .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload authorization is missing".to_owned()))?;
    exact_object_fields(
        authorization,
        &["from", "to", "value", "validAfter", "validBefore", "nonce"],
        &["from", "to", "value", "validAfter", "validBefore", "nonce"],
        "paymentPayload.payload.authorization",
    )?;
    if let Some(resource) = value.get("resource") {
        exact_object_fields(
            resource,
            &["url"],
            &["url", "description", "mimeType"],
            "paymentPayload.resource",
        )?;
    }
    Ok(())
}

/// Validates the exact official v2 Solana payload shape before typed
/// deserialization. The actual requirement and transaction semantics remain
/// delegated to the official x402 Solana types and verifier.
pub(crate) fn validate_solana_payment_payload_wire(value: &Value) -> Result<()> {
    exact_object_fields(
        value,
        &["x402Version", "accepted", "payload"],
        &["x402Version", "accepted", "payload", "resource", "extensions"],
        "paymentPayload",
    )?;
    let accepted = value
        .get("accepted")
        .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload accepted is missing".to_owned()))?;
    exact_object_fields(
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
    exact_object_fields(extra, &["feePayer"], &["feePayer"], "paymentPayload.accepted.extra")?;
    let payload = value
        .get("payload")
        .ok_or_else(|| Error::PaymentProtocol("x402 paymentPayload payload is missing".to_owned()))?;
    exact_object_fields(payload, &["transaction"], &["transaction"], "paymentPayload.payload")?;
    if let Some(resource) = value.get("resource") {
        exact_object_fields(
            resource,
            &["url"],
            &["url", "description", "mimeType"],
            "paymentPayload.resource",
        )?;
    }
    Ok(())
}

fn exact_object_fields(value: &Value, required: &[&str], allowed: &[&str], name: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::PaymentProtocol(format!("x402 {name} must be an object")))?;
    if required.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !allowed.contains(&field.as_str()))
    {
        return Err(Error::PaymentProtocol(format!(
            "x402 {name} fields do not match the official wire"
        )));
    }
    Ok(())
}

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

fn validate_resource(resource: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(resource)
        .map_err(|_| Error::PaymentProtocol("payment resource is not an absolute URL".to_owned()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Error::PaymentProtocol("payment resource URL is unsafe".to_owned()));
    }
    Ok(())
}

fn valid_evm_network(network: &str) -> bool {
    network
        .strip_prefix("eip155:")
        .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_solana_network(network: &str) -> bool {
    network
        .parse::<ChainId>()
        .ok()
        .and_then(|chain| SolanaChainReference::try_from(chain).ok())
        .is_some()
}

fn valid_evm_address(value: &str) -> bool {
    canonicalize_wallet_address(EVM_EOA_ALGORITHM, value).is_ok()
}

fn valid_signature(value: &Bytes) -> bool {
    value.len() == 65
}

fn hash_values(values: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for value in values {
        hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(value);
    }
    hex::encode(hash.finalize())
}

#[cfg(test)]
#[path = "../../tests/protocol/x402_test.rs"]
mod tests;
