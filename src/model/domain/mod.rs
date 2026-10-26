//! Persistence entities that map to authoritative SQL tables or aggregates.
//! Transport DTOs and application commands belong in the parent `model`
//! modules, not in this namespace.

mod approval;
mod client;
mod event;
mod ledger;
mod payment;
mod policy;
mod price;
mod receipt;
mod scenario;
mod wallet;

pub use approval::{ApprovalRequest, ApprovalStatus, PaymentScenarioApprover};
pub use client::PaymentClient;
pub use event::{PaymentEvent, PaymentEventType};
pub(crate) use ledger::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;
pub use ledger::{LedgerEntryType, PaymentLedgerEntry};
pub use payment::{PaymentIntent, PaymentStatus, ReservationStatus};
pub use policy::{PaymentPolicy, PaymentPolicyLimits};
pub use price::PaymentPrice;
pub use receipt::PaymentReceipt;
pub use scenario::PaymentScenario;
pub use wallet::Wallet;

#[cfg(test)]
#[path = "../../../tests/model/domain/mod_test.rs"]
mod tests;

/// Compatibility name for existing callers; pricing is stored by `PaymentPrice`.
pub type PaymentPrice = PaymentPrice;
