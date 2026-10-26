pub mod apm;
pub mod cache;
pub mod cmd;
pub mod config;
pub mod controller;
pub mod handler;
pub mod job;
pub mod model;
pub mod payment;
pub mod protocol;
pub mod storage;
pub mod transport;
pub mod utils;

#[allow(clippy::default_trait_access, clippy::doc_markdown)]
pub(crate) mod authguard_access_wire {
    tonic::include_proto!("authguard.access.v1");
}

#[allow(clippy::default_trait_access, clippy::doc_markdown)]
pub(crate) mod grpc_wire {
    tonic::include_proto!("agentvault.transport.v1");
}

pub use cmd::AgentVaultServer;
pub use config::AppConfig;
pub use utils::{Error, Result};
