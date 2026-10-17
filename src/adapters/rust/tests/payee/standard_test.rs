use crate::{PayeeOperation, StandardPayeeRequest};

#[test]
fn standard_adapter_preserves_business_operation_identity() {
    let request = StandardPayeeRequest {
        operation: PayeeOperation {
            scenario_id: "scenario".to_owned(),
            run_id: "run".to_owned(),
            offer_id: "offer".to_owned(),
            resource: "mcp://marketpulse/realtime".to_owned(),
            origin: "https://marketpulse.example".to_owned(),
            protocol: "x402".to_owned(),
            request_hash: "hash".to_owned(),
            credential_payload: None,
        },
    };
    assert_eq!(request.operation.run_id, "run");
}
