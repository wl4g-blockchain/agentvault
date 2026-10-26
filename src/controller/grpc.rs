use crate::{
    apm::AgentVaultMetrics,
    cmd::{RuntimeComponent, Shutdown},
    config::GrpcTransportConfig,
    controller::principal_context::PrincipalContext,
    grpc_wire::payment_transport_server::PaymentTransportServer,
    transport::grpc::GrpcEnvelopeService,
};
use anyhow::Context as _;
use async_trait::async_trait;
use axum::Router;
use std::net::SocketAddr;
pub struct GrpcController {
    config: GrpcTransportConfig,
    service: GrpcEnvelopeService,
}

impl GrpcController {
    #[must_use]
    pub fn new(
        config: GrpcTransportConfig,
        principals: PrincipalContext,
        router: Router,
        metrics: AgentVaultMetrics,
    ) -> Self {
        let service = GrpcEnvelopeService {
            router,
            principals,
            metrics,
            max_message_bytes: config.max_message_bytes,
            timeout: config.request_timeout,
        };
        Self { config, service }
    }
}

#[async_trait]
impl RuntimeComponent for GrpcController {
    fn name(&self) -> &'static str {
        "gRPC payment transport"
    }

    async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let address = SocketAddr::new(self.config.host, self.config.port);
        let service = PaymentTransportServer::new(self.service)
            .max_decoding_message_size(self.config.max_message_bytes)
            .max_encoding_message_size(self.config.max_message_bytes);
        tracing::info!(server.address = %address, "AgentVault gRPC transport listening");
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_shutdown(address, async move { shutdown.cancelled().await })
            .await
            .with_context(|| format!("serve gRPC payment transport on {address}"))
    }
}

#[cfg(test)]
#[path = "../../tests/controller/grpc_test.rs"]
mod tests;
