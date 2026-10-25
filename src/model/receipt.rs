use super::{Amount, PaymentIntent};

pub use super::domain::PaymentReceipt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferQuery {
    pub network: String,
    pub transaction: String,
    pub token_contract: String,
    pub payer: String,
    pub recipient: String,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmAuthorizationQuery {
    pub network: String,
    pub token_contract: String,
    pub payer: String,
    pub nonce: String,
    pub recipient: String,
    pub amount: Amount,
    pub from_block: u64,
}

pub type EvmTransferQuery = TransferQuery;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedTransfer {
    pub authority: String,
    pub transaction: String,
    pub effect_ref: String,
    pub confirmations: u64,
    pub minimum_confirmations: u64,
    pub effective_at: i64,
}

pub type ConfirmedEvmTransfer = ConfirmedTransfer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptObservation {
    Confirmed(ConfirmedTransfer),
    Pending,
    Mismatch { reason: String },
}

pub use ReceiptObservation as EvmReceiptObservation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvmAuthorizationObservation {
    Confirmed(ConfirmedTransfer),
    Pending,
    NoEffect { observed_block: u64, observed_at: i64 },
    Mismatch { reason: String },
}

/// Trusted observation produced after independently checking an outbound
/// merchant receipt against the configured chain evidence source.
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

#[cfg(test)]
#[path = "../../tests/model/receipt_test.rs"]
mod tests;
