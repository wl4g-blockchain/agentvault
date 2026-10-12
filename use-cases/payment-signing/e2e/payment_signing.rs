use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agent_wallet::{
    model::{
        sign::{PaymentSignHistoryPage, PaymentSignRequest, PaymentSignResponse, EIP712_SECP256K1, PROTOCOL_VERSION},
        wallet::{CreateWalletRequest, Wallet, WalletList},
    },
    IPaymentSigner, LocalPaymentSigner,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use sha3::{Digest as _, Keccak256};
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn real_process_exposes_wallet_sign_history_and_management_planes() {
    let temp = TempDir::new().expect("create E2E directory");
    let master_key = temp.path().join("master.key");
    let store = temp.path().join("store");
    let history = store.join("history.jsonl");
    let socket = temp.path().join("run/agentvault.sock");
    let config = temp.path().join("agentvault.yaml");
    let api_port = reserve_port();
    let management_port = reserve_port();

    run([
        "master-key",
        "generate",
        "--output",
        master_key.to_str().expect("UTF-8 master-key path"),
    ]);
    write_config(&config, &store, &master_key, &socket, api_port, management_port);

    let child = command()
        .args(["--config", config.to_str().expect("UTF-8 config path"), "serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start walletd");
    let mut child = ChildGuard(Some(child));
    let client = reqwest::Client::new();
    let api = format!("http://127.0.0.1:{api_port}/api/v1");
    let management = format!("http://127.0.0.1:{management_port}");
    wait_until_ready(&client, &management, &socket, child.child_mut()).await;

    let created = create_and_list_wallet(&client, &api).await;
    let (request, response) = sign_payment(&socket, &created).await;
    assert_history_and_metrics(&client, &api, &management, request.request_id).await;
    rotate_and_delete_wallet(&client, &api, &created).await;

    let history_source = std::fs::read_to_string(history).expect("read durable history");
    assert!(!history_source.contains(&request.digest_b64));
    assert!(!history_source.contains(response.signature.as_deref().expect("signature")));
    child.stop();
}

async fn create_and_list_wallet(client: &reqwest::Client, api: &str) -> Wallet {
    let created = client
        .post(format!("{api}/wallet"))
        .json(&CreateWalletRequest { id: "payer".to_owned() })
        .send()
        .await
        .expect("create wallet request");
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created: Wallet = created.json().await.expect("decode created wallet");

    let listed: WalletList = client
        .get(format!("{api}/wallet"))
        .send()
        .await
        .expect("list wallets")
        .error_for_status()
        .expect("wallet list status")
        .json()
        .await
        .expect("decode wallet list");
    assert_eq!(listed.total, 1);
    assert_eq!(listed.items[0], created);
    created
}

async fn sign_payment(socket: &Path, wallet: &Wallet) -> (PaymentSignRequest, PaymentSignResponse) {
    let digest: [u8; 32] = Keccak256::digest(b"agentvault-e2e-payment").into();
    let now = unix_timestamp();
    let request = PaymentSignRequest {
        version: PROTOCOL_VERSION.to_owned(),
        request_id: Uuid::new_v4(),
        client_id: "flowgent-e2e".to_owned(),
        wallet_id: "payer".to_owned(),
        purpose: "x402.payment".to_owned(),
        scheme: EIP712_SECP256K1.to_owned(),
        digest_b64: BASE64.encode(digest),
        issued_at: now,
        expires_at: now + 30,
        metadata: BTreeMap::from([("payment_id".to_owned(), "e2e-001".to_owned())]),
    };
    let signer = LocalPaymentSigner::new(socket, Duration::from_secs(5), 16 * 1024);
    let response = signer
        .execute(request.clone())
        .await
        .expect("sign over real Unix socket");
    assert!(response.error.is_none(), "signing failed: {:?}", response.error);
    assert_eq!(response.address.as_deref(), Some(wallet.address.as_str()));
    assert_eq!(
        recover_address(&digest, response.signature.as_deref().expect("signature")),
        wallet.address
    );
    (request, response)
}

async fn assert_history_and_metrics(client: &reqwest::Client, api: &str, management: &str, request_id: Uuid) {
    let page: PaymentSignHistoryPage = client
        .get(format!("{api}/sign?wallet_id=payer&outcome=succeeded"))
        .send()
        .await
        .expect("search signing history")
        .error_for_status()
        .expect("signing history status")
        .json()
        .await
        .expect("decode signing history");
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].request_id, request_id);

    let metrics = client
        .get(format!("{management}/metrics"))
        .send()
        .await
        .expect("fetch metrics")
        .error_for_status()
        .expect("metrics status")
        .text()
        .await
        .expect("decode metrics");
    assert!(metrics.contains("agentvault_sign_requests_total"));
    assert!(metrics.contains("transport=\"local\""));
    assert!(metrics.contains("outcome=\"succeeded\""));
}

async fn rotate_and_delete_wallet(client: &reqwest::Client, api: &str, original: &Wallet) {
    let rotated: Wallet = client
        .put(format!("{api}/wallet/payer"))
        .send()
        .await
        .expect("rotate wallet")
        .error_for_status()
        .expect("rotate status")
        .json()
        .await
        .expect("decode rotated wallet");
    assert_ne!(rotated.address, original.address);
    let deleted = client
        .delete(format!("{api}/wallet/payer"))
        .send()
        .await
        .expect("delete wallet");
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
}

fn write_config(config: &Path, store: &Path, master_key: &Path, socket: &Path, api_port: u16, management_port: u16) {
    std::fs::write(
        config,
        format!(
            r"server:
  enabled: true
  service_name: agentvault-e2e
  host: 127.0.0.1
  port: {api_port}
  context_path: /api/v1
  shutdown_timeout: 2s
mgmt:
  enabled: true
  host: 127.0.0.1
  port: {management_port}
  context_path: /
  health:
    liveness_path: /healthz
    readiness_path: /readyz
  metrics:
    enabled: true
    path: /metrics
  otel:
    enabled: false
logging:
  mode: HUMAN
  level: warn
store:
  directory: {store}
  master_key_file: {master_key}
  sign_history_file: history.jsonl
security:
  max_request_bytes: 16384
  max_request_ttl_seconds: 30
  clock_skew_seconds: 5
  dedup_ttl_seconds: 300
  max_dedup_entries: 10000
  max_history_entries: 10000
  client_policies:
    - client_id_prefix: flowgent-
      wallet_ids: [payer]
      purposes: [x402.payment]
signers:
  local:
    enabled: true
    socket_path: {socket}
    max_connections: 16
    request_timeout: 5s
  mqtt:
    enabled: false
",
            store = yaml_string(store),
            master_key = yaml_string(master_key),
            socket = yaml_string(socket),
        ),
    )
    .expect("write E2E configuration");
}

async fn wait_until_ready(client: &reqwest::Client, management: &str, socket: &Path, child: &mut Child) {
    for _ in 0..150 {
        if let Some(status) = child.try_wait().expect("poll walletd") {
            panic!("walletd exited before readiness: {status}");
        }
        if socket.exists()
            && client
                .get(format!("{management}/readyz"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("walletd did not become ready");
}

fn reserve_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve local port")
        .local_addr()
        .expect("read local port")
        .port()
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_walletd"))
}

fn run<const N: usize>(arguments: [&str; N]) {
    let output = command().args(arguments).output().expect("run walletd command");
    assert!(
        output.status.success(),
        "walletd command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn yaml_string(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).expect("quote YAML path")
}

fn unix_timestamp() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_secs(),
    )
    .expect("Unix timestamp fits i64")
}

fn recover_address(digest: &[u8; 32], encoded: &str) -> String {
    let signature = hex::decode(encoded.strip_prefix("0x").expect("0x signature prefix")).expect("hex signature");
    assert_eq!(signature.len(), 65);
    let compact = Signature::try_from(&signature[..64]).expect("compact signature");
    let recovery_id = RecoveryId::try_from(signature[64] - 27).expect("recovery ID");
    let verifying_key = VerifyingKey::recover_from_prehash(digest, &compact, recovery_id).expect("recover signer");
    let public_key = verifying_key.to_encoded_point(false);
    let hash = Keccak256::digest(&public_key.as_bytes()[1..]);
    format!("0x{}", hex::encode(&hash[12..]))
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("walletd child")
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}
