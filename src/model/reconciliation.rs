use super::{payment_recipients_match, Amount, AssetIdentity, PaymentIntent};
use crate::{Error, Result};

use super::SettlementProviderResult;

/// Confirmed financial fact returned by a trusted provider or RPC adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedSettlement {
    pub authority: String,
    pub asset: AssetIdentity,
    pub recipient: String,
    pub payer: Option<String>,
    pub settled_amount: Amount,
    pub posting_key: String,
    pub tx_hash: Option<String>,
    pub settlement_ref: Option<String>,
    pub effect_ref: Option<String>,
    pub confirmations: u64,
    pub minimum_confirmations: u64,
    pub effective_at: i64,
}

impl ConfirmedSettlement {
    pub(crate) fn validate_for(&self, intent: &PaymentIntent, observed_at: i64) -> Result<()> {
        if self.authority.trim().is_empty() {
            return Err(Error::Settlement("settlement evidence authority is empty".to_owned()));
        }
        if self.posting_key.trim().is_empty() {
            return Err(Error::Settlement("settlement evidence posting key is empty".to_owned()));
        }
        let expected_posting_key = crate::model::PaymentLedgerEntry::settlement_posting_key(
            &self.authority,
            &intent.asset.network,
            self.tx_hash.as_deref(),
            self.settlement_ref.as_deref(),
            self.effect_ref.as_deref(),
        )
        .map_err(|field| Error::Settlement(format!("invalid settlement posting evidence: {field}")))?;
        if self.posting_key != expected_posting_key {
            return Err(Error::Settlement(
                "settlement evidence posting key is not canonical".to_owned(),
            ));
        }
        if self.settled_amount != intent.amount {
            return Err(Error::Settlement(
                "settlement evidence amount does not match the exact payment intent".to_owned(),
            ));
        }
        if self.asset != intent.asset
            || !payment_recipients_match(&intent.asset.network, &self.recipient, &intent.recipient)
            || self.confirmations < self.minimum_confirmations
        {
            return Err(Error::Settlement(
                "settlement evidence asset, recipient, or confirmations do not match the intent".to_owned(),
            ));
        }
        if self.tx_hash.as_deref().is_none_or(|value| value.trim().is_empty())
            && self
                .settlement_ref
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(Error::Settlement(
                "settlement evidence has no transaction or provider reference".to_owned(),
            ));
        }
        crate::model::PaymentLedgerEntry::validate_trusted_effective_time(
            intent.created_at,
            self.effective_at,
            observed_at,
        )?;
        if intent
            .tx_hash
            .as_deref()
            .is_some_and(|expected| self.tx_hash.as_deref() != Some(expected))
        {
            return Err(Error::Settlement(
                "settlement evidence transaction conflicts with the payment intent".to_owned(),
            ));
        }
        if intent
            .settlement_ref
            .as_deref()
            .is_some_and(|expected| self.settlement_ref.as_deref() != Some(expected))
        {
            return Err(Error::Settlement(
                "settlement evidence reference conflicts with the payment intent".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconciliationEvidence {
    Confirmed(Box<ConfirmedSettlement>),
    Verified { verification_ref: String },
    Pending,
    Expired { reason: String },
    Unknown { reason: String },
    Unavailable { reason: String },
}

/// Trusted provider observation plus the operator-owned evidence authority.
/// This crate-private value joins settlement recovery to reconciliation and
/// is never serialized as a transport DTO.
pub(crate) enum ReconciliationProviderObservation {
    Observed {
        authority: String,
        minimum_confirmations: u64,
        result: Box<SettlementProviderResult>,
        effective_at: Option<i64>,
    },
    Expired {
        reason: String,
    },
}

/// Summary for one bounded reconciliation pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconciliationRun {
    pub claimed: u32,
    pub settled: u32,
    pub verified: u32,
    pub expired: u32,
    pub pending: u32,
    pub unknown: u32,
    pub unavailable: u32,
    pub provider_errors: u32,
    pub skipped: u32,
}

#[cfg(test)]
#[path = "../../tests/model/reconciliation_test.rs"]
mod tests;
