//! Wire-level protocol adaptation; payment authority remains in payment/domain services.
pub(crate) mod grpc;
pub mod http;
pub(crate) mod mqtt;
#[cfg(test)]
#[path = "../../tests/transport/mod_test.rs"]
mod tests;
