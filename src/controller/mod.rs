//! Inbound transport adapters for the shared payment application service.

mod grpc;
pub mod http;
#[cfg(unix)]
mod local;
mod management;
mod mqtt;

pub use grpc::GrpcController;
#[cfg(unix)]
pub use local::LocalController;
pub use management::ManagementServer;
pub use mqtt::MqttController;

#[cfg(test)]
#[path = "../../tests/controller/mod_test.rs"]
mod tests;
