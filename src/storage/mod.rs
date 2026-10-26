//! Durable payment persistence ports and SQL adapters.
//!
//! `storage` owns database-agnostic contracts, while `postgres` and `sqlite`
//! own dialect-specific prepared SQL. Correctness-sensitive mutations remain
//! explicit aggregate transactions; no generic row-update escape hatch is
//! exposed to handlers.

#[path = "storage.rs"]
mod port;
pub mod postgres;
pub mod sqlite;

pub use port::*;

#[cfg(test)]
#[path = "../../tests/storage/mod_test.rs"]
pub(crate) mod schema_tests;

pub mod approval;
pub use approval::*;
pub mod approver;
pub use approver::*;
pub mod client;
pub use client::*;
pub mod event;
pub use event::*;
pub mod ledger;
pub use ledger::*;
pub mod price;
pub use price::*;
pub mod outbox;
pub use outbox::*;
pub mod payment;
pub use payment::*;
pub mod policy;
pub use policy::*;
pub mod scenario;
pub use scenario::*;
pub mod signing;
pub use signing::*;
pub mod wallet;
pub use wallet::*;

pub(crate) mod schema;

mod entity;
pub use entity::*;
mod query;
pub use query::*;
