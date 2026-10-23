#![forbid(unsafe_code)]
#![allow(async_fn_in_trait)]
#![allow(clippy::missing_errors_doc)]

#[path = "agentvault.rs"]
mod agentvault;
#[path = "config/config.rs"]
mod config;
#[path = "model/approval.rs"]
mod model_approval;
#[path = "model/base.rs"]
mod model_base;
#[path = "model/payee.rs"]
mod model_payee;
#[path = "model/payer.rs"]
mod model_payer;
#[path = "payee/axum.rs"]
mod payee_axum;
#[path = "payee/payee.rs"]
mod payee_core;
#[path = "payee/standard.rs"]
mod payee_standard;
#[path = "payer/payer.rs"]
mod payer_core;
#[path = "payer/standard.rs"]
mod payer_standard;
#[path = "protocol/adapter.rs"]
mod protocol_adapter;
#[path = "protocol/mpp.rs"]
mod protocol_mpp;
#[path = "protocol/x402.rs"]
mod protocol_x402;
#[path = "transport/transport.rs"]
mod transport_core;
#[path = "transport/factory.rs"]
mod transport_factory;
#[path = "transport/grpc.rs"]
mod transport_grpc;
#[path = "transport/mqtt.rs"]
mod transport_mqtt;

pub(crate) use agentvault::paths;
pub use agentvault::AgentVault;
pub use config::{GrpcConfig, MqttConfig, PayeeConfig, PayerConfig};
pub use model_approval::*;
pub use model_base::*;
pub use model_payee::*;
pub use model_payer::*;
pub use payee_axum::{AxumPayeeMiddleware, AxumPayeeRoute};
pub use payee_core::PayeeMiddleware;
pub use payee_standard::{StandardPayeeMiddleware, StandardPayeeRequest};
pub use payer_core::PayerMiddleware;
pub use payer_standard::{PayerRequestExt, ReqwestPayerMiddleware};
pub use protocol_adapter::{
    MerchantEvidence, PaymentProtocol, PaymentProtocolFactory, ProtocolHttpResponse, ProtocolMcpMetadata,
    ProtocolMcpPaymentRequired, ProtocolRegistry,
};
pub use protocol_mpp::MppAdapter;
pub use protocol_x402::X402Adapter;
pub use transport_core::{
    HttpMethod, TransportError, TransportErrorKind, TransportRequest, TransportResponse, VaultTransport,
};
pub use transport_factory::{ConfiguredVaultTransport, VaultTransportConfig, VaultTransportFactory};
pub use transport_grpc::GrpcVaultTransport;
pub use transport_mqtt::MqttVaultTransport;

pub(crate) use transport_core::{percent_encode, ApiClient};
