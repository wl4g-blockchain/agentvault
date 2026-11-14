use std::{
    collections::BTreeMap,
    future::{poll_fn, ready, Future},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use agentvault_sdk::{
    AgentVault, PayeeConfig, PayerConfig, TransportError, TransportRequest, TransportResponse, VaultTransport,
};

#[derive(Clone)]
struct NoopTransport;

impl VaultTransport for NoopTransport {
    fn send(
        &self,
        _request: TransportRequest,
    ) -> impl Future<Output = Result<TransportResponse, TransportError>> + Send {
        ready(Ok(TransportResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: b"{}".to_vec(),
        }))
    }
}

#[test]
fn facade_builds_both_role_middlewares() {
    let vault = AgentVault::new(NoopTransport);
    let payer = PayerConfig::new("scenario").expect("payer config");
    assert!(vault.payer(payer).is_ok());
    assert!(vault.payee(PayeeConfig::default()).is_ok());
}

#[derive(Clone)]
struct BorrowedTransport(Arc<AtomicUsize>);

impl VaultTransport for BorrowedTransport {
    fn send(
        &self,
        _request: TransportRequest,
    ) -> impl Future<Output = Result<TransportResponse, TransportError>> + Send {
        poll_fn(|_| unreachable!("the ownership test does not send requests"))
    }

    fn close(&self) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.0.fetch_add(1, Ordering::SeqCst);
        ready(Ok(()))
    }
}

#[tokio::test]
async fn facade_does_not_close_an_injected_transport() {
    let closes = Arc::new(AtomicUsize::new(0));
    let vault = AgentVault::new(BorrowedTransport(Arc::clone(&closes)));

    vault.close().await.expect("borrowed close is a no-op");

    assert_eq!(closes.load(Ordering::SeqCst), 0);
}
