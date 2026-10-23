//! Payment protocol adapter boundary.

mod adapter;
mod factory;
pub mod mpp;
pub mod x402;

pub use adapter::{
    PaymentProtocol, ProtocolHttpCredential, ProtocolMerchantEvidence, ProtocolSettlement, ProtocolSigningRequest,
    ProtocolValidation,
};
pub use factory::{PaymentProtocolFactory, ProtocolRegistry};

#[cfg(test)]
#[path = "../../tests/protocol/mod_test.rs"]
mod tests;
