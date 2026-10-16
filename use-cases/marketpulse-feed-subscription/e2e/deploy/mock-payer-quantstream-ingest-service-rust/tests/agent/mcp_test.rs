use super::{MCPOperation, ToolResult};

#[test]
fn tool_results_separate_business_data_from_payment_evidence() {
    let paid = ToolResult {
        body: br#"{"result":{"paid":true}}"#.to_vec(),
        payment_intent_id: Some("intent-rust-1".to_owned()),
    };
    assert_eq!(paid.payment_intent_id.as_deref(), Some("intent-rust-1"));
}

#[test]
fn operation_metadata_drives_only_declared_paid_tool_calls() {
    let message = serde_json::json!({
        "method": "tools/call",
        "params": {"_meta": {"agentvault": {
            "runId": "run-rust-1", "executionKey": "run-rust-1.realtime",
            "paid": true, "failAfterSettlement": false
        }}}
    });
    let operation = MCPOperation::from_message(&message).expect("operation metadata");
    assert!(operation.paid);
    assert_eq!(operation.execution_key, "run-rust-1.realtime");
}
