use std::sync::Arc;

use crate::x402::X402Adapter;
use crate::x402::X402Asset;
use crate::PaymentProtocolFactory;

fn x402() -> Arc<X402Adapter> {
    Arc::new(
        X402Adapter::new(
            vec![X402Asset {
                fee_payer: None,
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

use crate::model::{payment_api::PaymentCredentialDto, PaymentCredential};
use serde_json::{json, Value};
#[test]
fn http_credential_debug_and_delivery_never_expose_nested_secrets() {
    let credential = PaymentCredential {
        protocol: "x402".to_owned(),
        challenge_hash: "challenge-secret".to_owned(),
        credential_hash: "credential-secret".to_owned(),
        payload: json!({"signature":"signature-secret"}),
    };
    let adapter = crate::x402::X402Adapter::new(
        vec![crate::x402::X402Asset {
            fee_payer: None,
            network: "eip155:31337".to_owned(),
            contract: "0x1111111111111111111111111111111111111111".to_owned(),
            symbol: "USDC".to_owned(),
            decimals: 6,
        }],
        60,
    )
    .expect("x402 adapter");
    let dto =
        PaymentCredentialDto::for_http(credential, "https://merchant.example", &adapter).expect("HTTP credential");
    let debug = format!("{dto:?}");
    assert!(!debug.contains("challenge-secret"));
    assert!(!debug.contains("credential-secret"));
    assert!(!debug.contains("signature-secret"));
    assert!(!debug.contains(&dto.http.as_ref().expect("delivery").header_value));

    let encoded = dto.http.expect("delivery").header_value;
    let decoded = x402_types::util::b64::Base64Bytes::from(encoded.as_bytes())
        .decode()
        .expect("standard base64");
    assert_eq!(
        serde_json::from_slice::<Value>(&decoded).unwrap(),
        json!({"signature":"signature-secret"})
    );
    assert!(!String::from_utf8(decoded).unwrap().contains("challenge-secret"));
}
