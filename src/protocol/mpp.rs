//! MPP/Tempo `charge` adapter built on the official `mpp` Rust crate.

use std::collections::BTreeMap;

use async_trait::async_trait;
use mpp::{
    format_authorization, format_www_authenticate, parse_authorization, parse_www_authenticate, Base64UrlJson,
    ChargeRequest,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    model::{
        canonicalize_wallet_address, payment_recipients_match, Amount, AssetIdentity, PaymentChallenge,
        PaymentCredential, PaymentIntent, PaymentOffer, PaymentReceipt, PaymentStatus, EVM_EOA_ALGORITHM,
    },
    Error, Result,
};

use super::{PaymentProtocol, ProtocolSettlement, ProtocolValidation};

const MPP_PROTOCOL: &str = "mpp";
const TEMPO_METHOD: &str = "tempo";

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
        if assets.is_empty()
            || timeout_seconds == 0
            || timeout_seconds > 3_600
            || assets.iter().any(|asset| {
                !valid_evm_network(&asset.network) || asset.currency.trim().is_empty() || asset.symbol.trim().is_empty()
            })
        {
            return Err(Error::Config("MPP Tempo adapter configuration is invalid".to_owned()));
        }
        Ok(Self {
            assets,
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
        validate_resource(expected_resource)?;
        let official = parse_www_authenticate(header).map_err(mpp_error)?;
        if !official.method.eq_ignore_ascii_case(TEMPO_METHOD) || !official.intent.is_charge() {
            return Err(Error::PaymentProtocol("MPP challenge is not a Tempo charge".to_owned()));
        }
        let request: ChargeRequest = official.request.decode().map_err(mpp_error)?;
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

        let network = tempo_network(&request)?;
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
            header: header.to_owned(),
            asset,
            amount,
            recipient,
            expires_at,
        })
    }

    /// Decodes the stored header through the official SDK and treats the
    /// protocol-neutral fields only as a checked projection of that wire data.
    fn validate_challenge_integrity(&self, challenge: &PaymentChallenge) -> Result<MppChallengeIntegrity> {
        ensure_protocol(challenge)?;
        let header = challenge
            .payload
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("stored MPP challenge is missing".to_owned()))?;
        let integrity = self.decode_challenge_header(&challenge.resource, header)?;
        let expected_hash = mpp_challenge_hash(&challenge.resource, header);
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
                .map_err(mpp_error)?;
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
        let challenge_hash = mpp_challenge_hash(expected_resource, header);
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
            payload: json!({"www_authenticate": header}),
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
        let authorization = format_authorization(&credential).map_err(mpp_error)?;
        Ok(PaymentCredential {
            protocol: MPP_PROTOCOL.to_owned(),
            challenge_hash: challenge.challenge_hash.clone(),
            credential_hash: hash_values(&[
                b"agentvault.mpp.credential.v1",
                challenge.challenge_hash.as_bytes(),
                authorization.as_bytes(),
            ]),
            payload: json!({"authorization": authorization}),
        })
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

    async fn build_challenge(&self, offer: &PaymentOffer, recipient: &str, now: i64) -> Result<PaymentChallenge> {
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
        validate_resource(&offer.resource)?;
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
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo network must use CAIP-2".to_owned()))?;
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
        let request = Base64UrlJson::from_typed(&request).map_err(mpp_error)?;
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
        .map_err(mpp_error)?;
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
            None,
        );
        let header = format_www_authenticate(&official).map_err(mpp_error)?;
        let challenge_hash = mpp_challenge_hash(&offer.resource, &header);
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
            payload: json!({"www_authenticate": header}),
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
        if credential.protocol != MPP_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol(
                "MPP credential is bound to another challenge".to_owned(),
            ));
        }
        if now >= challenge.expires_at {
            return Err(Error::PaymentProtocol("MPP challenge expired".to_owned()));
        }
        let authorization = credential
            .payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::PaymentProtocol("MPP Authorization value is missing".to_owned()))?;
        let submitted = parse_authorization(authorization).map_err(mpp_error)?;
        let expected = integrity.official.to_echo();
        if serde_json::to_value(&submitted.challenge)? != serde_json::to_value(expected)? {
            return Err(Error::PaymentProtocol("MPP challenge echo mismatch".to_owned()));
        }
        let _ = submitted.charge_payload().map_err(mpp_error)?;
        Ok(ProtocolValidation {
            payer: submitted.source,
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
        let submitted = parse_authorization(authorization).map_err(mpp_error)?;
        if serde_json::to_value(&submitted.challenge)? != serde_json::to_value(integrity.official.to_echo())? {
            return Err(Error::PaymentProtocol("MPP challenge echo mismatch".to_owned()));
        }
        let payload = submitted.charge_payload().map_err(mpp_error)?;
        let payload_type = payload.payload_type().to_string();
        let payment_bytes = decode_payment_reference(payload.data())?;
        let contract = challenge
            .asset
            .token_contract
            .as_deref()
            .unwrap_or(&challenge.asset.asset)
            .to_ascii_lowercase();
        Ok(hash_values(&[
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
        let submitted = parse_authorization(authorization).map_err(mpp_error)?;
        if serde_json::to_value(&submitted.challenge)? != serde_json::to_value(integrity.official.to_echo())? {
            return Err(Error::PaymentProtocol("MPP challenge echo mismatch".to_owned()));
        }
        let _ = submitted.charge_payload().map_err(mpp_error)?;
        Ok(ProtocolSettlement {
            protocol: MPP_PROTOCOL.to_owned(),
            payment_payload: credential.payload.clone(),
            payment_requirements: json!({"www_authenticate": integrity.header}),
        })
    }
}

#[cfg(test)]
fn official_challenge(challenge: &PaymentChallenge) -> Result<mpp::PaymentChallenge> {
    let header = challenge
        .payload
        .get("www_authenticate")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::PaymentProtocol("stored MPP challenge is missing".to_owned()))?;
    parse_www_authenticate(header).map_err(mpp_error)
}

fn tempo_network(request: &ChargeRequest) -> Result<String> {
    let details = request
        .method_details
        .as_ref()
        .and_then(Value::as_object)
        .filter(|details| details.len() == 1 && details.contains_key("chainId"))
        .ok_or_else(|| {
            Error::PaymentProtocol(
                "MPP Tempo methodDetails must contain only chainId; splits and delegated fees are unsupported"
                    .to_owned(),
            )
        })?;
    let chain_id = details
        .get("chainId")
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|value| value.to_string()))
        })
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| Error::PaymentProtocol("MPP Tempo chainId is missing or invalid".to_owned()))?;
    Ok(format!("eip155:{chain_id}"))
}

fn ensure_protocol(challenge: &PaymentChallenge) -> Result<()> {
    if challenge.protocol == MPP_PROTOCOL {
        Ok(())
    } else {
        Err(Error::PaymentProtocol("protocol adapter mismatch".to_owned()))
    }
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

fn decode_payment_reference(value: &str) -> Result<Vec<u8>> {
    value
        .strip_prefix("0x")
        .filter(|encoded| !encoded.is_empty() && encoded.len() % 2 == 0)
        .ok_or_else(|| Error::PaymentProtocol("MPP payment reference is not canonical hex".to_owned()))
        .and_then(|encoded| {
            hex::decode(encoded)
                .map_err(|_| Error::PaymentProtocol("MPP payment reference is not canonical hex".to_owned()))
        })
}

fn mpp_challenge_hash(resource: &str, header: &str) -> String {
    hash_values(&[b"agentvault.mpp.challenge.v1", resource.as_bytes(), header.as_bytes()])
}

fn hash_values(values: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for value in values {
        hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(value);
    }
    hex::encode(hash.finalize())
}

#[allow(clippy::needless_pass_by_value)]
fn mpp_error(error: mpp::MppError) -> Error {
    Error::PaymentProtocol(error.to_string())
}

#[cfg(test)]
#[path = "../../tests/protocol/mpp_test.rs"]
mod tests;
