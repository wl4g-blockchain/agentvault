use std::collections::BTreeMap;

use agentvault_sdk::{
    AgentVault, PayeeConfig, PayerConfig, TransportError, TransportRequest, TransportResponse, VaultTransport,
};

#[derive(Clone)]
struct NoopTransport;

impl VaultTransport for NoopTransport {
    async fn send(&self, _request: TransportRequest) -> Result<TransportResponse, TransportError> {
        Ok(TransportResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: b"{}".to_vec(),
        })
    }
}

#[test]
fn facade_builds_both_role_middlewares() {
    let vault = AgentVault::new(NoopTransport);
    let payer = PayerConfig::new("scenario").expect("payer config");
    assert!(vault.payer(payer).is_ok());
    assert!(vault.payee(PayeeConfig::default()).is_ok());
}
