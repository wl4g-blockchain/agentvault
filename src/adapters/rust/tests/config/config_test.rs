use std::time::Duration;

use super::*;

#[test]
fn role_configuration_validates_limits_and_redacts_edge_secrets() {
    let asset = AssetIdentity {
        network: "eip155:1".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: None,
        decimals: 6,
    };
    let config = PayerConfig::new("scenario", "wallet", "x402", asset, "100", "recipient")
        .expect("valid payer configuration")
        .with_approval_wait(Duration::from_secs(5), Duration::from_secs(1));
    assert!(config.validate().is_ok());
    assert!(config.with_delivery_retries(4).validate().is_err());
    assert!(PayeeConfig { max_body_bytes: 0 }.validate().is_err());

    let grpc = GrpcConfig::new("vault.example:443").with_authguard_bearer("edge-secret");
    assert!(!format!("{grpc:?}").contains("edge-secret"));
}
