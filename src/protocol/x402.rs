//! x402 v2 boundary for EVM USDC `exact` payments.
//!
//! This module validates and translates the v2 wire DTO. EIP-3009 signing and
//! cryptographic verification are delegated to the signer and an external
//! facilitator; `AgentVault` does not duplicate a facilitator implementation.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{eip712_domain, SolStruct as _};
use async_trait::async_trait;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use rand_core::{OsRng, RngCore as _};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use sha3::Keccak256;
use x402_chain_eip155::{
    chain::{AssetTransferMethod, ChecksummedAddress, DecimalU256, Eip155ChainReference},
    v1_eip155_exact::{
        ExactEvmPayload as Eip3009Payload, ExactEvmPayloadAuthorization, ExactScheme, TransferWithAuthorization,
    },
    v2_eip155_exact::{ExactEvmPayload, PaymentPayload as OfficialPaymentPayload, PaymentRequirements},
};
use x402_types::{
    proto::v2::{PaymentRequired as OfficialPaymentRequired, ResourceInfo, X402Version2},
    timestamp::UnixTimestamp,
};

use crate::{
    model::{
        canonicalize_wallet_address, payment_recipients_match, Amount, AssetIdentity, PaymentChallenge,
        PaymentCredential, PaymentIntent, PaymentOffer, PaymentReceipt, PaymentStatus, EVM_EOA_ALGORITHM,
    },
    Error, Result,
};

use super::{PaymentProtocol, ProtocolSettlement, ProtocolSigningRequest, ProtocolValidation};

const X402_PROTOCOL: &str = "x402";
const EXACT_SCHEME: &str = "exact";
// Match the official x402 EVM client. RPC "latest" block time can lag wall
// clock time (notably on development chains that mine only on transactions),
// so a one-second backdating window is not sufficient for immediate
// facilitator simulation.
const EIP3009_VALID_AFTER_SKEW_SECONDS: u64 = 10 * 60;

type X402PaymentRequired = OfficialPaymentRequired<PaymentRequirements>;
type X402PaymentPayload = OfficialPaymentPayload<PaymentRequirements>;
type X402Requirement = PaymentRequirements;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X402Asset {
    pub network: String,
    pub contract: String,
    pub symbol: String,
    pub decimals: u8,
}

/// Registry-backed adapter. Asset decimals and token identity are local trusted
/// configuration, never values accepted from a merchant challenge.
pub struct X402Adapter {
    assets: Vec<X402Asset>,
    max_timeout_seconds: u64,
}

impl X402Adapter {
    /// Creates an EVM/USDC/exact adapter.
    ///
    /// # Errors
    ///
    /// Rejects ambiguous assets and unsafe timeout bounds.
    pub fn new(assets: Vec<X402Asset>, max_timeout_seconds: u64) -> Result<Self> {
        if assets.is_empty() || max_timeout_seconds == 0 || max_timeout_seconds > 3_600 {
            return Err(Error::Config("x402 asset registry or timeout is invalid".to_owned()));
        }
        for asset in &assets {
            if !valid_evm_network(&asset.network)
                || !valid_address(&asset.contract)
                || !asset.symbol.eq_ignore_ascii_case("USDC")
                || asset.decimals != 6
            {
                return Err(Error::Config(
                    "x402 v2 currently supports only registered EVM USDC assets".to_owned(),
                ));
            }
        }
        Ok(Self {
            assets,
            max_timeout_seconds,
        })
    }

    fn registered_asset(&self, network: &str, contract: &str) -> Result<AssetIdentity> {
        let registered = self
            .assets
            .iter()
            .find(|asset| asset.network == network && asset.contract.eq_ignore_ascii_case(contract))
            .ok_or_else(|| Error::PaymentProtocol("x402 asset or network is not registered".to_owned()))?;
        Ok(AssetIdentity {
            network: registered.network.clone(),
            asset: registered.symbol.clone(),
            token_contract: Some(registered.contract.clone()),
            decimals: registered.decimals,
        })
    }

    fn requirement<'a>(&self, wire: &'a X402PaymentRequired) -> Result<&'a X402Requirement> {
        wire.accepts
            .iter()
            .find(|requirement| {
                requirement.scheme.as_ref() == EXACT_SCHEME
                    && self.assets.iter().any(|asset| {
                        asset.network == requirement.network.to_string()
                            && asset.contract.eq_ignore_ascii_case(&requirement.asset.to_string())
                    })
            })
            .ok_or_else(|| Error::PaymentProtocol("no supported x402 exact requirement".to_owned()))
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

        let selected = match stored_selected {
            Some(selected) => serde_json::from_value::<X402Requirement>(selected.clone())
                .map_err(|error| Error::PaymentProtocol(format!("invalid stored x402 requirement: {error}")))?,
            None => self.requirement(&payment_required)?.clone(),
        };
        if !payment_required
            .accepts
            .iter()
            .any(|requirement| requirement == &selected)
        {
            return Err(Error::PaymentProtocol(
                "x402 selected requirement is not advertised by PaymentRequired".to_owned(),
            ));
        }
        validate_requirement(&selected, self.max_timeout_seconds)?;

        let asset = self.registered_asset(&selected.network.to_string(), &selected.asset.to_string())?;
        let amount = amount_from_u256(selected.amount.0)?;
        let recipient = canonicalize_wallet_address(EVM_EOA_ALGORITHM, &selected.pay_to.to_string())
            .map_err(|_| Error::PaymentProtocol("x402 recipient is not a valid EVM address".to_owned()))?;
        Ok(X402ChallengeIntegrity {
            payment_required,
            selected,
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
        let outer_recipient = canonicalize_wallet_address(EVM_EOA_ALGORITHM, &challenge.recipient)
            .map_err(|_| Error::PaymentProtocol("x402 outer recipient is not a valid EVM address".to_owned()))?;

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
}

struct X402ChallengeIntegrity {
    payment_required: X402PaymentRequired,
    selected: X402Requirement,
    asset: AssetIdentity,
    amount: Amount,
    recipient: String,
}

#[async_trait]
impl PaymentProtocol for X402Adapter {
    fn name(&self) -> &'static str {
        X402_PROTOCOL
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
        let timeout = i64::try_from(integrity.selected.max_timeout_seconds)
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
                "selected": integrity.selected,
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
        let payload = decode_payment_payload(&signed_payload)?;
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
        let payload = serde_json::to_value(X402PaymentPayload {
            accepted: integrity.selected,
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
            resource: integrity.payment_required.resource,
            x402_version: X402Version2,
            extensions: None,
        })?;
        let digest = eip3009_digest(&payload)?.to_vec();
        Ok(ProtocolSigningRequest { digest, payload })
    }

    async fn complete_signing(
        &self,
        challenge: &PaymentChallenge,
        mut request: ProtocolSigningRequest,
        signature: &[u8],
    ) -> Result<PaymentCredential> {
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
        let mut payload = decode_payment_payload(&request.payload)?;
        eip3009_payload_mut(&mut payload)?.signature = Bytes::copy_from_slice(signature);
        request.payload = serde_json::to_value(payload)?;
        let recovered = verify_eip3009_signature(&request.payload)?;
        if !recovered.eq_ignore_ascii_case(&payer_from_payload(&request.payload)?) {
            return Err(Error::PaymentProtocol(
                "x402 signature was produced by a different wallet".to_owned(),
            ));
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
        let recipient = canonicalize_wallet_address(EVM_EOA_ALGORITHM, recipient)
            .map_err(|_| Error::PaymentProtocol("offer recipient is not a valid EVM address".to_owned()))?;
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
        let requirement = X402Requirement {
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
        };
        let expires_at = now.saturating_add(i64::try_from(self.max_timeout_seconds).unwrap_or(i64::MAX));
        let required = X402PaymentRequired {
            x402_version: X402Version2,
            error: None,
            resource: Some(ResourceInfo {
                url: offer.resource.clone(),
                description: None,
                mime_type: None,
            }),
            accepts: vec![requirement.clone()],
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
            payload: json!({"payment_required": wire, "selected": requirement}),
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
        let payload = decode_payment_payload(&credential.payload)?;
        validate_payload(challenge, &integrity, &payload, Some(now))?;
        Ok(ProtocolValidation {
            payer: Some(eip3009_payload(&payload)?.authorization.from.to_string()),
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
        let payload = decode_payment_payload(&credential.payload)?;
        validate_payload(challenge, &integrity, &payload, None)?;
        let contract = challenge
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("x402 asset has no token contract".to_owned()))?
            .to_ascii_lowercase();
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

    async fn settle_payment(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
    ) -> Result<ProtocolSettlement> {
        let integrity = self.validate_challenge_integrity(challenge)?;
        if credential.protocol != X402_PROTOCOL || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentProtocol("x402 credential binding mismatch".to_owned()));
        }
        let payload = decode_payment_payload(&credential.payload)?;
        validate_payload(challenge, &integrity, &payload, None)?;
        Ok(ProtocolSettlement {
            protocol: X402_PROTOCOL.to_owned(),
            payment_payload: credential.payload.clone(),
            payment_requirements: serde_json::to_value(integrity.selected)?,
        })
    }
}

fn validate_requirement(requirement: &X402Requirement, max_timeout: u64) -> Result<()> {
    if requirement.scheme.as_ref() != EXACT_SCHEME
        || !valid_evm_network(&requirement.network.to_string())
        || requirement.max_timeout_seconds == 0
        || requirement.max_timeout_seconds > max_timeout
        || !matches!(
            &requirement.extra,
            AssetTransferMethod::Eip3009 { name, version } if name == "USDC" && version == "2"
        )
        || requirement.amount.0.is_zero()
    {
        return Err(Error::PaymentProtocol(
            "unsupported or unsafe x402 requirement".to_owned(),
        ));
    }
    Ok(())
}

fn validate_payload(
    challenge: &PaymentChallenge,
    integrity: &X402ChallengeIntegrity,
    payload: &X402PaymentPayload,
    now: Option<i64>,
) -> Result<()> {
    let authorization = &eip3009_payload(payload)?.authorization;
    let valid_after = i64::try_from(authorization.valid_after.as_secs())
        .map_err(|_| Error::PaymentProtocol("x402 validAfter is invalid".to_owned()))?;
    let valid_before = i64::try_from(authorization.valid_before.as_secs())
        .map_err(|_| Error::PaymentProtocol("x402 validBefore is invalid".to_owned()))?;
    if payload.resource.as_ref().is_some_and(|resource| {
        !resource_matches(resource, integrity.payment_required.resource.as_ref()) || resource.url != challenge.resource
    }) || payload.accepted != integrity.selected
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
    Ok(())
}

fn resource_matches(resource: &ResourceInfo, expected: Option<&ResourceInfo>) -> bool {
    expected.is_some_and(|expected| {
        resource.url == expected.url
            && resource.description == expected.description
            && resource.mime_type == expected.mime_type
    })
}

fn eip3009_payload(payload: &X402PaymentPayload) -> Result<&Eip3009Payload> {
    match &payload.payload {
        ExactEvmPayload::Eip3009(payload) => Ok(payload),
        ExactEvmPayload::Permit2(_) => Err(Error::PaymentProtocol(
            "x402 exact currently supports only EIP-3009".to_owned(),
        )),
    }
}

fn eip3009_payload_mut(payload: &mut X402PaymentPayload) -> Result<&mut Eip3009Payload> {
    match &mut payload.payload {
        ExactEvmPayload::Eip3009(payload) => Ok(payload),
        ExactEvmPayload::Permit2(_) => Err(Error::PaymentProtocol(
            "x402 exact currently supports only EIP-3009".to_owned(),
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Eip3009AuthorizationIdentity {
    pub payer: String,
    pub nonce: String,
    pub valid_before: i64,
}

pub(crate) fn eip3009_authorization_identity(payload: &Value) -> Result<Eip3009AuthorizationIdentity> {
    let payload = decode_payment_payload(payload)?;
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
    let payload = decode_payment_payload(payload)?;
    let requirement = &payload.accepted;
    validate_requirement(requirement, u64::MAX)?;
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
    let decoded = decode_payment_payload(payload)?;
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

fn payer_from_payload(payload: &Value) -> Result<String> {
    let payload = decode_payment_payload(payload)?;
    Ok(eip3009_payload(&payload)?.authorization.from.to_string())
}

fn decode_payment_payload(value: &Value) -> Result<X402PaymentPayload> {
    validate_payment_payload_wire(value)?;
    serde_json::from_value(value.clone())
        .map_err(|error| Error::PaymentProtocol(format!("invalid x402 payment payload: {error}")))
}

/// Validates the exact official v2 EIP-3009 payload shape before typed
/// deserialization. The upstream official types intentionally tolerate
/// unknown JSON fields, while a signed payment boundary must fail closed.
pub(crate) fn validate_payment_payload_wire(value: &Value) -> Result<()> {
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

fn exact_object_fields(value: &Value, required: &[&str], allowed: &[&str], name: &str) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::PaymentProtocol(format!("x402 {name} must be an object")))?;
    if required.iter().any(|field| !object.contains_key(*field))
        || object.keys().any(|field| !allowed.contains(&field.as_str()))
    {
        return Err(Error::PaymentProtocol(format!(
            "x402 {name} fields do not match the official EIP-3009 wire"
        )));
    }
    Ok(())
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

fn valid_address(value: &str) -> bool {
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
