use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    apm::AgentVaultMetrics,
    model::{wallet::Wallet, Amount, AssetIdentity},
    storage::{PageRequest, WalletBalanceRecord, WalletBalanceStorage, WalletStorage},
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

/// Summary of one bounded managed-wallet observation pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalletBalanceSyncRun {
    pub scanned: u32,
    pub observed: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// Job-facing contract. Scheduling code invokes this component but never
/// performs chain I/O or persistence itself.
#[async_trait]
pub trait WalletBalanceSync: Send + Sync {
    async fn sync(&self, observed_at: i64) -> Result<WalletBalanceSyncRun>;
}

/// Periodically observes every locally managed wallet against each configured
/// payer asset. These snapshots support management and alerts only; payment
/// signing continues to load a fresh balance through [`WalletBalanceSource`].
pub struct WalletBalanceSynchronizer {
    wallets: Arc<dyn WalletStorage>,
    balances: Arc<dyn WalletBalanceStorage>,
    sources: Arc<dyn WalletBalanceSource>,
    assets: Vec<AssetIdentity>,
    metrics: AgentVaultMetrics,
    batch_size: u32,
    next_offset: AtomicU64,
}

impl WalletBalanceSynchronizer {
    /// Builds a bounded synchronizer and rejects ambiguous assets.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty asset set, invalid asset identity, or
    /// batch size outside the bounded storage-page contract.
    pub fn new(
        wallets: Arc<dyn WalletStorage>,
        balances: Arc<dyn WalletBalanceStorage>,
        sources: Arc<dyn WalletBalanceSource>,
        assets: impl IntoIterator<Item = AssetIdentity>,
        metrics: AgentVaultMetrics,
        batch_size: u32,
    ) -> Result<Self> {
        if batch_size == 0 || batch_size > 10_000 {
            return Err(Error::Config(
                "wallet balance sync batch_size must be between 1 and 10000".to_owned(),
            ));
        }
        let mut canonical = Vec::new();
        for asset in assets {
            asset
                .validate()
                .map_err(|field| Error::Config(format!("wallet balance sync asset has invalid {field}")))?;
            if !canonical.contains(&asset) {
                canonical.push(asset);
            }
        }
        if canonical.is_empty() {
            return Err(Error::Config(
                "wallet balance sync requires at least one enabled payer asset".to_owned(),
            ));
        }
        Ok(Self {
            wallets,
            balances,
            sources,
            assets: canonical,
            metrics,
            batch_size,
            next_offset: AtomicU64::new(0),
        })
    }

    async fn page(&self, offset: u64) -> Result<crate::storage::Page<Wallet>> {
        self.wallets
            .list(PageRequest {
                offset,
                limit: self.batch_size,
            })
            .await
    }

    async fn observe(&self, wallet: &Wallet, asset: &AssetIdentity, observed_at: i64) -> Result<WalletBalanceRecord> {
        let snapshot = self.sources.load(wallet, asset).await?;
        self.balances
            .upsert_balance(&WalletBalanceRecord {
                wallet_id: wallet.id.clone(),
                asset: asset.clone(),
                asset_available: snapshot.asset_available,
                native_available: snapshot.native_available,
                observed_at,
                updated_at: observed_at,
            })
            .await
    }
}

#[async_trait]
impl WalletBalanceSync for WalletBalanceSynchronizer {
    async fn sync(&self, observed_at: i64) -> Result<WalletBalanceSyncRun> {
        if observed_at < 0 {
            return Err(Error::InvalidRequest(
                "wallet balance sync timestamp must not be negative".to_owned(),
            ));
        }
        let requested_offset = self.next_offset.load(Ordering::Relaxed);
        let mut page = self.page(requested_offset).await?;
        let offset = if page.items.is_empty() && requested_offset > 0 {
            page = self.page(0).await?;
            0
        } else {
            requested_offset
        };
        let next = offset.saturating_add(u64::try_from(page.items.len()).unwrap_or(u64::MAX));
        self.next_offset
            .store(if next >= page.total { 0 } else { next }, Ordering::Relaxed);

        let mut run = WalletBalanceSyncRun::default();
        for wallet in page.items {
            run.scanned = run.scanned.saturating_add(1);
            let payment_wallet = match self.wallets.get_payment_wallet(&wallet.id).await {
                Ok(Some(payment_wallet)) if payment_wallet.signing_ready() => payment_wallet,
                Ok(_) => {
                    run.skipped = run.skipped.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    run.failed = run.failed.saturating_add(1);
                    tracing::warn!(
                        event.name = "agentvault.wallet.balance_sync_failed",
                        wallet.id = %wallet.id,
                        %error,
                        "failed to load managed wallet metadata"
                    );
                    continue;
                }
            };
            let mut compatible = false;
            for asset in &self.assets {
                if !payment_wallet.signing_ready_for_payment_network(&asset.network) {
                    continue;
                }
                compatible = true;
                match self.observe(&payment_wallet.wallet, asset, observed_at).await {
                    Ok(balance) => {
                        run.observed = run.observed.saturating_add(1);
                        self.metrics.record_wallet_balance_sync(&asset.network, true);
                        tracing::info!(
                            event.name = "agentvault.wallet.balance_synced",
                            wallet.id = %balance.wallet_id,
                            payment.network = %balance.asset.network,
                            payment.asset = %balance.asset.asset,
                            wallet.asset_available = %balance.asset_available,
                            wallet.native_available = %balance.native_available,
                            wallet.balance_observed_at = balance.observed_at,
                            "persisted managed-wallet balance observation"
                        );
                    }
                    Err(error) => {
                        run.failed = run.failed.saturating_add(1);
                        self.metrics.record_wallet_balance_sync(&asset.network, false);
                        tracing::warn!(
                            event.name = "agentvault.wallet.balance_sync_failed",
                            wallet.id = %payment_wallet.wallet.id,
                            payment.network = %asset.network,
                            payment.asset = %asset.asset,
                            %error,
                            "managed-wallet balance observation failed"
                        );
                    }
                }
            }
            if !compatible {
                run.skipped = run.skipped.saturating_add(1);
            }
        }
        Ok(run)
    }
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
