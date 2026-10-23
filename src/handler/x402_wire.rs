//! Compatibility re-export for the protocol-owned strict x402 wire parser.

pub(crate) use crate::protocol::x402::{parse_settle_response, parse_verify_response};

#[cfg(test)]
#[path = "../../tests/handler/x402_wire_test.rs"]
mod tests;
