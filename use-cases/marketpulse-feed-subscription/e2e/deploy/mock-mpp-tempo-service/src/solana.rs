//! Route x402 by trusted chain configuration; Solana uses the official facilitator.
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{str::FromStr as _, sync::Arc};
use x402_chain_solana::{
    chain::{SolanaChainProvider, SolanaChainReference},
    v2_solana_exact::{V2SolanaExactFacilitator, V2SolanaExactFacilitatorConfig},
};
use x402_types::scheme::X402SchemeFacilitator as _;

type OfficialFacilitator = V2SolanaExactFacilitator<SolanaChainProvider>;

#[derive(Clone)]
pub struct SolanaFacilitator {
    inner: Arc<OfficialFacilitator>,
    network: String,
    client: reqwest::Client,
    evm_url: String,
}

#[cfg(test)]
#[path = "../tests/solana_test.rs"]
mod tests;

impl SolanaFacilitator {
    pub async fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Ok(rpc) = std::env::var("SOLANA_RPC_URL") else {
            return Ok(None);
        };
        let reference = std::env::var("SOLANA_CHAIN_REFERENCE")?;
        let bytes: Vec<u8> = serde_json::from_str(&std::env::var("SOLANA_FACILITATOR_KEYPAIR")?)?;
        let key = solana_keypair::Keypair::try_from(bytes.as_slice())?;
        let provider = SolanaChainProvider::new(
            key,
            rpc,
            None,
            SolanaChainReference::from_str(&reference)?,
            400_000,
            1_000_000,
        )
        .await?;
        let evm_url = std::env::var("EVM_FACILITATOR_URL")?;
        Ok(Some(Self {
            inner: Arc::new(V2SolanaExactFacilitator::new(
                provider,
                V2SolanaExactFacilitatorConfig::default(),
            )),
            network: format!("solana:{reference}"),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_mins(1))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            evm_url,
        }))
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/x402/verify", post(Self::verify))
            .route("/x402/settle", post(Self::settle))
            .with_state(self)
    }

    async fn verify(State(state): State<Self>, Json(request): Json<Value>) -> (StatusCode, Json<Value>) {
        state.execute(request, false).await
    }

    async fn settle(State(state): State<Self>, Json(request): Json<Value>) -> (StatusCode, Json<Value>) {
        state.execute(request, true).await
    }

    async fn execute(&self, request: Value, settle: bool) -> (StatusCode, Json<Value>) {
        let operation = if settle { "settle" } else { "verify" };
        let network = request["paymentRequirements"]["network"].as_str().unwrap_or_default();
        let result = if network == "eip155:31337" {
            self.proxy_evm(operation, &request).await
        } else if network == self.network {
            self.execute_solana(settle, request.clone()).await
        } else {
            Err("unsupported network".to_owned())
        };
        let (status, response) = match result {
            Ok(value) => (StatusCode::OK, value),
            Err(error) => {
                eprintln!("x402 {operation} rejected: {error}");
                (StatusCode::BAD_REQUEST, Self::rejection(settle, network))
            }
        };
        println!(
            "{}",
            json!({"event":format!("manual.x402.{operation}"), "network":network,
            "valid":response["isValid"], "success":response["success"], "transaction":response["transaction"]})
        );
        (status, Json(response))
    }

    fn rejection(settle: bool, network: &str) -> Value {
        if settle {
            json!({"success": false, "errorReason": "payment_rejected", "network": network})
        } else {
            json!({"isValid": false, "invalidReason": "payment_rejected"})
        }
    }

    async fn execute_solana(&self, settle: bool, request: Value) -> Result<Value, String> {
        let raw = serde_json::value::RawValue::from_string(request.to_string()).map_err(|e| e.to_string())?;
        let request = raw.into();
        if settle {
            self.inner
                .settle(&request)
                .await
                .map(|r| r.0)
                .map_err(|e| e.to_string())
        } else {
            self.inner
                .verify(&request)
                .await
                .map(|r| r.0)
                .map_err(|e| e.to_string())
        }
    }

    async fn proxy_evm(&self, operation: &str, request: &Value) -> Result<Value, String> {
        let mut response = self
            .client
            .post(format!("{}/{operation}", self.evm_url.trim_end_matches('/')))
            .json(request)
            .send()
            .await
            .map_err(|_| "evm_upstream_transport_error".to_owned())?;
        let status = response.status();
        if !status.is_success() {
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "evm_upstream_body_error")? {
                if body.len() + chunk.len() > 8192 {
                    return Err(format!(
                        "evm_upstream_status={} reason=response_too_large",
                        status.as_u16()
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            return Err(format!(
                "evm_upstream_status={} reason={}",
                status.as_u16(),
                Self::upstream_error_class(&body)
            ));
        }
        response
            .json()
            .await
            .map_err(|_| "evm_upstream_invalid_json".to_owned())
    }

    // Classify the response internally; never log an upstream body that could echo
    // the credential. Only these fixed low-cardinality labels leave the boundary.
    fn upstream_error_class(body: &[u8]) -> &'static str {
        let body = String::from_utf8_lossy(body).to_ascii_lowercase();
        for (fragments, class) in [
            (
                &["accepted does not match", "accepted_requirements_mismatch"][..],
                "accepted_requirements_mismatch",
            ),
            (
                &["insufficient_funds", "balance is not enough", "insufficient funds"][..],
                "insufficient_funds",
            ),
            (
                &["not yet valid", "authorization not active", "invalid_payment_early"][..],
                "authorization_not_active",
            ),
            (
                &[
                    "authorization is expired",
                    "authorization expired",
                    "invalid_payment_expired",
                ][..],
                "authorization_expired",
            ),
            (&["signature", "invalid recovery", "ecrecover"][..], "invalid_signature"),
            (
                &[
                    "missing field",
                    "invalid format",
                    "invalid_format",
                    "deserializ",
                    "invalid type",
                ][..],
                "invalid_wire_format",
            ),
            (
                &["chain id", "unsupported chain", "chain_id_mismatch"][..],
                "network_mismatch",
            ),
            (&["recipient", "asset_mismatch"][..], "recipient_or_asset_mismatch"),
            (
                &["revert", "contract call failed", "onchain"][..],
                "onchain_call_failed",
            ),
            (
                &["transport", "connection", "timeout", "rpc"][..],
                "rpc_transport_error",
            ),
        ] {
            if fragments.iter().any(|fragment| body.contains(fragment)) {
                return class;
            }
        }
        "unclassified_upstream_error"
    }
}
