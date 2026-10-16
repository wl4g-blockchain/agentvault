//! Outbound x402 merchant-response ingestion and reconciliation evidence.
//!
//! The official `PAYMENT-RESPONSE` header is authenticated only as input from
//! the caller's merchant connection. Its transaction is persisted as an
//! untrusted locator; it never becomes an intent `tx_hash` or ledger fact until
//! [`EvmReceiptSource`] independently confirms the exact on-chain transfer.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use x402_types::{proto::v2::SettleResponse, util::b64::Base64Bytes};

use crate::{
    model::{
        canonicalize_payment_recipient, PaymentChallenge, PaymentCredential, PaymentDirection, PaymentEvent,
        PaymentEventType, PaymentIntent, PaymentStatus,
    },
    storage::{EventStorage, IdempotentWrite, OutboundEvidenceWrite, PageRequest, PaymentStorage},
    utils::secure_material::{SealedSensitiveMaterial, SensitiveMaterialProtector},
    Error, Result,
};

use super::{
    common::{outbound_credential_context, validate_identifier},
    x402_wire::parse_settle_response,
    EvmReceiptObservation, EvmReceiptSource, EvmTransferQuery, ProtocolRegistry, ScenarioHandler,
};

const MAX_PAYMENT_RESPONSE_BYTES: usize = 16 * 1024;
const EVENT_PAGE_SIZE: u32 = 1_000;

#[derive(Clone, Debug)]
pub struct RecordOutboundPaymentResponse {
    pub scenario_id: String,
    pub run_id: String,
    pub intent_id: String,
    pub idempotency_key: String,
    pub payment_response: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedOutboundEvidence {
    pub intent: PaymentIntent,
    pub accepted: bool,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutboundEvidenceObservation {
    Confirmed {
        authority: String,
        transaction: String,
        payer: String,
        effect_ref: String,
        confirmations: u64,
        minimum_confirmations: u64,
        effective_at: i64,
    },
    Pending,
    Unknown {
        reason: String,
    },
    Unavailable {
        reason: String,
    },
}

pub struct OutboundEvidenceDependencies {
    pub scenarios: Arc<ScenarioHandler>,
    pub payments: Arc<dyn PaymentStorage>,
    pub events: Arc<dyn EventStorage>,
    pub protocols: Arc<ProtocolRegistry>,
    pub material_protector: Arc<dyn SensitiveMaterialProtector>,
    pub receipts: Arc<dyn EvmReceiptSource>,
}

/// Coordinates the only accepted path from a merchant response to a trusted
/// outbound reconciliation observation.
pub struct OutboundEvidenceHandler {
    scenarios: Arc<ScenarioHandler>,
    payments: Arc<dyn PaymentStorage>,
    events: Arc<dyn EventStorage>,
    protocols: Arc<ProtocolRegistry>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
    receipts: Arc<dyn EvmReceiptSource>,
}

impl OutboundEvidenceHandler {
    #[must_use]
    pub fn new(dependencies: OutboundEvidenceDependencies) -> Self {
        Self {
            scenarios: dependencies.scenarios,
            payments: dependencies.payments,
            events: dependencies.events,
            protocols: dependencies.protocols,
            material_protector: dependencies.material_protector,
            receipts: dependencies.receipts,
        }
    }

    /// Persists an official x402 v2 settlement response as an untrusted hint.
    /// A failure response is returned to the SDK but cannot release the
    /// reservation or make a terminal state transition.
    ///
    /// # Errors
    ///
    /// Returns an error when authentication, intent binding, official wire
    /// decoding, signed-payer verification, or the atomic evidence write fails.
    pub async fn record_merchant_response(
        &self,
        principal: &str,
        command: &RecordOutboundPaymentResponse,
        now: i64,
    ) -> Result<RecordedOutboundEvidence> {
        validate_command(command)?;
        let _access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        let intent = self
            .payments
            .get_for_scenario(&command.scenario_id, &command.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("outbound payment intent".to_owned()))?;
        validate_intent_binding(&intent, command)?;
        let response = decode_payment_response(&command.payment_response)?;
        if response.network != intent.asset.network {
            return Err(Error::PaymentConflict(
                "merchant payment response network does not match the intent".to_owned(),
            ));
        }
        if !response.success {
            if !response.transaction.is_empty() {
                return Err(Error::PaymentProtocol(
                    "failed x402 settlement response contains a transaction".to_owned(),
                ));
            }
            return Ok(RecordedOutboundEvidence {
                intent,
                accepted: false,
                replayed: false,
            });
        }
        if response.error_reason.is_some() || !valid_transaction_hash(&response.transaction) {
            return Err(Error::PaymentProtocol(
                "successful x402 settlement response is malformed".to_owned(),
            ));
        }
        let response_payer = response
            .payer
            .as_deref()
            .ok_or_else(|| Error::PaymentProtocol("successful x402 settlement response has no payer".to_owned()))?;
        let response_payer = canonicalize_payment_recipient(&intent.asset.network, response_payer)
            .map_err(|_| Error::PaymentProtocol("x402 settlement payer is invalid".to_owned()))?;
        let expected_payer = self.load_expected_payer(&intent).await?;
        if response_payer != expected_payer {
            return Err(Error::PaymentConflict(
                "merchant payment response payer does not match the signed credential".to_owned(),
            ));
        }

        let transaction = response.transaction.to_ascii_lowercase();
        let evidence_hash = merchant_evidence_hash(
            &intent.id,
            &command.idempotency_key,
            &response.network,
            &transaction,
            &response_payer,
        );
        let updated_at = now.max(intent.updated_at.saturating_add(1));
        let mut pending = intent.clone();
        pending.status = PaymentStatus::PendingSettlement;
        pending.updated_at = updated_at;
        pending.next_reconcile_at = Some(updated_at);
        let event = PaymentEvent {
            id: merchant_event_id(&intent.id),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Submitted,
            attempt_no: 0,
            signature_ref: None,
            // A merchant transaction locator is intentionally not copied into
            // this trusted column. It remains payload-only until RPC confirms.
            tx_hash: None,
            payload: json!({
                "source": "x402_payment_response",
                "evidence_hash": evidence_hash,
                "idempotency_key": command.idempotency_key,
                "merchant_response": {
                    "success": true,
                    "transaction": transaction,
                    "network": response.network,
                    "payer": response_payer,
                },
            }),
            created_at: updated_at,
        };
        let stored = self
            .payments
            .record_outbound_evidence(OutboundEvidenceWrite {
                intent: &pending,
                expected_updated_at: intent.updated_at,
                evidence_hash: &evidence_hash,
                event: &event,
                outbox: &[],
            })
            .await?;
        let (intent, replayed) = match stored {
            IdempotentWrite::Inserted(intent) => (intent, false),
            IdempotentWrite::Replay(intent) => (intent, true),
        };
        Ok(RecordedOutboundEvidence {
            intent,
            accepted: true,
            replayed,
        })
    }

    pub(crate) async fn reconciliation_observation(
        &self,
        intent: &PaymentIntent,
    ) -> Result<OutboundEvidenceObservation> {
        if intent.direction != PaymentDirection::Outbound || intent.protocol != "x402" {
            return Err(Error::Settlement(
                "outbound x402 evidence received an incompatible intent".to_owned(),
            ));
        }
        let events = self.load_events(intent).await?;
        let Some(event) = events
            .iter()
            .find(|event| event.event_type == PaymentEventType::Submitted && event.attempt_no == 0)
        else {
            return Ok(OutboundEvidenceObservation::Unavailable {
                reason: "merchant PAYMENT-RESPONSE has not been durably reported".to_owned(),
            });
        };
        let stored = stored_response(event, intent)?;
        let expected_payer = expected_payer_from_events(
            intent,
            &events,
            self.protocols.as_ref(),
            self.material_protector.as_ref(),
        )
        .await?;
        if stored.payer != expected_payer {
            return Ok(OutboundEvidenceObservation::Unknown {
                reason: "durable merchant payer conflicts with the signed credential".to_owned(),
            });
        }
        let token_contract = intent
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::Settlement("outbound x402 intent has no token contract".to_owned()))?;
        let query = EvmTransferQuery {
            network: intent.asset.network.clone(),
            transaction: stored.transaction,
            token_contract: token_contract.to_owned(),
            payer: expected_payer,
            recipient: intent.recipient.clone(),
            amount: intent.amount,
        };
        match self.receipts.lookup(&query).await? {
            EvmReceiptObservation::Confirmed(evidence) => Ok(OutboundEvidenceObservation::Confirmed {
                authority: evidence.authority,
                transaction: evidence.transaction,
                payer: query.payer,
                effect_ref: evidence.effect_ref,
                confirmations: evidence.confirmations,
                minimum_confirmations: evidence.minimum_confirmations,
                effective_at: evidence.effective_at,
            }),
            EvmReceiptObservation::Pending => Ok(OutboundEvidenceObservation::Pending),
            EvmReceiptObservation::Mismatch { reason } => Ok(OutboundEvidenceObservation::Unknown { reason }),
        }
    }

    async fn load_expected_payer(&self, intent: &PaymentIntent) -> Result<String> {
        let events = self.load_events(intent).await?;
        expected_payer_from_events(
            intent,
            &events,
            self.protocols.as_ref(),
            self.material_protector.as_ref(),
        )
        .await
    }

    async fn load_events(&self, intent: &PaymentIntent) -> Result<Vec<PaymentEvent>> {
        let page = self
            .events
            .list_for_intent(
                &intent.id,
                PageRequest {
                    offset: 0,
                    limit: EVENT_PAGE_SIZE,
                },
            )
            .await?;
        Ok(page.items)
    }
}

#[derive(Clone, Debug)]
struct X402SettlementResponse {
    success: bool,
    error_reason: Option<String>,
    transaction: String,
    network: String,
    payer: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredMerchantResponse {
    success: bool,
    transaction: String,
    network: String,
    payer: String,
}

fn decode_payment_response(encoded: &str) -> Result<X402SettlementResponse> {
    if encoded.is_empty()
        || encoded.len() > MAX_PAYMENT_RESPONSE_BYTES
        || !encoded.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Error::InvalidRequest(
            "x402 PAYMENT-RESPONSE header is invalid".to_owned(),
        ));
    }
    let decoded = Base64Bytes::from(encoded.as_bytes())
        .decode()
        .map_err(|_| Error::PaymentProtocol("x402 PAYMENT-RESPONSE is not standard base64".to_owned()))?;
    if decoded.is_empty() || decoded.len() > MAX_PAYMENT_RESPONSE_BYTES {
        return Err(Error::InvalidRequest(
            "x402 PAYMENT-RESPONSE payload is too large".to_owned(),
        ));
    }
    match parse_settle_response(&decoded)? {
        SettleResponse::Success {
            payer,
            transaction,
            network,
        } => Ok(X402SettlementResponse {
            success: true,
            error_reason: None,
            transaction,
            network,
            payer: Some(payer),
        }),
        SettleResponse::Error { reason, network } => Ok(X402SettlementResponse {
            success: false,
            error_reason: Some(reason),
            transaction: String::new(),
            network,
            payer: None,
        }),
    }
}

async fn expected_payer_from_events(
    intent: &PaymentIntent,
    events: &[PaymentEvent],
    protocols: &ProtocolRegistry,
    material_protector: &dyn SensitiveMaterialProtector,
) -> Result<String> {
    let challenge: PaymentChallenge = events
        .iter()
        .find(|event| event.event_type == PaymentEventType::ChallengeReceived)
        .and_then(|event| event.payload.get("challenge").cloned())
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| Error::Storage("outbound challenge event is missing".to_owned()))?;
    let signed = events
        .iter()
        .find(|event| event.event_type == PaymentEventType::Signed)
        .ok_or_else(|| Error::Storage("outbound signed event is missing".to_owned()))?;
    let sealed: SealedSensitiveMaterial = signed
        .payload
        .get("sealed_credential")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| Error::Storage("outbound signed event has no sealed credential".to_owned()))?;
    let plaintext = material_protector.open(&outbound_credential_context(&intent.id), &sealed)?;
    let credential: PaymentCredential = serde_json::from_slice(&plaintext)?;
    let adapter = protocols.get(&intent.protocol)?;
    adapter.validate_payment(&challenge, intent).await?;
    let fingerprint = adapter.credential_fingerprint(&challenge, &credential)?;
    if signed.signature_ref.as_deref() != Some(fingerprint.as_str()) || credential.credential_hash != fingerprint {
        return Err(Error::PaymentConflict(
            "durable outbound credential fingerprint is inconsistent".to_owned(),
        ));
    }
    let settlement = adapter.settle_payment(&challenge, &credential).await?;
    let payer = settlement
        .payment_payload
        .pointer("/payload/authorization/from")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::PaymentProtocol("x402 signed payer is missing".to_owned()))?;
    canonicalize_payment_recipient(&intent.asset.network, payer)
        .map_err(|_| Error::PaymentProtocol("x402 signed payer is invalid".to_owned()))
}

fn stored_response(event: &PaymentEvent, intent: &PaymentIntent) -> Result<StoredMerchantResponse> {
    if event.intent_id != intent.id || event.tx_hash.is_some() || event.signature_ref.is_some() {
        return Err(Error::Storage(
            "outbound merchant evidence event binding is invalid".to_owned(),
        ));
    }
    let response: StoredMerchantResponse = event
        .payload
        .get("merchant_response")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| Error::Storage("outbound merchant response is missing".to_owned()))?;
    let key = event
        .payload
        .get("idempotency_key")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Storage("outbound merchant response idempotency key is missing".to_owned()))?;
    let evidence_hash = event
        .payload
        .get("evidence_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Storage("outbound merchant response hash is missing".to_owned()))?;
    let expected_hash = merchant_evidence_hash(
        &intent.id,
        key,
        &response.network,
        &response.transaction,
        &response.payer,
    );
    if !response.success
        || response.network != intent.asset.network
        || !valid_transaction_hash(&response.transaction)
        || !canonicalize_payment_recipient(&intent.asset.network, &response.payer)
            .is_ok_and(|payer| payer == response.payer)
        || evidence_hash != expected_hash
    {
        return Err(Error::Storage(
            "outbound merchant response integrity check failed".to_owned(),
        ));
    }
    Ok(response)
}

fn validate_command(command: &RecordOutboundPaymentResponse) -> Result<()> {
    validate_identifier("scenario_id", &command.scenario_id)?;
    validate_identifier("run_id", &command.run_id)?;
    validate_identifier("intent_id", &command.intent_id)?;
    validate_identifier("idempotency_key", &command.idempotency_key)?;
    if command.payment_response.is_empty() {
        return Err(Error::InvalidRequest("payment_response is required".to_owned()));
    }
    Ok(())
}

fn validate_intent_binding(intent: &PaymentIntent, command: &RecordOutboundPaymentResponse) -> Result<()> {
    if intent.scenario_id != command.scenario_id || intent.run_id != command.run_id {
        return Err(Error::Forbidden);
    }
    if intent.direction != PaymentDirection::Outbound || intent.protocol != "x402" {
        return Err(Error::InvalidTransition(
            "merchant response is supported only for outbound x402 intents".to_owned(),
        ));
    }
    if !matches!(
        intent.status,
        PaymentStatus::Signed | PaymentStatus::PendingSettlement | PaymentStatus::Unknown | PaymentStatus::Settled
    ) {
        return Err(Error::InvalidTransition(format!(
            "cannot report merchant response in {}",
            intent.status.as_str()
        )));
    }
    Ok(())
}

fn merchant_evidence_hash(
    intent_id: &str,
    idempotency_key: &str,
    network: &str,
    transaction: &str,
    payer: &str,
) -> String {
    hash_parts(&[
        b"agentvault.x402.merchant-evidence.v1",
        intent_id.as_bytes(),
        idempotency_key.as_bytes(),
        network.as_bytes(),
        transaction.as_bytes(),
        payer.as_bytes(),
    ])
}

fn merchant_event_id(intent_id: &str) -> String {
    hash_parts(&[b"agentvault.x402.merchant-event.v1", intent_id.as_bytes()])
}

fn hash_parts(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(part);
    }
    hex::encode(hash.finalize())
}

fn valid_transaction_hash(value: &str) -> bool {
    value.len() == 66 && value.starts_with("0x") && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "../../tests/handler/outbound_evidence_test.rs"]
mod tests;
