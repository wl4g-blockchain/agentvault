use super::*;
use serde_json::json;

fn spec() -> serde_json::Value {
    json!({
        "id":"price", "scenario_id":"scenario", "policy_id":"policy", "resource":"https://feed.example/data",
        "protocols":["x402"],"asset":{"network":"eip155:31337","asset":"USDC","token_contract":"0x1111111111111111111111111111111111111111","decimals":6},
        "amount":"10","wallet_id":"wallet","enabled":true
    })
}

#[test]
fn price_update_preserves_server_timestamps_and_rejects_owner_changes() {
    let stored = serde_json::from_value::<CreatePriceRequest>(spec())
        .unwrap()
        .0
        .into_domain(4);
    let request: UpdatePriceRequest = serde_json::from_value(json!({"price":spec(),"expected_updated_at":4})).unwrap();
    let (updated, revision) = request.into_domain("price", &stored, 4).unwrap();
    assert_eq!((updated.created_at, updated.updated_at, revision), (4, 5, 4));
    let mut other = spec();
    other["scenario_id"] = json!("other");
    let request: UpdatePriceRequest = serde_json::from_value(json!({"price":other,"expected_updated_at":4})).unwrap();
    assert!(request.into_domain("price", &stored, 5).is_err());
}

#[test]
fn price_requests_reject_client_timestamps_and_unknown_revisions() {
    let mut untrusted = spec();
    untrusted["created_at"] = json!(1);
    assert!(serde_json::from_value::<CreatePriceRequest>(untrusted).is_err());
    assert!(serde_json::from_value::<UpdatePriceRequest>(json!({"price":spec()})).is_err());
    assert!(serde_json::from_value::<DeletePriceRequest>(json!({"expected_updated_at":4,"force":true})).is_err());
}
