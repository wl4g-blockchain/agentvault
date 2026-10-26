use crate::{payee_a2a::A2aTaskStore, *};
use a2a_types::Task;
use async_trait::async_trait;
use axum::{
    body::Bytes,
    http::{HeaderMap, Method},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
struct Tasks(Mutex<Option<Task>>);
#[async_trait]
impl A2aTaskStore for Tasks {
    async fn get(&self, id: &str) -> crate::Result<Option<Task>> {
        Ok(self.0.lock().unwrap().clone().filter(|task| task.id == id))
    }
    async fn save(&self, task: &Task) -> crate::Result<()> {
        *self.0.lock().unwrap() = Some(task.clone());
        Ok(())
    }
}
#[derive(Clone)]
struct Vault {
    protocol: String,
    contract: Value,
    origin: Arc<Mutex<String>>,
    hashes: Arc<Mutex<Vec<String>>>,
    reports: Arc<AtomicUsize>,
}
impl Vault {
    fn intent(&self, inbound: bool, settled: bool) -> Value {
        json!({"id":if inbound{"in-1"}else{"out-1"},"scenario_id":"scenario","run_id":"run-1","price_id":"price","wallet_id":"wallet","policy_id":"policy",
            "direction":if inbound{"inbound"}else{"outbound"},"protocol":self.protocol,"asset":{"network":"eip155:42431","asset":"USDC","decimals":6},
            "recipient":"0x1111111111111111111111111111111111111111","amount":"1000","settled_amount":if settled{"1000"}else{"0"},
            "resource":if inbound{"a2a://feed".into()}else{format!("{}/a2a",self.origin.lock().unwrap())},"challenge_hash":"challenge-1",
            "request_hash":if inbound{self.hashes.lock().unwrap().last().cloned().unwrap_or_default()}else{"request-1".into()},
            "status":if settled{"settled"}else if inbound{"pending_settlement"}else{"signed"},"reservation_status":if inbound{"unreserved"}else{"reserved"},"reserved_amount":if inbound{"0"}else{"1000"},"created_at":1,"updated_at":2})
    }
    fn challenge(&self) -> Value {
        let mut value = json!({"scenario_id":"scenario","price_id":"price","resource":"a2a://feed","protocol":self.protocol,
            "asset":{"network":"eip155:42431","asset":"USDC","decimals":6},"amount":"1000","recipient":"0x1111111111111111111111111111111111111111",
            "nonce":"challenge-0123456789abcdef","challenge_hash":"challenge-1","expires_at":2_000_000_060_u64});
        value["payload"] = if self.protocol == "x402" {
            json!({"payment_required":self.contract["challenge_response"]["result"]["structuredContent"]})
        } else {
            let http = MppAdapter
                .mcp_challenge(&self.contract["challenge_response"], 65536)
                .unwrap()
                .unwrap();
            json!({"mcp_challenge":self.contract["challenge_response"]["error"]["data"]["challenges"][0],
                "www_authenticate": http.headers[mpp::WWW_AUTHENTICATE_HEADER]})
        };
        value
    }
    fn authorized(&self) -> Value {
        let payload = if self.protocol == "x402" {
            self.contract["credential_meta"]["x402/payment"].clone()
        } else {
            self.contract["credential_meta"]["org.paymentauth/credential"].clone()
        };
        let header = if self.protocol == "x402" {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap())
        } else {
            MppAdapter
                .presented_mcp_credential(&self.contract["credential_meta"], 65536)
                .unwrap()
                .unwrap()["authorization"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        json!({"authorization":{"outcome":"authorized","intent_id":"out-1","reserved_amount":"1000"},"intent":self.intent(false,false),"credential":{"protocol":self.protocol,"challenge_hash":"challenge-1","credential_hash":"credential-1","payload":payload,
            "http":{"bound_origin":*self.origin.lock().unwrap(),"header_name":if self.protocol=="x402"{"Payment-Signature"}else{"Payment-Authorization"},"header_value":header}}})
    }
}
impl VaultTransport for Vault {
    async fn send(&self, request: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let value = match request.path.as_str() {
            "/api/v1/payee/challenges" => {
                self.hashes
                    .lock()
                    .unwrap()
                    .push(body["request_hash"].as_str().unwrap().into());
                self.challenge()
            }
            "/api/v1/payee/verify" => {
                json!({"valid":true,"verification_ref":"verified","intent":self.intent(true,false)})
            }
            "/api/v1/payee/settle" => {
                json!({"intent":self.intent(true,true),"receipt":{"intent_id":"in-1","scenario_id":"scenario","direction":"inbound","protocol":self.protocol,"asset":{"network":"eip155:42431","asset":"USDC","decimals":6},"recipient":"0x1111111111111111111111111111111111111111","payer":"0x3333333333333333333333333333333333333333","requested_amount":"1000","settled_amount":"1000","status":"settled","tx_hash":"0x1111","evidence_hash":"evidence-1","confirmed_at":2_000_000_000_u64}})
            }
            "/api/v1/payments/authorize" => self.authorized(),
            "/api/v1/payments/out-1/outbound-response" => {
                self.reports.fetch_add(1, Ordering::Relaxed);
                json!({"intent":self.intent(false,false),"accepted":true,"replayed":false})
            }
            path => panic!("unexpected Vault path {path}"),
        };
        Ok(TransportResponse {
            headers: std::collections::BTreeMap::default(),
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
        })
    }
}
#[tokio::test]
async fn both_roles_both_protocols_preserve_task_and_business_failure_receipt() {
    run_a2a_cases(true).await;
}

#[tokio::test]
async fn a2a_body_over_http_402_uses_http_credentials_and_receipts() {
    run_a2a_cases(false).await;
}

async fn run_a2a_cases(message_level: bool) {
    let carrier: Value = serde_json::from_str(include_str!("../../../tests/payment_protocol_a2a.json")).unwrap();
    let protocols: Value = serde_json::from_str(include_str!("../../../tests/payment_protocol_mcp.json")).unwrap();
    for protocol in ["x402", "mpp"] {
        for failed in [false, true] {
            let vault = Vault {
                protocol: protocol.into(),
                contract: protocols["protocols"][protocol].clone(),
                origin: Arc::default(),
                hashes: Arc::default(),
                reports: Arc::default(),
            };
            let tasks = Arc::new(Tasks::default());
            let task: Task = serde_json::from_value(
                json!({"kind":"task","id":"task-1","contextId":"context-1","status":{"state":"submitted"}}),
            )
            .unwrap();
            tasks.save(&task).await.unwrap();
            let executions = Arc::new(AtomicUsize::new(0));
            let calls = executions.clone();
            let route = AxumPayeeRoute {
                method: Method::POST,
                path: "/a2a".into(),
                scenario_id: "scenario".into(),
                price_id: "price".into(),
                resource: "a2a://feed".into(),
                origin: "https://merchant.test".into(),
                protocol: protocol.into(),
                mcp_tool: None,
            };
            let layer = PayeeMiddleware::new(vault.clone(), PayeeConfig::default())
                .unwrap()
                .axum(vec![route]);
            let layer = if message_level {
                layer.with_a2a_task_store("/a2a", tasks.clone())
            } else {
                layer
            };
            let app=Router::new().route("/a2a",post(move|headers:HeaderMap,body:Bytes|{let calls=calls.clone();async move{
            calls.fetch_add(1,Ordering::Relaxed);assert_eq!(headers["authorization"],"Bearer application-token");
            let call=A2aCall::parse(&body,65536).unwrap().unwrap();assert_eq!(call.params().unwrap().message.task_id.as_deref(),Some("task-1"));
            assert_eq!(call.wire["params"]["message"]["metadata"],json!({"application.label":"keep"}));
            Json(if failed{json!({"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"business failed"}})}else{json!({"jsonrpc":"2.0","id":1,"result":{"kind":"task","id":"task-1","contextId":"context-1","status":{"state":"completed"}}})})
        }})).layer(layer);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            *vault.origin.lock().unwrap() = origin.clone();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let payer = PayerMiddleware::new(vault.clone(), PayerConfig::new("scenario").unwrap())
                .unwrap()
                .standard()
                .client()
                .unwrap();
            let response = payer
                .post(format!("{origin}/a2a"))
                .header("content-type", "application/json")
                .header("authorization", "Bearer application-token")
                .body(carrier["request"].to_string())
                .with_payment(PayerRequestContext::new("run-1").unwrap())
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{body}");
            let value: Value = serde_json::from_str(&body).unwrap();
            if failed && !message_level {
                assert_eq!(value["error"]["code"], -32603);
            } else {
                assert_eq!(
                    value["result"]["status"]["state"],
                    if failed { "failed" } else { "completed" }
                );
            }
            assert_eq!(executions.load(Ordering::Relaxed), 1);
            assert_eq!(vault.reports.load(Ordering::Relaxed), 1);
            let hashes = vault.hashes.lock().unwrap();
            assert_eq!(hashes.len(), 2);
            assert_eq!(hashes[0], hashes[1]);
            server.abort();
        }
    }
}
