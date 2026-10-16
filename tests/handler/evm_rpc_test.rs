use super::*;

#[test]
fn requires_https_except_explicit_loopback_and_rejects_transport_headers() {
    assert!(FixedEvmRpcClient::new(
        "http://example.com/",
        BTreeMap::new(),
        Duration::from_secs(1),
        1024,
        true,
    )
    .is_err());
    assert!(FixedEvmRpcClient::new(
        "http://127.0.0.1:8545/",
        BTreeMap::new(),
        Duration::from_secs(1),
        1024,
        true,
    )
    .is_ok());
    assert!(FixedEvmRpcClient::new(
        "https://rpc.example/",
        BTreeMap::from([("content-type".to_owned(), "secret".to_owned())]),
        Duration::from_secs(1),
        1024,
        false,
    )
    .is_err());
}
