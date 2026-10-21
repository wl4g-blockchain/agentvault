use std::sync::Arc;

use crate::protocol::{
    x402::{X402Adapter, X402Asset},
    PaymentProtocolFactory,
};

fn x402() -> Arc<X402Adapter> {
    Arc::new(
        X402Adapter::new(
            vec![X402Asset {
                network: "eip155:31337".to_owned(),
                contract: "0x0000000000000000000000000000000000000001".to_owned(),
                symbol: "USDC".to_owned(),
                decimals: 6,
            }],
            300,
        )
        .expect("adapter"),
    )
}

#[test]
fn factory_builds_isolated_registry_and_rejects_duplicates() {
    let first = PaymentProtocolFactory::new()
        .register(x402())
        .build()
        .expect("registry");
    let empty = PaymentProtocolFactory::new().build().expect("empty registry");

    assert!(first.contains("x402"));
    assert!(!empty.contains("x402"));
    assert!(PaymentProtocolFactory::new()
        .register(x402())
        .register(x402())
        .build()
        .is_err());
}
