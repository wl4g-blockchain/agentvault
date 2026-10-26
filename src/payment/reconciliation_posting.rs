use crate::{
    model::ConfirmedSettlement, model::LedgerEntryType, model::PaymentEvent, model::PaymentEventType,
    model::PaymentIntent, model::PaymentLedgerEntry, model::PaymentReceipt, model::PaymentStatus,
    model::ReservationStatus, storage::OutboxMessage, storage::OutboxStatus, Error, Result,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};

pub(crate) struct ReconciliationPosting;
pub(crate) struct ReconciliationCommit {
    pub(crate) intent: PaymentIntent,
    pub(crate) ledger: PaymentLedgerEntry,
    pub(crate) event: PaymentEvent,
    pub(crate) outbox: OutboxMessage,
}

impl ReconciliationPosting {
    pub(crate) fn build_reconciliation_commit(
        intent: &PaymentIntent,
        evidence: &ConfirmedSettlement,
        attempt_no: u32,
        now: i64,
    ) -> Result<ReconciliationCommit> {
        let wallet_id = intent
            .wallet_id
            .as_deref()
            .ok_or_else(|| Error::Settlement("settled intent has no wallet".to_owned()))?;
        let stable_material = format!("{}:{}:{}", wallet_id, intent.id, evidence.posting_key);
        let ledger_id = ReconciliationPosting::stable_id("ledger", &stable_material);
        let receipt = ReconciliationPosting::reconciled_receipt(intent, evidence)?;
        Ok(ReconciliationCommit {
            intent: ReconciliationPosting::reconciled_intent(intent, evidence, now),
            ledger: ReconciliationPosting::reconciled_ledger(intent, evidence, wallet_id, &ledger_id, now)?,
            event: ReconciliationPosting::reconciled_event(
                intent,
                evidence,
                &receipt,
                &ledger_id,
                &stable_material,
                attempt_no,
                now,
            ),
            outbox: ReconciliationPosting::reconciled_outbox(intent, evidence, &stable_material, now),
        })
    }

    pub(crate) fn reconciled_intent(intent: &PaymentIntent, evidence: &ConfirmedSettlement, now: i64) -> PaymentIntent {
        let mut settled = intent.clone();
        settled.status = PaymentStatus::Settled;
        settled.settled_amount = evidence.settled_amount;
        settled.tx_hash.clone_from(&evidence.tx_hash);
        settled.settlement_ref.clone_from(&evidence.settlement_ref);
        settled.next_reconcile_at = None;
        // The immutable ledger/receipt retain the trusted chain timestamp. The
        // intent lifecycle itself cannot predate local intent creation when the
        // two clocks differ within the accepted bounded-skew window.
        settled.settled_at = Some(evidence.effective_at.max(intent.created_at));
        settled.updated_at = now.max(intent.updated_at.saturating_add(1));
        if settled.reservation_status == ReservationStatus::Reserved {
            settled.reservation_status = ReservationStatus::Consumed;
        }
        settled
    }

    pub(crate) fn reconciled_ledger(
        intent: &PaymentIntent,
        evidence: &ConfirmedSettlement,
        wallet_id: &str,
        ledger_id: &str,
        now: i64,
    ) -> Result<PaymentLedgerEntry> {
        let ledger = PaymentLedgerEntry {
            id: ledger_id.to_owned(),
            intent_id: Some(intent.id.clone()),
            scenario_id: Some(intent.scenario_id.clone()),
            wallet_id: wallet_id.to_owned(),
            entry_type: LedgerEntryType::Payment,
            direction: intent.direction,
            asset: intent.asset.clone(),
            amount: evidence.settled_amount,
            posting_key: evidence.posting_key.clone(),
            tx_hash: evidence.tx_hash.clone(),
            settlement_ref: evidence.settlement_ref.clone(),
            effect_ref: evidence.effect_ref.clone(),
            transfer_ref: None,
            reversal_of: None,
            effective_at: evidence.effective_at,
            created_at: now,
        };
        ledger
            .validate()
            .map_err(|field| Error::Settlement(format!("invalid reconciled ledger field: {field}")))?;
        Ok(ledger)
    }

    pub(crate) fn reconciled_receipt(intent: &PaymentIntent, evidence: &ConfirmedSettlement) -> Result<PaymentReceipt> {
        let evidence_hash = ReconciliationPosting::stable_id(
            "evidence",
            &format!(
                "{}:{}:{:?}:{:?}:{}",
                evidence.authority,
                evidence.posting_key,
                evidence.tx_hash,
                evidence.settlement_ref,
                evidence.settled_amount
            ),
        );
        let receipt = PaymentReceipt {
            intent_id: intent.id.clone(),
            scenario_id: intent.scenario_id.clone(),
            direction: intent.direction,
            protocol: intent.protocol.clone(),
            asset: intent.asset.clone(),
            recipient: intent.recipient.clone(),
            payer: evidence.payer.clone(),
            requested_amount: intent.amount,
            settled_amount: evidence.settled_amount,
            status: PaymentStatus::Settled,
            tx_hash: evidence.tx_hash.clone(),
            settlement_ref: evidence.settlement_ref.clone(),
            evidence_hash,
            confirmed_at: evidence.effective_at,
        };
        receipt
            .validate()
            .map_err(|field| Error::Settlement(format!("invalid reconciled receipt field: {field}")))?;
        Ok(receipt)
    }

    pub(crate) fn reconciled_event(
        intent: &PaymentIntent,
        evidence: &ConfirmedSettlement,
        receipt: &PaymentReceipt,
        ledger_id: &str,
        stable_material: &str,
        attempt_no: u32,
        now: i64,
    ) -> PaymentEvent {
        PaymentEvent {
            id: ReconciliationPosting::stable_id("event", stable_material),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Reconciled,
            attempt_no,
            signature_ref: None,
            tx_hash: evidence.tx_hash.clone(),
            payload: json!({
                "authority": evidence.authority,
                "posting_key": evidence.posting_key,
                "ledger_id": ledger_id,
                "receipt": receipt,
            }),
            created_at: now,
        }
    }

    pub(crate) fn reconciled_outbox(
        intent: &PaymentIntent,
        evidence: &ConfirmedSettlement,
        stable_material: &str,
        now: i64,
    ) -> OutboxMessage {
        OutboxMessage {
            id: ReconciliationPosting::stable_id("outbox", stable_material),
            aggregate_type: "payment_intent".to_owned(),
            aggregate_id: intent.id.clone(),
            event_type: "payment_settled".to_owned(),
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({
                "intent_id": intent.id,
                "scenario_id": intent.scenario_id,
                "status": PaymentStatus::Settled.as_str(),
                "settled_amount": evidence.settled_amount,
                "tx_hash": evidence.tx_hash,
                "settlement_ref": evidence.settlement_ref,
            }),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            available_at: now,
            lease_owner: None,
            lease_expires_at: None,
            last_error: None,
            created_at: now,
            delivered_at: None,
        }
    }

    pub(crate) fn nonempty(value: Option<String>) -> Option<String> {
        value.filter(|candidate| !candidate.trim().is_empty())
    }

    pub(crate) fn stable_id(prefix: &str, material: &str) -> String {
        let digest = Sha256::digest(material.as_bytes());
        format!("{prefix}-{}", hex::encode(digest))
    }
}

#[cfg(test)]
#[path = "../../tests/payment/reconciliation_posting_test.rs"]
mod tests;
