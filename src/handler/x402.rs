//! Official x402 v2 facilitator HTTP adapter.
//!
//! The adapter speaks the standard `/verify` and `/settle` envelope instead
//! of `AgentVault`'s internal provider DTO. Financial evidence is reconstructed
//! only from an operator-owned asset registry and the validated
//! `paymentRequirements`; a facilitator response cannot change the asset,
//! amount, network, or recipient that is posted to the ledger.

use std::{collections::BTreeMap, str::FromStr as _};

use async_trait::async_trait;
use reqwest::{header::HeaderMap, StatusCode};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use solana_signature::Signature as SolanaSignature;
use x402_chain_eip155::{
    chain::AssetTransferMethod,
    v2_eip155_exact::{
        ExactEvmPayload, PaymentPayload as OfficialPaymentPayload, PaymentRequirements as OfficialPaymentRequirements,
    },
};
use x402_chain_solana::{
    chain::SolanaChainReference,
    v2_solana_exact::{
        PaymentPayload as OfficialSolanaPaymentPayload, PaymentRequirements as OfficialSolanaPaymentRequirements,
    },
};
use x402_types::chain::ChainId;
use x402_types::proto::v2::{
    SettleResponse as OfficialSettleResponse, VerifyRequest as OfficialVerifyRequest,
    VerifyResponse as OfficialVerifyResponse, X402Version2,
};

use crate::{
    model::{canonicalize_payment_recipient, payment_recipients_match, Amount, AssetIdentity},
    protocol::x402::{
        parse_settle_response, parse_verify_response, solana_payment_payer, validate_evm_payment_payload_wire,
        validate_solana_payment_payload_wire, X402Asset,
    },
    Error, Result,
};

use super::{
    provider::{FixedHttpJsonTransport, ProviderHttpEndpoint, ProviderHttpResponse},
    ExternalSettlementProvider, SettlementOperation, SettlementProviderConfig, SettlementProviderRequest,
    SettlementProviderResult,
};

const EXACT_SCHEME: &str = "exact";

/// Remote x402 v2 facilitator backed by the official HTTP wire contract.
pub struct X402FacilitatorProvider {
    transport: FixedHttpJsonTransport,
    assets: Vec<TrustedAsset>,
}

#[derive(Clone)]
struct TrustedAsset {
    network: String,
    contract: String,
    symbol: String,
    decimals: u8,
}

impl X402FacilitatorProvider {
    /// Creates an x402 facilitator client with a bounded trusted asset table.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe HTTP configuration, an empty/ambiguous
    /// registry, or an asset outside the supported v2 EVM/Solana USDC exact
    /// profiles.
    pub fn new(config: &SettlementProviderConfig, headers: HeaderMap, assets: Vec<X402Asset>) -> Result<Self> {
        if assets.is_empty() {
            return Err(Error::Config("x402 facilitator asset registry is empty".to_owned()));
        }
        let mut trusted = Vec::with_capacity(assets.len());
        for asset in assets {
            let contract = canonicalize_payment_recipient(&asset.network, &asset.contract)
                .map_err(|_| Error::Config("x402 facilitator asset contract is invalid".to_owned()))?;
            if !(is_evm_network(&asset.network) || is_solana_network(&asset.network))
                || !asset.symbol.eq_ignore_ascii_case("USDC")
                || asset.decimals != 6
                || trusted.iter().any(|prior: &TrustedAsset| {
                    prior.network == asset.network
                        && payment_recipients_match(&asset.network, &prior.contract, &contract)
                })
            {
                return Err(Error::Config(
                    "x402 facilitator assets must be unique EVM or Solana USDC registrations".to_owned(),
                ));
            }
            trusted.push(TrustedAsset {
                network: asset.network,
                contract,
                symbol: asset.symbol,
                decimals: asset.decimals,
            });
        }
        Ok(Self {
            transport: FixedHttpJsonTransport::new(config, headers)?,
            assets: trusted,
        })
    }

    fn validate_request(&self, request: &SettlementProviderRequest) -> Result<ValidatedPayment> {
        if request.protocol != "x402" {
            return Err(Error::Settlement(
                "x402 facilitator received a non-x402 provider request".to_owned(),
            ));
        }
        let network = request
            .payment_requirements
            .get("network")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Settlement("x402 paymentRequirements network is missing".to_owned()))?;
        if is_evm_network(network) {
            return self.validate_evm_request(request);
        }
        if is_solana_network(network) {
            return self.validate_solana_request(request);
        }
        Err(Error::Settlement(
            "x402 facilitator received an unsupported payment network".to_owned(),
        ))
    }

    fn validate_evm_request(&self, request: &SettlementProviderRequest) -> Result<ValidatedPayment> {
        let requirements: OfficialPaymentRequirements = serde_json::from_value(request.payment_requirements.clone())
            .map_err(|error| Error::Settlement(format!("invalid x402 paymentRequirements: {error}")))?;
        let network = requirements.network.to_string();
        if requirements.scheme.as_ref() != EXACT_SCHEME
            || !is_evm_network(&network)
            || !matches!(
                &requirements.extra,
                AssetTransferMethod::Eip3009 { name, version } if name == "USDC" && version == "2"
            )
        {
            return Err(Error::Settlement(
                "x402 facilitator supports only v2 EVM exact requirements".to_owned(),
            ));
        }
        let contract = canonicalize_payment_recipient(&network, &requirements.asset.to_string())
            .map_err(|_| Error::Settlement("x402 paymentRequirements asset is invalid".to_owned()))?;
        let registered = self.trusted_asset(&network, &contract)?;
        let amount = Amount::new(
            u128::try_from(requirements.amount.0)
                .map_err(|_| Error::Settlement("x402 paymentRequirements amount is invalid".to_owned()))?,
        );
        if amount.is_zero() || requirements.max_timeout_seconds == 0 {
            return Err(Error::Settlement(
                "x402 paymentRequirements amount or timeout is invalid".to_owned(),
            ));
        }
        let recipient = canonicalize_payment_recipient(&network, &requirements.pay_to.to_string())
            .map_err(|_| Error::Settlement("x402 paymentRequirements payTo is invalid".to_owned()))?;
        validate_evm_payment_payload_wire(&request.payment_payload)?;
        let payment_payload: OfficialPaymentPayload = serde_json::from_value(request.payment_payload.clone())
            .map_err(|error| Error::Settlement(format!("invalid x402 paymentPayload: {error}")))?;
        if payment_payload.accepted != requirements {
            return Err(Error::Settlement(
                "x402 paymentPayload does not bind the supplied paymentRequirements".to_owned(),
            ));
        }
        let eip3009 = match &payment_payload.payload {
            ExactEvmPayload::Eip3009(payload) => payload,
            ExactEvmPayload::Permit2(_) => {
                return Err(Error::Settlement(
                    "x402 facilitator supports only EIP-3009 payloads".to_owned(),
                ))
            }
        };
        let payer = canonicalize_payment_recipient(&network, &eip3009.authorization.from.to_string())
            .map_err(|_| Error::Settlement("x402 paymentPayload payer is invalid".to_owned()))?;
        let authorized_recipient = canonicalize_payment_recipient(&network, &eip3009.authorization.to.to_string())
            .map_err(|_| Error::Settlement("x402 paymentPayload authorization recipient is invalid".to_owned()))?;
        if authorized_recipient != recipient || eip3009.authorization.value != requirements.amount.0 {
            return Err(Error::Settlement(
                "x402 paymentPayload authorization does not match paymentRequirements".to_owned(),
            ));
        }
        if eip3009.signature.len() != 65
            || eip3009.authorization.valid_after.as_secs() >= eip3009.authorization.valid_before.as_secs()
        {
            return Err(Error::Settlement(
                "x402 paymentPayload contains malformed EIP-3009 material".to_owned(),
            ));
        }
        Ok(ValidatedPayment {
            asset: AssetIdentity {
                network: registered.network.clone(),
                asset: registered.symbol.clone(),
                token_contract: Some(registered.contract.clone()),
                decimals: registered.decimals,
            },
            amount,
            recipient,
            payer,
            payment_payload: request.payment_payload.clone(),
            payment_requirements: request.payment_requirements.clone(),
        })
    }

    fn validate_solana_request(&self, request: &SettlementProviderRequest) -> Result<ValidatedPayment> {
        let requirements: OfficialSolanaPaymentRequirements =
            serde_json::from_value(request.payment_requirements.clone())
                .map_err(|error| Error::Settlement(format!("invalid x402 Solana paymentRequirements: {error}")))?;
        let network = requirements.network.to_string();
        if requirements.scheme.as_ref() != EXACT_SCHEME || !is_solana_network(&network) {
            return Err(Error::Settlement(
                "x402 facilitator supports only v2 Solana exact requirements".to_owned(),
            ));
        }
        let contract = canonicalize_payment_recipient(&network, &requirements.asset.to_string())
            .map_err(|_| Error::Settlement("x402 Solana paymentRequirements asset is invalid".to_owned()))?;
        let registered = self.trusted_asset(&network, &contract)?;
        let amount = Amount::new(u128::from(requirements.amount.inner()));
        if amount.is_zero() || requirements.max_timeout_seconds == 0 {
            return Err(Error::Settlement(
                "x402 Solana paymentRequirements amount or timeout is invalid".to_owned(),
            ));
        }
        let recipient = canonicalize_payment_recipient(&network, &requirements.pay_to.to_string())
            .map_err(|_| Error::Settlement("x402 Solana paymentRequirements payTo is invalid".to_owned()))?;
        validate_solana_payment_payload_wire(&request.payment_payload)?;
        let payment_payload: OfficialSolanaPaymentPayload = serde_json::from_value(request.payment_payload.clone())
            .map_err(|error| Error::Settlement(format!("invalid x402 Solana paymentPayload: {error}")))?;
        if payment_payload.accepted != requirements {
            return Err(Error::Settlement(
                "x402 Solana paymentPayload does not bind the supplied paymentRequirements".to_owned(),
            ));
        }
        let payer = canonicalize_payment_recipient(&network, &solana_payment_payer(&request.payment_payload)?)
            .map_err(|_| Error::Settlement("x402 Solana paymentPayload payer is invalid".to_owned()))?;
        Ok(ValidatedPayment {
            asset: AssetIdentity {
                network: registered.network.clone(),
                asset: registered.symbol.clone(),
                token_contract: Some(registered.contract.clone()),
                decimals: registered.decimals,
            },
            amount,
            recipient,
            payer,
            payment_payload: request.payment_payload.clone(),
            payment_requirements: request.payment_requirements.clone(),
        })
    }

    fn trusted_asset(&self, network: &str, contract: &str) -> Result<&TrustedAsset> {
        self.assets
            .iter()
            .find(|asset| asset.network == network && payment_recipients_match(network, &asset.contract, contract))
            .ok_or_else(|| Error::Settlement("x402 paymentRequirements asset is not registered".to_owned()))
    }

    async fn call(
        &self,
        operation: SettlementOperation,
        request: &SettlementProviderRequest,
        payment: &ValidatedPayment,
    ) -> Result<ProviderHttpResponse> {
        let endpoint = match operation {
            SettlementOperation::Verify => ProviderHttpEndpoint::Verify,
            SettlementOperation::Settle => ProviderHttpEndpoint::Settle,
        };
        let wire = OfficialVerifyRequest {
            x402_version: X402Version2,
            payment_payload: payment.payment_payload.clone(),
            payment_requirements: payment.payment_requirements.clone(),
        };
        let response = self.transport.post_json(endpoint, &request.run_id, &wire).await?;
        tracing::info!(
            event.name = match operation {
                SettlementOperation::Verify => "agentvault.facilitator.verify.completed",
                SettlementOperation::Settle => "agentvault.facilitator.settle.completed",
            },
            run.id = %request.run_id,
            http.response.status_code = response.status.as_u16(),
            "completed the configured x402 facilitator operation"
        );
        if !accepted_facilitator_status(response.status) {
            let reason = safe_facilitator_reason(&response.body)
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default();
            return Err(Error::Settlement(format!(
                "x402 facilitator returned HTTP {}{reason}",
                response.status,
            )));
        }
        Ok(response)
    }

    fn parse_verify(response: &[u8], payment: &ValidatedPayment) -> Result<SettlementProviderResult> {
        let response: OfficialVerifyResponse = parse_verify_response(response)?;
        let outcome = match response {
            OfficialVerifyResponse::Valid { payer } => ProviderOutcome {
                valid: true,
                settled: false,
                payer: validate_response_payer(Some(payer), &payment.payer, &payment.asset.network)?,
                transaction: None,
                invalid_reason: None,
                effect_ref: None,
                confirmations: None,
            },
            OfficialVerifyResponse::Invalid { reason, payer } => {
                if reason.is_empty() {
                    return Err(Error::Settlement(
                        "x402 /verify failure response has no invalidReason".to_owned(),
                    ));
                }
                ProviderOutcome {
                    valid: false,
                    settled: false,
                    payer: validate_response_payer(payer, &payment.payer, &payment.asset.network)?,
                    transaction: None,
                    invalid_reason: Some(reason),
                    effect_ref: None,
                    confirmations: None,
                }
            }
        };
        Ok(provider_result(payment, outcome))
    }

    fn parse_settle(response: &[u8], payment: &ValidatedPayment) -> Result<SettlementProviderResult> {
        let response: OfficialSettleResponse = parse_settle_response(response)?;
        match response {
            OfficialSettleResponse::Success {
                payer,
                transaction,
                network,
            } => {
                if network != payment.asset.network
                    || !valid_settlement_transaction(&payment.asset.network, &transaction)
                {
                    return Err(Error::Settlement(
                        "x402 /settle success response conflicts with paymentRequirements".to_owned(),
                    ));
                }
                let payer = validate_response_payer(Some(payer), &payment.payer, &payment.asset.network)?;
                let effect_ref = stable_effect_ref(&payment.asset, payment.amount, &payment.recipient, &transaction);
                Ok(provider_result(
                    payment,
                    ProviderOutcome {
                        valid: true,
                        settled: true,
                        payer,
                        transaction: Some(transaction),
                        invalid_reason: None,
                        effect_ref: Some(effect_ref),
                        confirmations: Some(1),
                    },
                ))
            }
            OfficialSettleResponse::Error { reason, network } => {
                if network != payment.asset.network || reason.is_empty() {
                    return Err(Error::Settlement(
                        "x402 /settle error response conflicts with paymentRequirements".to_owned(),
                    ));
                }
                let remains_valid = reason == "settlement_pending";
                Ok(provider_result(
                    payment,
                    ProviderOutcome {
                        valid: remains_valid,
                        settled: false,
                        payer: None,
                        transaction: None,
                        invalid_reason: Some(reason),
                        effect_ref: None,
                        confirmations: None,
                    },
                ))
            }
        }
    }
}

#[async_trait]
impl ExternalSettlementProvider for X402FacilitatorProvider {
    async fn execute(
        &self,
        operation: SettlementOperation,
        request: &SettlementProviderRequest,
    ) -> Result<SettlementProviderResult> {
        let payment = self.validate_request(request)?;
        let response = self.call(operation, request, &payment).await?;
        match operation {
            SettlementOperation::Verify => Self::parse_verify(&response.body, &payment),
            SettlementOperation::Settle => Self::parse_settle(&response.body, &payment),
        }
    }
}

struct ValidatedPayment {
    asset: AssetIdentity,
    amount: Amount,
    recipient: String,
    payer: String,
    payment_payload: Value,
    payment_requirements: Value,
}

struct ProviderOutcome {
    valid: bool,
    settled: bool,
    payer: Option<String>,
    transaction: Option<String>,
    invalid_reason: Option<String>,
    effect_ref: Option<String>,
    confirmations: Option<u64>,
}

fn provider_result(payment: &ValidatedPayment, outcome: ProviderOutcome) -> SettlementProviderResult {
    SettlementProviderResult {
        valid: outcome.valid,
        settled: outcome.settled,
        payer: outcome.payer,
        transaction: outcome.transaction,
        settlement_ref: None,
        effect_ref: outcome.effect_ref,
        invalid_reason: outcome.invalid_reason,
        asset: Some(payment.asset.clone()),
        amount: Some(payment.amount),
        recipient: Some(payment.recipient.clone()),
        confirmations: outcome.confirmations,
        extensions: BTreeMap::new(),
    }
}

fn validate_response_payer(payer: Option<String>, expected: &str, network: &str) -> Result<Option<String>> {
    payer
        .map(|payer| {
            let payer = canonicalize_payment_recipient(network, &payer)
                .map_err(|_| Error::Settlement("x402 facilitator payer is invalid".to_owned()))?;
            if payer != expected {
                return Err(Error::Settlement(
                    "x402 facilitator payer does not match paymentPayload".to_owned(),
                ));
            }
            Ok(payer)
        })
        .transpose()
}

fn accepted_facilitator_status(status: StatusCode) -> bool {
    status.is_success() || status == StatusCode::BAD_REQUEST || status == StatusCode::PRECONDITION_FAILED
}

/// Extracts only a bounded, low-entropy protocol reason from an error body.
/// Detailed provider messages can contain calldata or signatures and are
/// deliberately excluded from application errors and logs.
fn safe_facilitator_reason(body: &[u8]) -> Option<String> {
    const SAFE_KEYS: [&str; 4] = ["invalidReason", "errorReason", "reason", "message"];
    const MAX_REASON_BYTES: usize = 96;

    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    SAFE_KEYS.into_iter().find_map(|key| {
        let reason = object.get(key)?.as_str()?.trim();
        if reason.is_empty()
            || reason.len() > MAX_REASON_BYTES
            || !reason
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'_' | b'-' | b'.' | b':'))
        {
            return None;
        }
        Some(reason.to_owned())
    })
}

fn is_evm_network(network: &str) -> bool {
    network
        .strip_prefix("eip155:")
        .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_solana_network(network: &str) -> bool {
    network
        .parse::<ChainId>()
        .ok()
        .and_then(|chain| SolanaChainReference::try_from(chain).ok())
        .is_some()
}

fn valid_settlement_transaction(network: &str, value: &str) -> bool {
    if is_evm_network(network) {
        return valid_fixed_hex(value, 32);
    }
    is_solana_network(network)
        && SolanaSignature::from_str(value).is_ok_and(|signature| signature != SolanaSignature::default())
}

fn valid_fixed_hex(value: &str, bytes: usize) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|encoded| encoded.len() == bytes * 2 && encoded.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn stable_effect_ref(asset: &AssetIdentity, amount: Amount, recipient: &str, transaction: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"agentvault.x402.exact.effect.v1");
    let amount = amount.to_string();
    let transaction = if is_evm_network(&asset.network) {
        transaction.to_ascii_lowercase()
    } else {
        transaction.to_owned()
    };
    for value in [
        asset.network.as_bytes(),
        asset.token_contract.as_deref().unwrap_or_default().as_bytes(),
        amount.as_bytes(),
        recipient.as_bytes(),
        transaction.as_bytes(),
    ] {
        hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(value);
    }
    format!("x402-exact:{}", hex::encode(hash.finalize()))
}

#[cfg(test)]
#[path = "../../tests/handler/x402_test.rs"]
mod tests;
