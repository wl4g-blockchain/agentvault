//! Official MPP validation/broadcast with receipt-based recovery, never a fabricated settlement.

use std::{collections::BTreeMap, env, sync::Arc, time::Duration};

use agentvault::model::{Amount, AssetIdentity, SettlementProviderResult};
use alloy::{network::ReceiptResponse as _, providers::Provider as _};
use alloy_primitives::{Address, Bytes, B256};
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use mpp::{protocol::methods::tempo::TempoChargeExt as _, ChargeMethod as _};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::ServiceError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderRequest {
    pub protocol: String,
    pub run_id: String,
    pub payment_payload: Value,
    pub payment_requirements: Value,
}

#[derive(Clone)]
pub(crate) struct TempoFacilitator {
    method: mpp::server::TempoChargeMethod<mpp::server::TempoProvider>,
    provider: mpp::server::TempoProvider,
    asset: AssetIdentity,
    recipient: Address,
    settle_lock: Arc<Mutex<()>>,
}

struct ValidatedCharge {
    credential: mpp::PaymentCredential,
    charge: mpp::ChargeRequest,
    transaction_hash: B256,
    payer: Address,
    amount: Amount,
}

impl TempoFacilitator {
    pub(crate) async fn from_env() -> Result<Option<Self>, ServiceError> {
        let Ok(rpc) = env::var("TEMPO_RPC_URL") else {
            return Ok(None);
        };
        let chain_id = env::var("TEMPO_CHAIN_ID")
            .unwrap_or_else(|_| "1337".to_owned())
            .parse::<u64>()?;
        let token = env::var("TEMPO_TOKEN")
            .unwrap_or_else(|_| "0x20c0000000000000000000000000000000000000".to_owned())
            .parse::<Address>()?;
        let recipient = env::var("TEMPO_RECIPIENT")?.parse::<Address>()?;
        if token.is_zero() || recipient.is_zero() {
            return Err("Tempo token and recipient must be nonzero".into());
        }
        let provider = mpp::server::tempo_provider(&rpc)?;
        if provider.get_chain_id().await? != chain_id {
            return Err("Tempo provider network does not match TEMPO_CHAIN_ID".into());
        }
        Ok(Some(Self {
            method: mpp::server::TempoChargeMethod::new(provider.clone()).with_store(Arc::new(mpp::MemoryStore::new())),
            provider,
            asset: AssetIdentity {
                network: format!("eip155:{chain_id}"),
                asset: env::var("TEMPO_ASSET").unwrap_or_else(|_| "pathUSD".to_owned()),
                token_contract: Some(format!("{token:#x}")),
                decimals: 6,
            },
            recipient,
            settle_lock: Arc::new(Mutex::new(())),
        }))
    }

    pub(crate) fn router(self) -> Router {
        Router::new()
            .route("/verify", post(Self::verify))
            .route("/settle", post(Self::settle))
            .with_state(self)
    }

    async fn verify(
        State(provider): State<Self>,
        Json(request): Json<ProviderRequest>,
    ) -> (StatusCode, Json<SettlementProviderResult>) {
        provider.execute(&request, false).await
    }

    async fn settle(
        State(provider): State<Self>,
        Json(request): Json<ProviderRequest>,
    ) -> (StatusCode, Json<SettlementProviderResult>) {
        // Recheck the receipt under one lock so concurrent redelivery cannot broadcast twice.
        // The signed transaction hash also survives process restarts as the chain identity.
        let _guard = provider.settle_lock.lock().await;
        provider.execute(&request, true).await
    }

    async fn execute(&self, request: &ProviderRequest, settle: bool) -> (StatusCode, Json<SettlementProviderResult>) {
        let validated = match self.validate_charge(request).await {
            Ok(value) => value,
            Err(reason) => return (StatusCode::OK, Json(Self::rejected(reason))),
        };
        // Once validation succeeds, RPC/broadcast failures are unknown outcomes.
        // Returning 503 makes Vault reconcile by read-only verify, never treat an
        // already broadcast transfer as a definite payment rejection.
        let (status, result) = match self.execute_checked(&validated, &request.run_id, settle).await {
            Ok(result) => (StatusCode::OK, result),
            Err(reason) => (StatusCode::SERVICE_UNAVAILABLE, Self::rejected(reason)),
        };
        tracing::info!(
            event = if settle { "manual.mpp.settle" } else { "manual.mpp.verify" },
            run_id = %request.run_id, valid = result.valid, settled = result.settled,
            transaction = result.transaction.as_deref().unwrap_or(""),
            "external Tempo payment operation"
        );
        (status, Json(result))
    }

    async fn execute_checked(
        &self,
        validated: &ValidatedCharge,
        run_id: &str,
        settle: bool,
    ) -> Result<SettlementProviderResult, String> {
        let hash = validated.transaction_hash;
        let mut receipt = self
            .provider
            .get_transaction_receipt(hash)
            .await
            .map_err(|_| "Tempo receipt query failed".to_owned())?;
        if settle && receipt.is_none() {
            let receipt_wire = self
                .method
                .broadcast(&validated.credential, &validated.charge)
                .await
                .map_err(|_| "Tempo broadcast failed; recover with verify before retrying settle".to_owned())?;
            if receipt_wire.reference.parse::<B256>().ok() != Some(hash) {
                return Err("Tempo broadcast returned a different transaction".to_owned());
            }
            tracing::info!(event = "manual.mpp.broadcast", run_id = %run_id,
                transaction = %hash, "broadcast signed Tempo transaction");
            for _ in 0..100 {
                receipt = self
                    .provider
                    .get_transaction_receipt(hash)
                    .await
                    .map_err(|_| "Tempo confirmation query failed".to_owned())?;
                if receipt.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if receipt.is_none() {
                return Err("Tempo transaction was broadcast but remains unconfirmed; recover with verify".to_owned());
            }
        }
        let mut extensions = BTreeMap::new();
        let confirmations = if let Some(receipt) = receipt {
            if !receipt.status() || receipt.from() != validated.payer {
                return Err("confirmed Tempo receipt status or payer mismatches".to_owned());
            }
            if receipt.fee_payer != validated.payer
                || receipt.fee_token.map(|token| format!("{token:#x}")) != self.asset.token_contract
            {
                return Err("confirmed Tempo receipt fee identity mismatches".to_owned());
            }
            let fee = tempo_primitives::transaction::calc_gas_balance_spending(
                receipt.gas_used(),
                receipt.effective_gas_price(),
            );
            extensions.insert("gas_used".to_owned(), json!(receipt.gas_used()));
            extensions.insert(
                "effective_gas_price".to_owned(),
                json!(receipt.effective_gas_price().to_string()),
            );
            extensions.insert("gas_fee".to_owned(), json!(fee.to_string()));
            extensions.insert("fee_payer".to_owned(), json!(format!("{:#x}", receipt.fee_payer)));
            extensions.insert("fee_token".to_owned(), json!(self.asset.token_contract));
            let current = self
                .provider
                .get_block_number()
                .await
                .map_err(|_| "Tempo block query failed".to_owned())?;
            let block = receipt
                .block_number()
                .ok_or_else(|| "confirmed Tempo receipt omitted block".to_owned())?;
            Some(current.saturating_sub(block).saturating_add(1))
        } else {
            None
        };
        Ok(SettlementProviderResult {
            valid: true,
            settled: confirmations.is_some(),
            payer: Some(format!("{:#x}", validated.payer)),
            transaction: confirmations.map(|_| format!("{hash:#x}")),
            settlement_ref: None,
            effect_ref: confirmations.map(|_| "tempo:tip20-transfer:0".to_owned()),
            invalid_reason: None,
            asset: Some(self.asset.clone()),
            amount: Some(validated.amount),
            recipient: Some(format!("{:#x}", self.recipient)),
            confirmations,
            extensions,
        })
    }

    async fn validate_charge(&self, request: &ProviderRequest) -> Result<ValidatedCharge, String> {
        let (credential, charge) = request.parse_mpp()?;
        Self::validate_charge_scope(&charge, &self.asset, self.recipient)?;
        let validated = self
            .method
            .validate(&credential, &charge)
            .await
            .map_err(|_| "official Tempo credential validation failed".to_owned())?;
        let payload = credential
            .charge_payload()
            .map_err(|_| "invalid Tempo charge payload".to_owned())?;
        let signed = payload
            .signed_tx()
            .ok_or_else(|| "only signed Tempo transaction credentials are supported".to_owned())?;
        let bytes = signed
            .parse::<Bytes>()
            .map_err(|_| "invalid signed Tempo transaction bytes".to_owned())?;
        let payer = validated
            .source
            .as_deref()
            .and_then(|source| source.rsplit(':').next())
            .and_then(|source| source.parse::<Address>().ok())
            .ok_or_else(|| "validated Tempo charge omitted payer".to_owned())?;
        let amount = charge
            .amount
            .parse::<u128>()
            .map(Amount::new)
            .map_err(|_| "Tempo amount exceeds the supported range".to_owned())?;
        Ok(ValidatedCharge {
            credential,
            charge,
            transaction_hash: alloy_primitives::keccak256(&bytes),
            payer,
            amount,
        })
    }

    fn validate_charge_scope(
        charge: &mpp::ChargeRequest,
        asset: &AssetIdentity,
        recipient: Address,
    ) -> Result<(), String> {
        let expected_chain = asset
            .network
            .strip_prefix("eip155:")
            .and_then(|chain| chain.parse::<u64>().ok());
        let currency = charge
            .currency_address()
            .map_err(|_| "invalid Tempo currency".to_owned())?;
        let amount = charge.amount_u256().map_err(|_| "invalid Tempo amount".to_owned())?;
        if charge.chain_id() != expected_chain
            || asset
                .token_contract
                .as_deref()
                .and_then(|token| token.parse::<Address>().ok())
                != Some(currency)
            || charge.recipient_address().ok() != Some(recipient)
            || amount.is_zero()
            || charge.fee_payer()
            || charge.machine_token_enabled()
            || charge
                .splits()
                .map_err(|_| "invalid Tempo split parameters".to_owned())?
                .is_some_and(|splits| !splits.is_empty())
        {
            return Err("Tempo charge is outside the configured direct-charge scope".to_owned());
        }
        Ok(())
    }

    fn rejected(reason: String) -> SettlementProviderResult {
        SettlementProviderResult {
            valid: false,
            settled: false,
            payer: None,
            transaction: None,
            settlement_ref: None,
            effect_ref: None,
            invalid_reason: Some(reason),
            asset: None,
            amount: None,
            recipient: None,
            confirmations: None,
            extensions: BTreeMap::new(),
        }
    }
}

impl ProviderRequest {
    fn parse_mpp(&self) -> Result<(mpp::PaymentCredential, mpp::ChargeRequest), String> {
        if self.protocol != "mpp"
            || self.run_id.is_empty()
            || self.run_id.len() > 128
            || !self
                .run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
        {
            return Err("provider request identity is invalid".to_owned());
        }
        let challenge = self
            .payment_requirements
            .get("www_authenticate")
            .and_then(Value::as_str)
            .ok_or_else(|| "MPP challenge is required".to_owned())?;
        let challenge = mpp::parse_www_authenticate(challenge).map_err(|_| "invalid MPP challenge".to_owned())?;
        let authorization = self
            .payment_payload
            .get("authorization")
            .and_then(Value::as_str)
            .ok_or_else(|| "MPP authorization is required".to_owned())?;
        let credential = mpp::parse_authorization(authorization).map_err(|_| "invalid MPP authorization".to_owned())?;
        if serde_json::to_value(&credential.challenge).map_err(|_| "invalid MPP challenge echo".to_owned())?
            != serde_json::to_value(challenge.to_echo()).map_err(|_| "invalid MPP challenge".to_owned())?
        {
            return Err("MPP challenge echo mismatch".to_owned());
        }
        let charge = challenge
            .request
            .decode::<mpp::ChargeRequest>()
            .map_err(|_| "invalid MPP charge request".to_owned())?;
        Ok((credential, charge))
    }
}

#[cfg(test)]
#[path = "../tests/provider_test.rs"]
mod tests;
