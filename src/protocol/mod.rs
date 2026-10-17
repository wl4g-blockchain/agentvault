//! Payment protocol adapter boundary.

mod adapter;
pub mod mpp;
pub mod x402;

pub use adapter::{PaymentProtocol, ProtocolSettlement, ProtocolSigningRequest, ProtocolValidation};

#[cfg(test)]
#[path = "../../tests/protocol/mod_test.rs"]
mod tests;
