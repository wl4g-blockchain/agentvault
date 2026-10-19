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
pub(crate) use port::{outbound_evidence_replay_matches, validate_outbound_evidence_transition};

#[cfg(test)]
#[path = "../../tests/storage/mod_test.rs"]
mod schema_tests;
