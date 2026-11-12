use crate::{A2aCall, PaymentProtocolFactory};
use serde_json::{json, Value};
use std::collections::BTreeSet;
#[test]
fn shared_native_contract_and_unsupported_boundaries() {
    let fixture: Value = serde_json::from_str(include_str!("../../../tests/payment_protocol_a2a.json")).unwrap();
    let registry = PaymentProtocolFactory::new()
        .create(&["x402".into(), "mpp".into()])
        .unwrap();
    let mut expected = BTreeSet::new();
    for t in ["grpc", "mqtt"] {
        for p in ["x402", "mpp"] {
            for role in ["payer", "payee"] {
                expected.insert(format!("{t}/{p}/{role}"));
            }
        }
    }
    let rows = fixture["matrix"].as_array().unwrap();
    let actual: BTreeSet<_> = rows
        .iter()
        .map(|r| {
            format!(
                "{}/{}/{}",
                r["transport"].as_str().unwrap(),
                r["protocol"].as_str().unwrap(),
                r["role"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(rows.len(), 8);
    assert_eq!(actual, expected);
    for (name, contract) in fixture["protocols"].as_object().unwrap() {
        assert_eq!(
            registry
                .a2a_challenge(&contract["challenge_response"], 65536)
                .unwrap()
                .unwrap()
                .0
                .name(),
            name
        );
        let mut request = contract["credential_request"].clone();
        let mut call = A2aCall::parse(&serde_json::to_vec(&request).unwrap(), 65536)
            .unwrap()
            .unwrap();
        assert_eq!(registry.a2a_credential(&request, 65536).unwrap().unwrap().0, *name);
        assert_eq!(
            registry
                .a2a_evidence(&contract["settled_response"], 65536)
                .unwrap()
                .unwrap()
                .0,
            *name
        );
        registry.a2a_strip_credentials(&mut request).unwrap();
        assert!(registry.a2a_credential(&request, 65536).unwrap().is_none());
        assert_eq!(
            request["params"]["message"]["metadata"],
            json!({"application.label":"keep"})
        );
        let mut task = A2aCall::task(&contract["settled_response"]).unwrap();
        task.context_id = "wrong".into();
        assert!(call.bind(&task).is_err());
    }
    for method in ["message/stream", "SendMessage", "SendStreamingMessage"] {
        let mut request = fixture["request"].clone();
        request["method"] = json!(method);
        assert!(A2aCall::parse(&serde_json::to_vec(&request).unwrap(), 65536).is_err());
    }
    let mut request = fixture["protocols"]["x402"]["credential_request"].clone();
    request["_meta"] = fixture["protocols"]["mpp"]["credential_request"]["_meta"].clone();
    assert!(registry.a2a_credential(&request, 65536).is_err());
    let mut embedded = fixture["protocols"]["x402"]["challenge_response"].clone();
    embedded["result"]["status"]["message"]["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("x402.payment.required");
    assert!(registry.a2a_challenge(&embedded, 65536).is_err());
}

#[test]
fn canonical_body_is_independent_of_metadata_and_task_insertion_order() {
    let first=br#"{"jsonrpc":"2.0","id":"run-1","method":"message/send","params":{"message":{"kind":"message","messageId":"run-1","role":"user","parts":[{"kind":"text","text":"news"}],"metadata":{"application.b":2,"application.a":1},"taskId":"app-task","contextId":"app-context"}},"_meta":{}}"#;
    let retry=br#"{"jsonrpc":"2.0","id":"run-1","method":"message/send","params":{"message":{"kind":"message","messageId":"run-1","role":"user","parts":[{"kind":"text","text":"news"}],"taskId":"app-task","contextId":"app-context","metadata":{"application.a":1,"application.b":2}}},"_meta":{}}"#;
    let first = A2aCall::parse(first, 65536).unwrap().unwrap();
    let retry = A2aCall::parse(retry, 65536).unwrap().unwrap();
    assert_eq!(first.body().unwrap(), retry.body().unwrap());
    let mut changed = retry;
    changed.wire["params"]["message"]["contextId"] = json!("different-context");
    assert_ne!(first.body().unwrap(), changed.body().unwrap());
    changed.wire["params"]["message"]["metadata"]["application.large-integer"] = json!(u64::MAX);
    let decoded: Value = serde_json::from_slice(&changed.body().unwrap()).unwrap();
    assert_eq!(
        decoded["params"]["message"]["metadata"]["application.large-integer"],
        json!(u64::MAX)
    );
}
