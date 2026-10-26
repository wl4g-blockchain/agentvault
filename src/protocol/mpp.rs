//! MPP/Tempo `charge` adapter built on the official `mpp` Rust crate.

use std::{collections::BTreeMap, num::NonZeroU64};

use alloy::eips::Encodable2718;
use alloy_primitives::{Address, Bytes, Signature, TxKind, B256, U256};
use alloy_sol_types::SolCall;
use async_trait::async_trait;
use mpp::{
    client::tempo::{
        charge::{
            tx_builder::{build_charge_credential, build_tempo_tx, TempoTxOptions},
            TempoCharge,
        },
        MAX_FEE_PER_GAS, MAX_PRIORITY_FEE_PER_GAS,
    },
    format_authorization, format_www_authenticate, parse_authorization, parse_receipt, parse_www_authenticate_all,
    Base64UrlJson, ChargeRequest, PayloadType,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tempo_alloy::{
    contracts::precompiles::ITIP20,
    primitives::transaction::{Call, TempoTransaction},
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    model::{
        canonicalize_wallet_address, payment_recipients_match, Amount, AssetIdentity, PaymentChallenge,
        PaymentCredential, PaymentIntent, PaymentPrice, PaymentReceipt, PaymentStatus, EVM_EOA_ALGORITHM,
    },
    Error, Result,
};

use super::{
    PaymentProtocol, ProtocolHttpCredential, ProtocolMerchantEvidence, ProtocolSettlement, ProtocolValidation,
};

#[path = "mpp_tempo.rs"]
mod tempo;
#[path = "mpp_wire.rs"]
mod wire;
use tempo::TempoSigningPayload;
use wire::MppWire;

const MPP_PROTOCOL: &str = "mpp";
const TEMPO_METHOD: &str = "tempo";
const TEMPO_SIGNING_PAYLOAD_KIND: &str = "tempo_charge_v1";
const TEMPO_CHARGE_GAS_LIMIT: u64 = 1_000_000;
const TEMPO_CHARGE_VALIDITY_SECONDS: i64 = 25;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MppAsset {
    pub network: String,
    pub currency: String,
    pub symbol: String,
    pub token_contract: Option<String>,
    pub decimals: u8,
}

/// Official-SDK-backed translation adapter for the initial Tempo charge flow.
pub struct MppAdapter {
    assets: Vec<MppAsset>,
    payee: Option<MppPayeeSettings>,
    timeout_seconds: u64,
}

struct MppPayeeSettings {
    realm: String,
    challenge_secret: Zeroizing<String>,
}

impl MppAdapter {
    /// Creates a payer-only Tempo charge adapter. It can parse remote
    /// challenges and build credentials but cannot issue payee challenges.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty asset registry, invalid network, or unsafe
    /// challenge timeout.
    pub fn new_payer(assets: Vec<MppAsset>, timeout_seconds: u64) -> Result<Self> {
        Self::new_for_direction(assets, None, timeout_seconds)
    }

    /// Creates a payee Tempo charge adapter with the secret used by the
    /// official SDK to authenticate locally issued challenge IDs.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid payer settings or insufficient payee
    /// challenge-key material.
    pub fn new_payee(
        assets: Vec<MppAsset>,
        realm: impl Into<String>,
        challenge_secret: impl Into<String>,
        timeout_seconds: u64,
    ) -> Result<Self> {
        let realm = realm.into();
        let challenge_secret = Zeroizing::new(challenge_secret.into());
        if realm.trim().is_empty() || challenge_secret.len() < 32 {
            return Err(Error::Config("MPP Tempo payee configuration is invalid".to_owned()));
        }
        Self::new_for_direction(
            assets,
            Some(MppPayeeSettings {
                realm,
                challenge_secret,
            }),
            timeout_seconds,
        )
    }

    fn new_for_direction(assets: Vec<MppAsset>, payee: Option<MppPayeeSettings>, timeout_seconds: u64) -> Result<Self> {
        if assets.is_empty() || timeout_seconds == 0 || timeout_seconds > 3_600 {
            return Err(Error::Config("MPP Tempo adapter configuration is invalid".to_owned()));
        }
        let mut normalized_assets = Vec::with_capacity(assets.len());
        for mut asset in assets {
            if !MppWire::valid_evm_network(&asset.network) || asset.symbol.trim().is_empty() {
                return Err(Error::Config("MPP Tempo adapter configuration is invalid".to_owned()));
            }
            let currency = canonicalize_wallet_address(EVM_EOA_ALGORITHM, &asset.currency)
                .map_err(|_| Error::Config("MPP Tempo currency must be an EVM token address".to_owned()))?;
            if let Some(contract) = asset.token_contract.as_deref() {
                let contract = canonicalize_wallet_address(EVM_EOA_ALGORITHM, contract)
                    .map_err(|_| Error::Config("MPP Tempo token_contract must be an EVM address".to_owned()))?;
                if contract != currency {
                    return Err(Error::Config(
                        "MPP Tempo currency and token_contract must identify the same token".to_owned(),
                    ));
                }
            }
            asset.currency.clone_from(&currency);
            asset.token_contract = Some(currency);
            normalized_assets.push(asset);
        }
        Ok(Self {
            assets: normalized_assets,
            payee,
            timeout_seconds,
        })
    }

    fn registered_asset(&self, network: &str, currency: &str) -> Result<AssetIdentity> {
        let asset = self
            .assets
            .iter()
            .find(|asset| asset.network == network && asset.currency.eq_ignore_ascii_case(currency))
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo asset or network is not registered".to_owned()))?;
        Ok(AssetIdentity {
            network: asset.network.clone(),
            asset: asset.symbol.clone(),
            token_contract: asset.token_contract.clone(),
            decimals: asset.decimals,
        })
    }

    fn decode_challenge_header(&self, expected_resource: &str, header: &str) -> Result<MppChallengeIntegrity> {
        MppWire::validate_resource(expected_resource)?;
        let mut selected = None;
        for parsed in parse_www_authenticate_all([header]) {
            let challenge = parsed.map_err(MppWire::error)?;
            if selected.is_none() && challenge.method.eq_ignore_ascii_case(TEMPO_METHOD) && challenge.intent.is_charge()
            {
                selected = Some(challenge);
            }
        }
        let official = selected
            .ok_or_else(|| Error::PaymentProtocol("no allowed MPP Tempo charge challenge was advertised".to_owned()))?;
        let selected_header = format_www_authenticate(&official).map_err(MppWire::error)?;
        let request: ChargeRequest = official.request.decode().map_err(MppWire::error)?;
        let scoped_resource = request
            .mppx_scope
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|scope| scope.get("resource"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP charge resource scope is required".to_owned()))?;
        if scoped_resource != expected_resource {
            return Err(Error::PaymentProtocol(
                "MPP charge resource scope does not match the request".to_owned(),
            ));
        }

        let network = MppWire::tempo_network(&request)?;
        let asset = self.registered_asset(&network, &request.currency)?;
        let amount = request
            .parse_amount()
            .ok()
            .map(Amount::new)
            .filter(|amount| !amount.is_zero())
            .ok_or_else(|| Error::PaymentProtocol("MPP amount must be a positive base-unit integer".to_owned()))?;
        let recipient = request
            .recipient
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("MPP charge recipient is required".to_owned()))
            .and_then(|recipient| {
                canonicalize_wallet_address(EVM_EOA_ALGORITHM, recipient)
                    .map_err(|_| Error::PaymentProtocol("MPP charge recipient is not a valid EVM address".to_owned()))
            })?;
        let expires_at = official
            .expires_at()
            .map(time::OffsetDateTime::unix_timestamp)
            .ok_or_else(|| Error::PaymentProtocol("MPP charge expiry is required and must be valid".to_owned()))?;
        if expires_at <= 0 {
            return Err(Error::PaymentProtocol("MPP charge expiry is invalid".to_owned()));
        }

        Ok(MppChallengeIntegrity {
            official,
            request,
            header: selected_header,
            asset,
            amount,
            recipient,
            expires_at,
        })
    }

    /// Decodes the stored header through the official SDK and treats the
    /// protocol-neutral fields only as a checked projection of that wire data.
    fn validate_challenge_integrity(&self, challenge: &PaymentChallenge) -> Result<MppChallengeIntegrity> {
        MppWire::ensure_protocol(challenge)?;
        let header = challenge
            .payload
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("stored MPP challenge is missing".to_owned()))?;
        let integrity = self.decode_challenge_header(&challenge.resource, header)?;
        if let Some(projected) = challenge.payload.get("mcp_challenge") {
            if Self::mcp_challenge_value(&integrity.official)? != *projected {
                return Err(Error::PaymentProtocol(
                    "stored MPP MCP challenge projection is inconsistent".to_owned(),
                ));
            }
        }
        let expected_hash = MppWire::challenge_hash(&challenge.resource, header);
        let outer_recipient = canonicalize_wallet_address(EVM_EOA_ALGORITHM, &challenge.recipient)
            .map_err(|_| Error::PaymentProtocol("MPP outer recipient is not a valid EVM address".to_owned()))?;
        if challenge.asset != integrity.asset
            || challenge.amount != integrity.amount
            || challenge.recipient != outer_recipient
            || challenge.recipient != integrity.recipient
            || challenge.nonce != integrity.official.id
            || challenge.expires_at != integrity.expires_at
            || challenge.challenge_hash != expected_hash
        {
            return Err(Error::PaymentProtocol(
                "stored MPP challenge integrity check failed".to_owned(),
            ));
        }

        if let Some(offer_id) = challenge.offer_id.as_deref() {
            let payee = self.payee.as_ref().ok_or_else(|| {
                Error::PaymentProtocol("payer-only MPP adapter cannot verify a payee challenge".to_owned())
            })?;
            if !integrity.official.verify(payee.challenge_secret.as_str())
                || integrity.request.external_id.as_deref() != Some(offer_id)
            {
                return Err(Error::PaymentProtocol(
                    "payee MPP challenge authenticity check failed".to_owned(),
                ));
            }
            let opaque = integrity
                .official
                .opaque
                .as_ref()
                .ok_or_else(|| Error::PaymentProtocol("payee MPP challenge metadata is missing".to_owned()))?
                .decode::<BTreeMap<String, String>>()
                .map_err(MppWire::error)?;
            if opaque.get("offer_id").map(String::as_str) != Some(offer_id)
                || opaque.get("resource").map(String::as_str) != Some(challenge.resource.as_str())
                || opaque.get("scenario_id").map(String::as_str) != Some(challenge.scenario_id.as_str())
            {
                return Err(Error::PaymentProtocol(
                    "payee MPP challenge metadata binding failed".to_owned(),
                ));
            }
        }
        Ok(integrity)
    }

    fn wrap_official_credential(
        challenge: &PaymentChallenge,
        credential: &mpp::PaymentCredential,
    ) -> Result<PaymentCredential> {
        let authorization = format_authorization(credential).map_err(MppWire::error)?;
        Ok(PaymentCredential {
            protocol: MPP_PROTOCOL.to_owned(),
            challenge_hash: challenge.challenge_hash.clone(),
            credential_hash: MppWire::hash_values(&[
                b"agentvault.mpp.credential.v1",
                challenge.challenge_hash.as_bytes(),
                authorization.as_bytes(),
            ]),
            payload: json!({"authorization": authorization}),
        })
    }

    /// MCP expands `request` as native JSON while HTTP carries the same value
    /// as base64url. Keep that transport projection beside the canonical HTTP
    /// value so SDK providers do not reimplement protocol semantics.
    fn mcp_challenge_value(challenge: &mpp::PaymentChallenge) -> Result<Value> {
        let error = serde_json::to_value(mpp::mcp::payment_required_error(challenge))?;
        error
            .pointer("/data/challenges/0")
            .cloned()
            .ok_or_else(|| Error::PaymentProtocol("MPP MCP projection omitted its challenge".to_owned()))
    }

    fn payer_from_source(network: &str, source: Option<&str>) -> Result<String> {
        let chain_id = network
            .strip_prefix("eip155:")
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo payer network is invalid".to_owned()))?;
        let prefix = format!("did:pkh:eip155:{chain_id}:");
        let payer = source.and_then(|source| source.strip_prefix(&prefix)).ok_or_else(|| {
            Error::PaymentProtocol("MPP Tempo credential source DID is missing or mismatched".to_owned())
        })?;
        canonicalize_wallet_address(EVM_EOA_ALGORITHM, payer)
            .map_err(|_| Error::PaymentProtocol("MPP Tempo credential source address is invalid".to_owned()))
    }
}

struct MppChallengeIntegrity {
    official: mpp::PaymentChallenge,
    request: ChargeRequest,
    header: String,
    asset: AssetIdentity,
    amount: Amount,
    recipient: String,
    expires_at: i64,
}

#[async_trait]
impl PaymentProtocol for MppAdapter {
    fn name(&self) -> &'static str {
        MPP_PROTOCOL
    }

    fn decode_http_challenge(
        &self,
        resource: &str,
        headers: &BTreeMap<String, String>,
        _body: &str,
    ) -> Result<Option<Value>> {
        let Some(header) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("www-authenticate"))
            .map(|(_, value)| value.as_str())
        else {
            return Ok(None);
        };
        let mut advertised_payment = false;
        let mut advertised_supported = false;
        for parsed in parse_www_authenticate_all([header]) {
            advertised_payment = true;
            let challenge = parsed.map_err(MppWire::error)?;
            advertised_supported |= challenge.method.eq_ignore_ascii_case(TEMPO_METHOD) && challenge.intent.is_charge();
        }
        if !advertised_payment || !advertised_supported {
            return Ok(None);
        }
        let integrity = self.decode_challenge_header(resource, header)?;
        Ok(Some(json!({"www_authenticate": integrity.header})))
    }

    fn encode_http_credential(&self, credential: &PaymentCredential) -> Result<ProtocolHttpCredential> {
        if credential.protocol != MPP_PROTOCOL {
            return Err(Error::PaymentProtocol("MPP credential protocol mismatch".to_owned()));
        }
        let header_value = credential
            .payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP credential has no Authorization value".to_owned()))?;
        let submitted = parse_authorization(header_value).map_err(MppWire::error)?;
        let header_name = match submitted.challenge.header.as_deref() {
            Some(header) if header.eq_ignore_ascii_case("payment-authorization") => "payment-authorization",
            Some(_) => {
                return Err(Error::PaymentProtocol(
                    "MPP challenge selected an unsupported credential header".to_owned(),
                ))
            }
            None => "authorization",
        };
        Ok(ProtocolHttpCredential {
            header_name: header_name.to_owned(),
            header_value: header_value.to_owned(),
        })
    }

    fn merchant_evidence_header(&self) -> &'static str {
        "payment-receipt"
    }

    fn decode_merchant_evidence(&self, value: &str) -> Result<ProtocolMerchantEvidence> {
        if value.is_empty() || value.len() > 16 * 1024 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(Error::InvalidRequest(
                "MPP Payment-Receipt header is invalid".to_owned(),
            ));
        }
        let receipt = parse_receipt(value).map_err(MppWire::error)?;
        if !receipt.is_success()
            || !receipt.method.eq_ignore_ascii_case(TEMPO_METHOD)
            || receipt.subscription_id.is_some()
            || !MppWire::valid_transaction_hash(&receipt.reference)
        {
            return Err(Error::PaymentProtocol(
                "MPP receipt is not a successful Tempo charge transaction".to_owned(),
            ));
        }
        Ok(ProtocolMerchantEvidence {
            accepted: true,
            transaction: Some(receipt.reference.to_ascii_lowercase()),
            network: None,
            payer: None,
        })
    }

    fn credential_payer(&self, challenge: &PaymentChallenge, credential: &PaymentCredential) -> Result<String> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != MPP_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol("MPP credential binding mismatch".to_owned()));
        }
        let authorization = credential
            .payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP Authorization value is missing".to_owned()))?;
        let submitted = parse_authorization(authorization).map_err(MppWire::error)?;
        if serde_json::to_value(&submitted.challenge)? != serde_json::to_value(integrity.official.to_echo())? {
            return Err(Error::PaymentProtocol("MPP challenge echo mismatch".to_owned()));
        }
        let payload = submitted.charge_payload().map_err(MppWire::error)?;
        if payload.payload_type() != PayloadType::Transaction {
            return Err(Error::PaymentProtocol(
                "MPP Tempo charge requires a signed transaction credential".to_owned(),
            ));
        }
        Self::payer_from_source(&challenge.asset.network, submitted.source.as_deref())
    }

    async fn parse_challenge(
        &self,
        scenario_id: &str,
        expected_resource: &str,
        wire: &Value,
        now: i64,
        advertised_expires_at: Option<i64>,
    ) -> Result<PaymentChallenge> {
        let header = wire
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP WWW-Authenticate value is missing".to_owned()))?;
        let integrity = self.decode_challenge_header(expected_resource, header)?;
        let maximum_expiry = now.saturating_add(
            i64::try_from(self.timeout_seconds)
                .map_err(|_| Error::PaymentProtocol("MPP timeout overflows".to_owned()))?,
        );
        if integrity.expires_at <= now
            || integrity.expires_at > maximum_expiry
            || advertised_expires_at.is_some_and(|expires_at| expires_at != integrity.expires_at)
        {
            return Err(Error::PaymentProtocol(
                "MPP challenge expiry is outside the allowed window".to_owned(),
            ));
        }
        let challenge_hash = MppWire::challenge_hash(expected_resource, &integrity.header);
        let mcp_challenge = Self::mcp_challenge_value(&integrity.official)?;
        let challenge = PaymentChallenge {
            scenario_id: scenario_id.to_owned(),
            offer_id: None,
            resource: expected_resource.to_owned(),
            protocol: MPP_PROTOCOL.to_owned(),
            asset: integrity.asset,
            amount: integrity.amount,
            recipient: integrity.recipient,
            nonce: integrity.official.id,
            challenge_hash,
            expires_at: integrity.expires_at,
            payload: json!({"www_authenticate": integrity.header, "mcp_challenge": mcp_challenge}),
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
                "MPP challenge does not match the payment intent".to_owned(),
            ));
        }
        Ok(())
    }

    async fn build_credential(&self, challenge: &PaymentChallenge, signed_payload: Value) -> Result<PaymentCredential> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        let payload: mpp::PaymentPayload = serde_json::from_value(signed_payload)
            .map_err(|error| Error::PaymentProtocol(format!("invalid MPP charge payload: {error}")))?;
        let credential = mpp::PaymentCredential::new(integrity.official.to_echo(), payload);
        let mut credential = Self::wrap_official_credential(challenge, &credential)?;
        credential.credential_hash = self.credential_fingerprint(challenge, &credential)?;
        Ok(credential)
    }

    async fn prepare_signing(
        &self,
        challenge: &PaymentChallenge,
        payer: &str,
        now: i64,
    ) -> Result<super::ProtocolSigningRequest> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        let payload = TempoSigningPayload::prepare(&integrity, payer, now)?;
        let digest = payload.validate(&integrity)?.to_vec();
        Ok(super::ProtocolSigningRequest {
            digest,
            payload: serde_json::to_value(payload)?,
        })
    }

    async fn complete_signing(
        &self,
        challenge: &PaymentChallenge,
        request: super::ProtocolSigningRequest,
        signature: &[u8],
    ) -> Result<PaymentCredential> {
        if request.digest.len() != 32 || signature.len() != 65 {
            return Err(Error::PaymentProtocol(
                "MPP Tempo signer returned invalid secp256k1 material".to_owned(),
            ));
        }
        let integrity = self.validate_challenge_integrity(challenge)?;
        let payload: TempoSigningPayload = serde_json::from_value(request.payload)
            .map_err(|error| Error::PaymentProtocol(format!("invalid MPP Tempo signing payload: {error}")))?;
        let digest = payload.validate(&integrity)?;
        if request.digest.as_slice() != digest.as_slice() {
            return Err(Error::PaymentProtocol(
                "MPP Tempo signing request digest no longer matches its transaction".to_owned(),
            ));
        }
        let signature = Signature::from_raw(signature)
            .map_err(|error| Error::PaymentProtocol(format!("invalid MPP Tempo signature: {error}")))?;
        if signature.normalize_s().is_some() {
            return Err(Error::PaymentProtocol(
                "MPP Tempo signature is not low-S canonical".to_owned(),
            ));
        }
        let recovered = signature
            .recover_address_from_prehash(&digest)
            .map_err(|error| Error::PaymentProtocol(format!("recover MPP Tempo signer: {error}")))?;
        let payer = payload
            .payer
            .parse::<Address>()
            .map_err(|_| Error::PaymentProtocol("MPP Tempo payer binding is invalid".to_owned()))?;
        if recovered != payer {
            return Err(Error::PaymentProtocol(
                "MPP Tempo signature was produced by a different wallet".to_owned(),
            ));
        }
        let chain_id = payload.transaction.chain_id;
        let transaction = payload.transaction.into_signed(signature.into());
        let transaction_bytes = transaction.encoded_2718();
        let credential = build_charge_credential(&integrity.official, &transaction_bytes, chain_id, payer);
        let mut credential = Self::wrap_official_credential(challenge, &credential)?;
        credential.credential_hash = self.credential_fingerprint(challenge, &credential)?;
        Ok(credential)
    }

    async fn verify_receipt(&self, intent: &PaymentIntent, receipt: &PaymentReceipt) -> Result<()> {
        if intent.protocol != MPP_PROTOCOL
            || receipt.protocol != MPP_PROTOCOL
            || receipt.intent_id != intent.id
            || receipt.scenario_id != intent.scenario_id
            || receipt.asset != intent.asset
            || !payment_recipients_match(&intent.asset.network, &receipt.recipient, &intent.recipient)
            || receipt.requested_amount != intent.amount
            || receipt.status != PaymentStatus::Settled
        {
            return Err(Error::PaymentProtocol("MPP receipt binding mismatch".to_owned()));
        }
        receipt
            .validate()
            .map_err(|field| Error::PaymentProtocol(format!("invalid MPP receipt field: {field}")))
    }

    async fn build_challenge(&self, offer: &PaymentPrice, recipient: &str, now: i64) -> Result<PaymentChallenge> {
        let payee = self.payee.as_ref().ok_or_else(|| {
            Error::PaymentProtocol("payer-only MPP adapter cannot issue a payee challenge".to_owned())
        })?;
        if !offer.supports_protocol(MPP_PROTOCOL) {
            return Err(Error::PaymentProtocol(
                "offer cannot produce an MPP challenge".to_owned(),
            ));
        }
        let recipient = canonicalize_wallet_address(EVM_EOA_ALGORITHM, recipient)
            .map_err(|_| Error::PaymentProtocol("offer recipient is not a valid EVM address".to_owned()))?;
        MppWire::validate_resource(&offer.resource)?;
        let configured = self
            .assets
            .iter()
            .find(|configured| {
                configured.network == offer.asset.network
                    && configured.symbol == offer.asset.asset
                    && configured.token_contract == offer.asset.token_contract
                    && configured.decimals == offer.asset.decimals
            })
            .ok_or_else(|| Error::PaymentProtocol("offer asset is not registered for MPP".to_owned()))?;
        let chain_id = offer
            .asset
            .network
            .strip_prefix("eip155:")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo network must use numeric CAIP-2".to_owned()))?;
        let request = ChargeRequest {
            amount: offer.amount.to_string(),
            currency: configured.currency.clone(),
            decimals: None,
            recipient: Some(recipient.clone()),
            description: None,
            external_id: Some(offer.id.clone()),
            method_details: Some(json!({"chainId": chain_id})),
            mppx_scope: Some(json!({"resource": offer.resource})),
        };
        let request = Base64UrlJson::from_typed(&request).map_err(MppWire::error)?;
        let expires_at = now.saturating_add(i64::try_from(self.timeout_seconds).unwrap_or(i64::MAX));
        let expires = OffsetDateTime::from_unix_timestamp(expires_at)
            .map_err(|error| Error::PaymentProtocol(format!("MPP expiry is invalid: {error}")))?
            .format(&Rfc3339)
            .map_err(|error| Error::PaymentProtocol(format!("format MPP expiry: {error}")))?;
        let issuance_id = Uuid::new_v4().to_string();
        let opaque = Base64UrlJson::from_typed(&BTreeMap::from([
            ("challenge_id", issuance_id.as_str()),
            ("offer_id", offer.id.as_str()),
            ("resource", offer.resource.as_str()),
            ("scenario_id", offer.scenario_id.as_str()),
        ]))
        .map_err(MppWire::error)?;
        let official = mpp::PaymentChallenge::with_secret_key_full(
            payee.challenge_secret.as_str(),
            &payee.realm,
            TEMPO_METHOD,
            "charge",
            request,
            Some(&expires),
            None,
            None,
            Some(opaque),
            Some("Payment-Authorization"),
        );
        let header = format_www_authenticate(&official).map_err(MppWire::error)?;
        let challenge_hash = MppWire::challenge_hash(&offer.resource, &header);
        let mcp_challenge = Self::mcp_challenge_value(&official)?;
        let challenge = PaymentChallenge {
            scenario_id: offer.scenario_id.clone(),
            offer_id: Some(offer.id.clone()),
            resource: offer.resource.clone(),
            protocol: MPP_PROTOCOL.to_owned(),
            asset: offer.asset.clone(),
            amount: offer.amount,
            recipient,
            nonce: official.id,
            challenge_hash,
            expires_at,
            payload: json!({"www_authenticate": header, "mcp_challenge": mcp_challenge}),
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
        if now >= challenge.expires_at {
            return Err(Error::PaymentProtocol("MPP challenge expired".to_owned()));
        }
        let payer = self.credential_payer(challenge, credential)?;
        Ok(ProtocolValidation {
            payer: Some(payer),
            requires_external_settlement: true,
        })
    }

    fn credential_fingerprint(&self, challenge: &PaymentChallenge, credential: &PaymentCredential) -> Result<String> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        let authorization = credential
            .payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP Authorization value is missing".to_owned()))?;
        let submitted = parse_authorization(authorization).map_err(MppWire::error)?;
        if serde_json::to_value(&submitted.challenge)? != serde_json::to_value(integrity.official.to_echo())? {
            return Err(Error::PaymentProtocol("MPP challenge echo mismatch".to_owned()));
        }
        let payload = submitted.charge_payload().map_err(MppWire::error)?;
        let payload_type = payload.payload_type().to_string();
        let payment_bytes = MppWire::decode_payment_reference(payload.data())?;
        let contract = challenge
            .asset
            .token_contract
            .as_deref()
            .unwrap_or(&challenge.asset.asset)
            .to_ascii_lowercase();
        Ok(MppWire::hash_values(&[
            b"agentvault.mpp.replay.v1",
            challenge.asset.network.as_bytes(),
            contract.as_bytes(),
            payload_type.as_bytes(),
            &payment_bytes,
        ]))
    }

    async fn settle_payment(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
    ) -> Result<ProtocolSettlement> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != MPP_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol("MPP credential binding mismatch".to_owned()));
        }
        let authorization = credential
            .payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP Authorization value is missing".to_owned()))?;
        let submitted = parse_authorization(authorization).map_err(MppWire::error)?;
        if serde_json::to_value(&submitted.challenge)? != serde_json::to_value(integrity.official.to_echo())? {
            return Err(Error::PaymentProtocol("MPP challenge echo mismatch".to_owned()));
        }
        self.credential_payer(challenge, credential)?;
        Ok(ProtocolSettlement {
            protocol: MPP_PROTOCOL.to_owned(),
            payment_payload: credential.payload.clone(),
            payment_requirements: json!({"www_authenticate": integrity.header}),
        })
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/mpp_test.rs"]
mod tests;
