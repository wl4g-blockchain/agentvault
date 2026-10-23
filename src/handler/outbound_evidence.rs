//! Outbound merchant-response ingestion and reconciliation evidence.
//!
//! A protocol receipt header is authenticated only as input from the caller's
//! merchant connection. Its transaction is persisted as an untrusted locator;
//! it never becomes an intent `tx_hash` or ledger fact until [`ReceiptSource`]
//! independently confirms the exact on-chain transfer.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use crate::{
    model::{
        canonicalize_payment_recipient, PaymentChallenge, PaymentCredential, PaymentDirection, PaymentEvent,
        PaymentEventType, PaymentIntent, PaymentLedgerEntry, PaymentStatus,
    },
    storage::{EventStorage, IdempotentWrite, OutboundEvidenceWrite, PaymentStorage},
    utils::secure_material::{SealedSensitiveMaterial, SensitiveMaterialProtector},
    Error, Result,
};

use super::{
    common::{outbound_credential_context, validate_identifier},
    ProtocolRegistry, ReceiptObservation, ReceiptSource, ScenarioHandler, TransferQuery,
};

const MAX_MERCHANT_EVIDENCE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct RecordOutboundPaymentResponse {
    pub scenario_id: String,
    pub run_id: String,
    pub intent_id: String,
    pub protocol: String,
    pub header_name: String,
    pub header_value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedOutboundEvidence {
    pub intent: PaymentIntent,
    pub accepted: bool,
    pub replayed: bool,
}

struct ValidatedMerchantEvidence {
    network: String,
    transaction: String,
    payer: String,
    evidence_hash: String,
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
    pub receipts: Arc<dyn ReceiptSource>,
}

/// Coordinates the only accepted path from a merchant response to a trusted
/// outbound reconciliation observation.
pub struct OutboundEvidenceHandler {
    scenarios: Arc<ScenarioHandler>,
    payments: Arc<dyn PaymentStorage>,
    events: Arc<dyn EventStorage>,
    protocols: Arc<ProtocolRegistry>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
    receipts: Arc<dyn ReceiptSource>,
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

    /// Persists one protocol-defined merchant receipt as an untrusted hint. A
    /// negative response is returned to the SDK but cannot release the
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
        let Some(response) = self.validate_merchant_evidence(&intent, command).await? else {
            return Ok(RecordedOutboundEvidence {
                intent,
                accepted: false,
                replayed: false,
            });
        };
        let updated_at = now.max(intent.updated_at.saturating_add(1));
        let mut pending = intent.clone();
        pending.status = PaymentStatus::PendingSettlement;
        pending.updated_at = updated_at;
        pending.next_reconcile_at = Some(updated_at);
        let event = PaymentEvent {
            id: merchant_event_id(&intent.protocol, &intent.id),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Submitted,
            attempt_no: 0,
            signature_ref: None,
            // A merchant transaction locator is intentionally not copied into
            // this trusted column. It remains payload-only until RPC confirms.
            tx_hash: None,
            payload: json!({
                "source": format!("{}_merchant_evidence", intent.protocol),
                "evidence_hash": response.evidence_hash,
                "run_id": intent.run_id,
                "merchant_response": {
                    "success": true,
                    "protocol": intent.protocol,
                    "transaction": response.transaction,
                    "network": response.network,
                    "payer": response.payer,
                },
            }),
            created_at: updated_at,
        };
        let stored = self
            .payments
            .record_outbound_evidence(OutboundEvidenceWrite {
                intent: &pending,
                expected_updated_at: intent.updated_at,
                evidence_hash: &response.evidence_hash,
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

    async fn validate_merchant_evidence(
        &self,
        intent: &PaymentIntent,
        command: &RecordOutboundPaymentResponse,
    ) -> Result<Option<ValidatedMerchantEvidence>> {
        let adapter = self.protocols.get(&intent.protocol)?;
        if adapter.merchant_evidence_header().is_empty()
            || !command
                .header_name
                .eq_ignore_ascii_case(adapter.merchant_evidence_header())
        {
            return Err(Error::PaymentProtocol(
                "merchant evidence header does not match the payment protocol".to_owned(),
            ));
        }
        let response = adapter.decode_merchant_evidence(&command.header_value)?;
        if !response.accepted {
            return Ok(None);
        }
        let network = response.network.unwrap_or_else(|| intent.asset.network.clone());
        if network != intent.asset.network {
            return Err(Error::PaymentConflict(
                "merchant payment response network does not match the intent".to_owned(),
            ));
        }
        let transaction = response
            .transaction
            .as_deref()
            .and_then(|transaction| PaymentLedgerEntry::canonical_transaction(&intent.asset.network, transaction).ok())
            .ok_or_else(|| Error::PaymentProtocol("successful merchant receipt has no valid transaction".to_owned()))?;
        let expected_payer = self.load_expected_payer(intent).await?;
        let response_payer = match response.payer.as_deref() {
            Some(payer) => {
                let payer = canonicalize_payment_recipient(&intent.asset.network, payer)
                    .map_err(|_| Error::PaymentProtocol("merchant settlement payer is invalid".to_owned()))?;
                if payer != expected_payer {
                    return Err(Error::PaymentConflict(
                        "merchant payment response payer does not match the signed credential".to_owned(),
                    ));
                }
                payer
            }
            None => expected_payer,
        };
        let evidence_hash = merchant_evidence_hash(
            &intent.protocol,
            &intent.id,
            &intent.run_id,
            &network,
            &transaction,
            &response_payer,
        );
        Ok(Some(ValidatedMerchantEvidence {
            network,
            transaction,
            payer: response_payer,
            evidence_hash,
        }))
    }

    pub(crate) async fn reconciliation_observation(
        &self,
        intent: &PaymentIntent,
    ) -> Result<OutboundEvidenceObservation> {
        if intent.direction != PaymentDirection::Outbound || !self.protocols.contains(&intent.protocol) {
            return Err(Error::Settlement(
                "outbound merchant evidence received an incompatible intent".to_owned(),
            ));
        }
        let Some(event) = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Submitted, 0)
            .await?
        else {
            return Ok(OutboundEvidenceObservation::Unavailable {
                reason: "merchant settlement evidence has not been durably reported".to_owned(),
            });
        };
        let stored = stored_response(&event, intent)?;
        let expected_payer = self.load_expected_payer(intent).await?;
        if stored.payer != expected_payer {
            return Ok(OutboundEvidenceObservation::Unknown {
                reason: "durable merchant payer conflicts with the signed credential".to_owned(),
            });
        }
        let token_contract = intent
            .asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::Settlement("outbound intent has no token contract".to_owned()))?;
        let query = TransferQuery {
            network: intent.asset.network.clone(),
            transaction: stored.transaction,
            token_contract: token_contract.to_owned(),
            payer: expected_payer,
            recipient: intent.recipient.clone(),
            amount: intent.amount,
        };
        match self.receipts.lookup(&query).await? {
            ReceiptObservation::Confirmed(evidence) => Ok(OutboundEvidenceObservation::Confirmed {
                authority: evidence.authority,
                transaction: evidence.transaction,
                payer: query.payer,
                effect_ref: evidence.effect_ref,
                confirmations: evidence.confirmations,
                minimum_confirmations: evidence.minimum_confirmations,
                effective_at: evidence.effective_at,
            }),
            ReceiptObservation::Pending => Ok(OutboundEvidenceObservation::Pending),
            ReceiptObservation::Mismatch { reason } => Ok(OutboundEvidenceObservation::Unknown { reason }),
        }
    }

    async fn load_expected_payer(&self, intent: &PaymentIntent) -> Result<String> {
        let challenge = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::ChallengeReceived, 0)
            .await?
            .ok_or_else(|| Error::Storage("outbound challenge event is missing".to_owned()))?;
        let signed = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Signed, 0)
            .await?
            .ok_or_else(|| Error::Storage("outbound signed event is missing".to_owned()))?;
        expected_payer_from_events(
            intent,
            &challenge,
            &signed,
            self.protocols.as_ref(),
            self.material_protector.as_ref(),
        )
        .await
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredMerchantResponse {
    success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol: Option<String>,
    transaction: String,
    network: String,
    payer: String,
}

async fn expected_payer_from_events(
    intent: &PaymentIntent,
    challenge_event: &PaymentEvent,
    signed: &PaymentEvent,
    protocols: &ProtocolRegistry,
    material_protector: &dyn SensitiveMaterialProtector,
) -> Result<String> {
    let challenge: PaymentChallenge = challenge_event
        .payload
        .get("challenge")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| Error::Storage("outbound challenge event is missing".to_owned()))?;
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
    let payer = adapter.credential_payer(&challenge, &credential)?;
    canonicalize_payment_recipient(&intent.asset.network, &payer)
        .map_err(|_| Error::PaymentProtocol("signed payer is invalid".to_owned()))
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
    let run_id = event
        .payload
        .get("run_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Storage("outbound merchant response run ID is missing".to_owned()))?;
    let evidence_hash = event
        .payload
        .get("evidence_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Storage("outbound merchant response hash is missing".to_owned()))?;
    let protocol = response.protocol.as_deref().unwrap_or("x402");
    let expected_hash = merchant_evidence_hash(
        protocol,
        &intent.id,
        run_id,
        &response.network,
        &response.transaction,
        &response.payer,
    );
    if !response.success
        || run_id != intent.run_id
        || protocol != intent.protocol
        || response.network != intent.asset.network
        || !PaymentLedgerEntry::canonical_transaction(&response.network, &response.transaction)
            .is_ok_and(|transaction| transaction == response.transaction)
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
    validate_identifier("protocol", &command.protocol)?;
    if command.header_name.is_empty()
        || command.header_name.len() > 64
        || !command
            .header_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || command.header_value.is_empty()
        || command.header_value.len() > MAX_MERCHANT_EVIDENCE_BYTES
        || !command.header_value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(Error::InvalidRequest("merchant evidence header is invalid".to_owned()));
    }
    Ok(())
}

fn validate_intent_binding(intent: &PaymentIntent, command: &RecordOutboundPaymentResponse) -> Result<()> {
    if intent.scenario_id != command.scenario_id || intent.run_id != command.run_id {
        return Err(Error::Forbidden);
    }
    if intent.direction != PaymentDirection::Outbound || intent.protocol != command.protocol {
        return Err(Error::InvalidTransition(
            "merchant response protocol does not match the outbound intent".to_owned(),
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
    protocol: &str,
    intent_id: &str,
    run_id: &str,
    network: &str,
    transaction: &str,
    payer: &str,
) -> String {
    if protocol == "x402" {
        return hash_parts(&[
            b"agentvault.x402.merchant-evidence.v1",
            intent_id.as_bytes(),
            run_id.as_bytes(),
            network.as_bytes(),
            transaction.as_bytes(),
            payer.as_bytes(),
        ]);
    }
    hash_parts(&[
        b"agentvault.protocol.merchant-evidence.v1",
        protocol.as_bytes(),
        intent_id.as_bytes(),
        run_id.as_bytes(),
        network.as_bytes(),
        transaction.as_bytes(),
        payer.as_bytes(),
    ])
}

fn merchant_event_id(protocol: &str, intent_id: &str) -> String {
    if protocol == "x402" {
        return hash_parts(&[b"agentvault.x402.merchant-event.v1", intent_id.as_bytes()]);
    }
    hash_parts(&[
        b"agentvault.protocol.merchant-event.v1",
        protocol.as_bytes(),
        intent_id.as_bytes(),
    ])
}

fn hash_parts(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(part);
    }
    hex::encode(hash.finalize())
}

#[cfg(test)]
#[path = "../../tests/handler/outbound_evidence_test.rs"]
mod tests;
