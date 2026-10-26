use crate::utils::evm_client::FixedEvmRpcClient;
use crate::{
    model::{
        Amount, ConfirmedEvmTransfer, ConfirmedTransfer, EvmAuthorizationObservation, EvmAuthorizationQuery,
        EvmReceiptObservation, EvmTransferQuery, ReceiptObservation, TransferQuery,
    },
    Error, Result,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use sha3::Keccak256;
use solana_client::{
    nonblocking::rpc_client::RpcClient,
    rpc_config::{CommitmentConfig, RpcTransactionConfig},
    rpc_request::RpcRequest,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature as SolanaSignature;
use solana_transaction_status_client_types::{
    option_serializer::OptionSerializer, EncodedConfirmedTransactionWithStatusMeta, TransactionConfirmationStatus,
    UiTransactionEncoding, UiTransactionTokenBalance,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use x402_chain_solana::chain::SolanaChainReference;
use x402_types::chain::ChainId;

const ERC20_TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const MAX_AUTHORIZATION_LOG_RANGE: u64 = 2_000;
const MAX_AUTHORIZATION_RECOVERY_BLOCKS: u64 = 65_536;

mod evm;
mod solana;
pub use evm::{EvmRpcReceiptConfig, EvmRpcReceiptSource};
pub use solana::{SolanaRpcReceiptConfig, SolanaRpcReceiptSource};
#[async_trait]
pub trait ReceiptSource: Send + Sync {
    async fn lookup(&self, query: &TransferQuery) -> Result<ReceiptObservation>;
}

pub use ReceiptSource as EvmReceiptSource;

#[async_trait]
pub trait EvmAuthorizationSource: Send + Sync {
    /// Returns the current canonical block after validating the configured
    /// RPC's chain identity. Callers persist a conservative predecessor before
    /// invoking a state-changing facilitator operation.
    async fn latest_block(&self, network: &str) -> Result<u64>;

    async fn lookup_authorization(&self, query: &EvmAuthorizationQuery) -> Result<EvmAuthorizationObservation>;
}

pub trait EvmEvidenceSource: ReceiptSource + EvmAuthorizationSource {}

impl<T> EvmEvidenceSource for T where T: ReceiptSource + EvmAuthorizationSource {}

/// Immutable network-to-RPC routing. Transaction hints can select only a
/// configured canonical network identifier, never an endpoint.
pub struct ReceiptRegistry {
    sources: BTreeMap<String, Arc<dyn ReceiptSource>>,
    authorizations: BTreeMap<String, Arc<dyn EvmAuthorizationSource>>,
}

pub type EvmReceiptRegistry = ReceiptRegistry;

impl ReceiptRegistry {
    /// Builds an immutable network-to-source registry.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed or duplicate EIP-155 network binding.
    pub fn new(sources: impl IntoIterator<Item = (String, Arc<dyn EvmEvidenceSource>)>) -> Result<Self> {
        let mut receipts = Vec::new();
        let mut authorizations = Vec::new();
        for (network, source) in sources {
            let receipt: Arc<dyn ReceiptSource> = source.clone();
            let authorization: Arc<dyn EvmAuthorizationSource> = source;
            receipts.push((network.clone(), receipt));
            authorizations.push((network, authorization));
        }
        Self::from_sources(receipts, authorizations)
    }

    /// Builds the shared receipt router while keeping EVM authorization-event
    /// recovery as an optional chain-specific capability.
    ///
    /// # Errors
    ///
    /// Rejects malformed or duplicate receipt networks and malformed or
    /// duplicate EVM authorization-source networks.
    pub fn from_sources(
        sources: impl IntoIterator<Item = (String, Arc<dyn ReceiptSource>)>,
        authorizations: impl IntoIterator<Item = (String, Arc<dyn EvmAuthorizationSource>)>,
    ) -> Result<Self> {
        let mut registered = BTreeMap::new();
        for (network, source) in sources {
            ReceiptRegistry::validate_receipt_network(&network).map_err(Error::Config)?;
            if registered.insert(network.clone(), source).is_some() {
                return Err(Error::Config(format!("duplicate receipt source for {network}")));
            }
        }
        let mut registered_authorizations = BTreeMap::new();
        for (network, source) in authorizations {
            EvmRpcReceiptSource::validate_network(&network).map_err(Error::Config)?;
            if registered_authorizations.insert(network.clone(), source).is_some() {
                return Err(Error::Config(format!(
                    "duplicate EVM authorization source for {network}"
                )));
            }
        }
        Ok(Self {
            sources: registered,
            authorizations: registered_authorizations,
        })
    }
}

#[async_trait]
impl ReceiptSource for ReceiptRegistry {
    async fn lookup(&self, query: &TransferQuery) -> Result<ReceiptObservation> {
        self.sources
            .get(&query.network)
            .ok_or_else(|| Error::Settlement(format!("no receipt source configured for {}", query.network)))?
            .lookup(query)
            .await
    }
}

#[async_trait]
impl EvmAuthorizationSource for ReceiptRegistry {
    async fn latest_block(&self, network: &str) -> Result<u64> {
        self.authorizations
            .get(network)
            .ok_or_else(|| Error::Settlement(format!("no receipt source configured for {network}")))?
            .latest_block(network)
            .await
    }

    async fn lookup_authorization(&self, query: &EvmAuthorizationQuery) -> Result<EvmAuthorizationObservation> {
        self.authorizations
            .get(&query.network)
            .ok_or_else(|| Error::Settlement(format!("no receipt source configured for {}", query.network)))?
            .lookup_authorization(query)
            .await
    }
}

impl ReceiptRegistry {
    pub(crate) fn validate_receipt_network(network: &str) -> std::result::Result<(), String> {
        if EvmRpcReceiptSource::validate_network(network).is_ok() {
            return Ok(());
        }
        network
            .parse::<ChainId>()
            .ok()
            .and_then(|chain| SolanaChainReference::try_from(chain).ok())
            .map(|_| ())
            .ok_or_else(|| "receipt network must be a canonical EIP-155 or Solana CAIP-2 ID".to_owned())
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/receipt/mod_test.rs"]
mod tests;
