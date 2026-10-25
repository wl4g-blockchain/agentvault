//! Persistence entities that map to authoritative SQL tables or aggregates.
//! Transport DTOs and application commands belong in the parent `model`
//! modules, not in this namespace.

mod approval;
mod client;
mod event;
mod ledger;
mod offer;
mod payment;
mod policy;
mod receipt;
mod scenario;
mod wallet;

pub use approval::{ApprovalRequest, ApprovalStatus, PaymentScenarioApprover};
pub use client::PaymentClient;
pub use event::{PaymentEvent, PaymentEventType};
pub use ledger::{settlement_posting_key, LedgerEntryType, PaymentLedgerEntry};
pub(crate) use ledger::{
    MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS, MAX_SETTLEMENT_EFFECTIVE_TIME_PAST_SKEW_SECONDS,
};
pub use offer::PaymentOffer;
pub use payment::{PaymentIntent, PaymentStatus, ReservationStatus};
pub use policy::{PaymentPolicy, PaymentPolicyLimits};
pub use receipt::PaymentReceipt;
pub use scenario::PaymentScenario;
pub use wallet::Wallet;

#[cfg(test)]
#[path = "../../../tests/model/domain/mod_test.rs"]
mod tests;
