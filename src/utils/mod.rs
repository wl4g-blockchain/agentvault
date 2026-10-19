//! Cohesive cross-cutting primitives with no payment workflow ownership.

pub(crate) mod aead;
pub(crate) mod crypto;
mod error;
pub(crate) mod runtime_identity;
pub(crate) mod secret;
pub mod secure_material;

pub use error::{Error, Result};

#[cfg(test)]
#[path = "../../tests/utils/mod_test.rs"]
mod tests;
