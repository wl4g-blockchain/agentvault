use super::*;
use crate::{HttpConfig, VaultTransport, VaultTransportConfig};
#[tokio::test]
async fn factory_creates_isolated_idempotently_closable_transports() {
    let factory = VaultTransportFactory::new(VaultTransportConfig::Http(HttpConfig::new("http://127.0.0.1:65535")));
    let first = factory.create().unwrap();
    let second = factory.create().unwrap();
    first.close().await.unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
}
