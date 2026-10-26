use crate::*;
use serde_json::Value;
#[derive(Clone)]
struct Unused;
impl VaultTransport for Unused {
    async fn send(&self, _: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        panic!("Vault must not be called")
    }
}
#[tokio::test]
async fn extension_acknowledgement_required_before_authorization() {
    let fixture: Value = serde_json::from_str(include_str!("../../../tests/payment_protocol_a2a.json")).unwrap();
    let core = PayerMiddleware::new(Unused, PayerConfig::new("scenario").unwrap()).unwrap();
    let request = PayerHttpRequest {
        method: "POST".into(),
        url: "https://merchant.test/a2a".into(),
        origin: "https://merchant.test".into(),
        headers: std::collections::BTreeMap::default(),
        body: serde_json::to_vec(&fixture["request"]).unwrap(),
        follow_redirects: false,
    };
    let response = PayerHttpResponse {
        status: 200,
        headers: std::collections::BTreeMap::default(),
        body: serde_json::to_vec(&fixture["protocols"]["x402"]["challenge_response"]).unwrap(),
    };
    assert!(core
        .authorize_a2a(&request, &response, &PayerRequestContext::new("run-1").unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("acknowledged"));
    let uri = fixture["x402_extension"].as_str().unwrap();
    assert_eq!(core.a2a_extensions(uri), uri);
}
