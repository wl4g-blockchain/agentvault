use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agent_wallet::protocol::{SignRequest, SignResponse, EIP712_SECP256K1, PROTOCOL_VERSION};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use serde_json::json;
use sha3::{Digest, Keccak256};
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn walletd_signs_over_local_transport() {
    let temp = TempDir::new().expect("create temporary E2E directory");
    let master_key = temp.path().join("master.key");
    let store = temp.path().join("store");
    let socket = temp.path().join("run/wallet.sock");
    let config = temp.path().join("wallet.toml");

    run([
        "master-key",
        "generate",
        "--output",
        master_key.to_str().expect("UTF-8 master-key path"),
    ]);
    write_config(&config, &store, &master_key, &socket);
    let expected_address = generate_key(&config);

    let child = command()
        .args(["--config", config.to_str().expect("UTF-8 config path"), "serve"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start walletd");
    let mut child = ChildGuard(Some(child));
    wait_for_socket(&socket, child.0.as_mut().expect("walletd child"));

    let digest: [u8; 32] = Keccak256::digest(b"wallet-e2e-payment").into();
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_secs(),
    )
    .expect("Unix timestamp fits i64");
    let request: SignRequest = serde_json::from_value(json!({
        "version": PROTOCOL_VERSION,
        "request_id": Uuid::new_v4(),
        "client_id": "flowgent-e2e",
        "wallet_id": "payer",
        "purpose": "x402.payment",
        "scheme": EIP712_SECP256K1,
        "digest_b64": BASE64.encode(digest),
        "issued_at": now,
        "expires_at": now + 30,
    }))
    .expect("construct signing request");

    let mut stream = UnixStream::connect(&socket).expect("connect to walletd socket");
    serde_json::to_writer(&mut stream, &request).expect("write signing request");
    stream.write_all(b"\n").expect("terminate request line");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read signing response");
    let response: SignResponse = serde_json::from_str(&response).expect("decode signing response");
    assert!(response.error.is_none(), "signing failed: {:?}", response.error);
    assert_eq!(response.address.as_deref(), Some(expected_address.as_str()));
    assert_eq!(
        recover_address(&digest, response.signature.as_deref().expect("signature")),
        expected_address
    );

    child.stop();
}

fn write_config(config: &Path, store: &Path, master_key: &Path, socket: &Path) {
    std::fs::write(
        config,
        format!(
            r#"
[store]
directory = {store}
master_key_file = {master_key}

[security]
max_request_bytes = 16384
max_request_ttl_seconds = 30
clock_skew_seconds = 5
dedup_ttl_seconds = 300
max_dedup_entries = 10000

[[security.client_policies]]
client_id_prefix = "flowgent-"
wallet_ids = ["payer"]
purposes = ["x402.payment"]

[transports.local]
enabled = true
socket_path = {socket}

[transports.mqtt]
enabled = false
"#,
            store = quoted_path(store),
            master_key = quoted_path(master_key),
            socket = quoted_path(socket),
        ),
    )
    .expect("write E2E configuration");
}

fn generate_key(config: &Path) -> String {
    let generated = command()
        .args([
            "--config",
            config.to_str().expect("UTF-8 config path"),
            "key",
            "generate",
            "payer",
        ])
        .output()
        .expect("run key generation");
    assert!(
        generated.status.success(),
        "key generation failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let fields: Vec<_> = String::from_utf8(generated.stdout)
        .expect("UTF-8 key output")
        .trim()
        .split('\t')
        .map(str::to_owned)
        .collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], "payer");
    assert_eq!(fields[1], "secp256k1");
    fields[2].clone()
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

fn quoted_path(path: &Path) -> String {
    toml::Value::String(path.to_string_lossy().into_owned()).to_string()
}

fn wait_for_socket(socket: &Path, child: &mut Child) {
    for _ in 0..100 {
        if socket.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll walletd") {
            panic!("walletd exited before creating its socket: {status}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("walletd did not create {}", socket.display());
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
