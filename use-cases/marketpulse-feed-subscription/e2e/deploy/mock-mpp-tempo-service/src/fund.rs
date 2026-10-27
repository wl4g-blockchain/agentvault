//! Localnet faucet. This module is shipped only in the E2E provider image.
use alloy::{eips::Encodable2718 as _, network::ReceiptResponse as _, providers::Provider as _};
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall as _;
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tempo_primitives::{transaction::Call, TempoTransaction};

alloy_sol_types::sol! {
    interface Tip20 { function transfer(address to, uint256 amount) external returns (bool); }
}

#[cfg(test)]
#[path = "../tests/fund_test.rs"]
mod tests;

#[derive(Clone)]
pub struct FundingService {
    provider: mpp::server::TempoProvider,
    lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FundingRequest {
    address: Address,
}

impl FundingService {
    pub fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Ok(url) = std::env::var("TEMPO_RPC_URL") else {
            return Ok(None);
        };
        Ok(Some(Self {
            provider: mpp::server::tempo_provider(&url)?,
            lock: std::sync::Arc::default(),
        }))
    }

    pub fn router(self) -> Router {
        Router::new().route("/fund", post(Self::fund)).with_state(self)
    }

    async fn fund(State(state): State<Self>, Json(request): Json<FundingRequest>) -> (StatusCode, Json<Value>) {
        let _guard = state.lock.lock().await;
        match state.transfer(request.address).await {
            Ok(hash) => (
                StatusCode::OK,
                Json(json!({"transaction": hash, "amount":"10000000000"})),
            ),
            Err(error) => {
                eprintln!("localnet funding rejected: {error}");
                (StatusCode::BAD_GATEWAY, Json(json!({"error":"funding_failed"})))
            }
        }
    }

    async fn transfer(&self, recipient: Address) -> Result<String, Box<dyn std::error::Error>> {
        let key = hex::decode(std::env::var("TEMPO_DEV_PRIVATE_KEY")?)?;
        let signer = PrivateKeySigner::from_bytes(&B256::from_slice(&key))?;
        let payer = signer.address();
        if self.provider.get_chain_id().await? != 1337 {
            return Err("faucet restricted to localnet 1337".into());
        }
        let gas = self.provider.get_gas_price().await?;
        let transaction = TempoTransaction {
            chain_id: 1337,
            nonce: self.provider.get_transaction_count(payer).await?,
            gas_limit: 5_000_000,
            max_fee_per_gas: gas,
            max_priority_fee_per_gas: gas,
            calls: vec![Call {
                to: TxKind::Call("0x20c0000000000000000000000000000000000000".parse()?),
                value: U256::ZERO,
                input: Bytes::from(Tip20::transferCall::new((recipient, U256::from(10_000_000_000u64))).abi_encode()),
            }],
            ..TempoTransaction::default()
        };
        let signature = signer.sign_hash_sync(&transaction.signature_hash())?;
        let raw = format!(
            "0x{}",
            hex::encode(transaction.into_signed(signature.into()).encoded_2718())
        );
        let hash: B256 = self
            .provider
            .raw_request("eth_sendRawTransaction".into(), (raw,))
            .await?;
        for _ in 0..120 {
            if let Some(receipt) = self.provider.get_transaction_receipt(hash).await? {
                if !receipt.status() {
                    return Err("localnet funding reverted".into());
                }
                return Ok(format!("{hash:#x}"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Err("localnet funding confirmation timed out".into())
    }
}
