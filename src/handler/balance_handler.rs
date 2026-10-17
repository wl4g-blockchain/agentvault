use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    model::{wallet::Wallet, Amount, AssetIdentity},
    Error, Result,
};

use super::evm_rpc::FixedEvmRpcClient;

const BALANCE_OF_SELECTOR: &str = "70a08231";

/// Current trusted balances observed immediately before a payment signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletBalanceSnapshot {
    pub asset_available: Amount,
    pub native_available: Amount,
    /// Operator-configured gas reserve for the network served by this source.
    /// It travels with the observation so a multi-network registry cannot
    /// accidentally apply one chain's threshold to another chain.
    pub minimum_native_required: Amount,
}

#[async_trait]
pub trait WalletBalanceSource: Send + Sync {
    async fn load(&self, wallet: &Wallet, asset: &AssetIdentity) -> Result<WalletBalanceSnapshot>;
}

/// Immutable network router for trusted balance sources. Payment challenges
/// select only a canonical network identifier; endpoint ownership remains in
/// operator configuration.
pub struct WalletBalanceRegistry {
    sources: BTreeMap<String, Arc<dyn WalletBalanceSource>>,
}

impl WalletBalanceRegistry {
    /// Creates a registry and rejects blank or duplicate network bindings.
    ///
    /// # Errors
    ///
    /// Returns an error when no source is configured or a network binding is
    /// malformed or repeated.
    pub fn new(sources: impl IntoIterator<Item = (String, Arc<dyn WalletBalanceSource>)>) -> Result<Self> {
        let mut registered = BTreeMap::new();
        for (network, source) in sources {
            if network.trim().is_empty() || registered.insert(network.clone(), source).is_some() {
                return Err(Error::Config(format!(
                    "invalid or duplicate balance-source network: {network}"
                )));
            }
        }
        if registered.is_empty() {
            return Err(Error::Config(
                "at least one trusted balance source is required".to_owned(),
            ));
        }
        Ok(Self { sources: registered })
    }
}

#[async_trait]
impl WalletBalanceSource for WalletBalanceRegistry {
    async fn load(&self, wallet: &Wallet, asset: &AssetIdentity) -> Result<WalletBalanceSnapshot> {
        self.sources
            .get(&asset.network)
            .ok_or_else(|| Error::Settlement(format!("no balance source configured for {}", asset.network)))?
            .load(wallet, asset)
            .await
    }
}

/// Read-only EVM JSON-RPC balance adapter with a fixed operator-owned endpoint.
pub struct EvmRpcBalanceSource {
    rpc: FixedEvmRpcClient,
    minimum_native_required: Amount,
}

impl EvmRpcBalanceSource {
    /// Creates a redirect-free, bounded RPC client.
    ///
    /// # Errors
    ///
    /// Rejects unsafe endpoints, invalid secret headers, and zero limits.
    pub fn new(
        endpoint: &str,
        secret_headers: BTreeMap<String, String>,
        timeout: Duration,
        max_response_bytes: usize,
        minimum_native_required: Amount,
        allow_insecure_loopback: bool,
    ) -> Result<Self> {
        Ok(Self {
            rpc: FixedEvmRpcClient::new(
                endpoint,
                secret_headers,
                timeout,
                max_response_bytes,
                allow_insecure_loopback,
            )?,
            minimum_native_required,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Amount> {
        let result = self.rpc.request(method, params).await?;
        parse_hex_amount(
            result
                .as_str()
                .ok_or_else(|| Error::Settlement("EVM RPC result is missing".to_owned()))?,
        )
    }
}

#[async_trait]
impl WalletBalanceSource for EvmRpcBalanceSource {
    async fn load(&self, wallet: &Wallet, asset: &AssetIdentity) -> Result<WalletBalanceSnapshot> {
        let network_chain = asset
            .network
            .strip_prefix("eip155:")
            .ok_or_else(|| Error::Settlement("balance source supports only EIP-155 assets".to_owned()))?;
        if network_chain.is_empty() || !network_chain.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::Settlement("payment network is invalid".to_owned()));
        }
        let address = crate::model::canonicalize_payment_recipient(&asset.network, &wallet.address)
            .map_err(|_| Error::Settlement("payment wallet address is invalid".to_owned()))?;
        let contract = asset
            .token_contract
            .as_deref()
            .ok_or_else(|| Error::Settlement("EVM token contract is missing".to_owned()))?;
        let contract = crate::model::canonicalize_payment_recipient(&asset.network, contract)
            .map_err(|_| Error::Settlement("EVM token contract is invalid".to_owned()))?;
        let data = format!("0x{BALANCE_OF_SELECTOR}{:0>64}", &address[2..]);
        let (observed_chain, native_available, asset_available) = tokio::try_join!(
            self.request("eth_chainId", json!([])),
            self.request("eth_getBalance", json!([address, "latest"])),
            self.request("eth_call", json!([{"to": contract, "data": data}, "latest"])),
        )?;
        let expected_chain = network_chain
            .parse::<u128>()
            .map_err(|_| Error::Settlement("payment network chain ID is invalid".to_owned()))?;
        if observed_chain.value() != expected_chain {
            return Err(Error::Settlement(
                "EVM RPC chain ID does not match the payment network".to_owned(),
            ));
        }
        Ok(WalletBalanceSnapshot {
            asset_available,
            native_available,
            minimum_native_required: self.minimum_native_required,
        })
    }
}

fn parse_hex_amount(value: &str) -> Result<Amount> {
    let encoded = value
        .strip_prefix("0x")
        .filter(|encoded| !encoded.is_empty() && encoded.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| Error::Settlement("EVM RPC amount is invalid".to_owned()))?;
    let significant = encoded.trim_start_matches('0');
    if significant.len() > 32 {
        return Ok(Amount::new(u128::MAX));
    }
    u128::from_str_radix(if significant.is_empty() { "0" } else { significant }, 16)
        .map(Amount::new)
        .map_err(|_| Error::Settlement("EVM RPC amount is invalid".to_owned()))
}

#[cfg(test)]
#[path = "../../tests/handler/balance_handler_test.rs"]
mod tests;
