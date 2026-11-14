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

pub use approval::ApprovalRequest;
pub use approval::ApprovalStatus;
pub use approval::PaymentScenarioApprover;
pub use client::PaymentClient;
pub use event::PaymentEvent;
pub use event::PaymentEventType;
pub use ledger::LedgerEntryType;
pub use ledger::PaymentLedgerEntry;
pub use ledger::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;
pub use payment::PaymentIntent;
pub use payment::PaymentStatus;
pub use payment::ReservationStatus;
pub use policy::PaymentPolicy;
pub use policy::PaymentPolicyLimits;
pub use price::PaymentPrice;
pub use receipt::PaymentReceipt;
pub use scenario::PaymentScenario;
pub use wallet::Wallet;

#[cfg(test)]
#[path = "../../../tests/model/domain/mod_test.rs"]
mod tests;
