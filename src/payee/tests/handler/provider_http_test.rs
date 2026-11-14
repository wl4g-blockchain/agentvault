use std::time::Duration;

use super::*;

#[test]
fn rejects_remote_plain_http_and_path_traversal() {
    let remote = SettlementProviderConfig {
        base_url: "http://provider.example".to_owned(),
        ..SettlementProviderConfig::default()
    };
    assert!(FixedHttpJsonTransport::new(&remote, HeaderMap::new()).is_err());

    for path in ["../settle", "https://evil.example/settle", "%2e%2e/settle"] {
        let unsafe_config = SettlementProviderConfig {
            base_url: "https://provider.example/api".to_owned(),
            settle_path: path.to_owned(),
            ..SettlementProviderConfig::default()
        };
        assert!(FixedHttpJsonTransport::new(&unsafe_config, HeaderMap::new()).is_err());
    }
}

#[test]
fn permits_explicit_loopback_and_rejects_reserved_headers() {
    let config = SettlementProviderConfig {
        base_url: "http://127.0.0.1:8088/api".to_owned(),
        timeout: Duration::from_secs(1),
        allow_insecure_loopback: true,
        ..SettlementProviderConfig::default()
    };
    assert!(FixedHttpJsonTransport::new(&config, HeaderMap::new()).is_ok());

    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", HeaderValue::from_static("operator-value"));
    assert!(FixedHttpJsonTransport::new(&config, headers).is_err());

    for name in ["traceparent", "tracestate"] {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static("operator-value"));
        assert!(FixedHttpJsonTransport::new(&config, headers).is_err());
    }
}
