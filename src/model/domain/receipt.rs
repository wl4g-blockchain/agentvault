use serde::{Deserialize, Serialize};

use crate::model::{Amount, AssetIdentity, PaymentDirection, PaymentStatus};

/// Protocol-neutral evidence returned after settlement observation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentReceipt {
    pub intent_id: String,
    pub scenario_id: String,
    pub direction: PaymentDirection,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub recipient: String,
    /// Payer identity observed by the trusted facilitator/provider or chain
    /// verifier, never accepted from an unverified merchant response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    pub requested_amount: Amount,
    pub settled_amount: Amount,
    pub status: PaymentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settlement_ref: Option<String>,
    /// Hash of trusted provider, facilitator, or chain evidence.
    pub evidence_hash: String,
    pub confirmed_at: i64,
}

impl PaymentReceipt {
    /// Ensures a receipt cannot claim confirmation without evidence and a
    /// settlement reference.
    ///
    /// # Errors
    ///
    /// Returns the invalid field or invariant name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.intent_id.trim().is_empty() {
            return Err("intent_id");
        }
        if self.scenario_id.trim().is_empty() {
            return Err("scenario_id");
        }
        if self.protocol.trim().is_empty() {
            return Err("protocol");
        }
        self.asset.validate()?;
        if self.requested_amount.is_zero() {
            return Err("requested_amount");
        }
        if self.payer.as_deref().is_some_and(str::is_empty) {
            return Err("payer");
        }
        if self.settled_amount > self.requested_amount {
            return Err("settled_amount");
        }
        if self.status == PaymentStatus::Settled {
            if self.settled_amount.is_zero() {
                return Err("settled_amount");
            }
            if self.tx_hash.is_none() && self.settlement_ref.is_none() {
                return Err("settlement_reference");
            }
            if self.evidence_hash.trim().is_empty() {
                return Err("evidence_hash");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/model/domain/receipt_test.rs"]
mod tests;
