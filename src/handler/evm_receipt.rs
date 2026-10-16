//! Trusted EVM receipt evidence for outbound ERC-20 payments.
//!
//! A merchant `PAYMENT-RESPONSE` contributes only a transaction locator. This
//! adapter independently proves the canonical chain, successful receipt,
//! exact ERC-20 `Transfer` effect, confirmation depth, and block timestamp
//! through an operator-owned read-only RPC endpoint.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use sha3::{Digest as _, Keccak256};

use crate::{model::Amount, Error, Result};

use super::evm_rpc::FixedEvmRpcClient;

const ERC20_TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const MAX_AUTHORIZATION_LOG_RANGE: u64 = 2_000;
const MAX_AUTHORIZATION_RECOVERY_BLOCKS: u64 = 65_536;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmTransferQuery {
    pub network: String,
    pub transaction: String,
    pub token_contract: String,
    pub payer: String,
    pub recipient: String,
    pub amount: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmAuthorizationQuery {
    pub network: String,
    pub token_contract: String,
    pub payer: String,
    pub nonce: String,
    pub recipient: String,
    pub amount: Amount,
    /// Durable, operator-RPC-derived lower bound captured before `/settle`.
    pub from_block: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedEvmTransfer {
    pub authority: String,
    pub transaction: String,
    pub effect_ref: String,
    pub confirmations: u64,
    pub minimum_confirmations: u64,
    pub effective_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvmReceiptObservation {
    Confirmed(ConfirmedEvmTransfer),
    Pending,
    Mismatch { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvmAuthorizationObservation {
    Confirmed(ConfirmedEvmTransfer),
    /// An exact authorization event exists, but its transfer is not yet final.
    Pending,
    /// The complete bounded interval through this canonical block contains no
    /// matching authorization. `observed_at` is that block's timestamp.
    NoEffect {
        observed_block: u64,
        observed_at: i64,
    },
    Mismatch {
        reason: String,
    },
}

#[async_trait]
pub trait EvmReceiptSource: Send + Sync {
    async fn lookup(&self, query: &EvmTransferQuery) -> Result<EvmReceiptObservation>;
}

#[async_trait]
pub trait EvmAuthorizationSource: Send + Sync {
    /// Returns the current canonical block after validating the configured
    /// RPC's chain identity. Callers persist a conservative predecessor before
    /// invoking a state-changing facilitator operation.
    async fn latest_block(&self, network: &str) -> Result<u64>;

    async fn lookup_authorization(&self, query: &EvmAuthorizationQuery) -> Result<EvmAuthorizationObservation>;
}

pub trait EvmEvidenceSource: EvmReceiptSource + EvmAuthorizationSource {}

impl<T> EvmEvidenceSource for T where T: EvmReceiptSource + EvmAuthorizationSource {}

/// Immutable network-to-RPC routing. Transaction hints can select only a
/// configured canonical network identifier, never an endpoint.
pub struct EvmReceiptRegistry {
    sources: BTreeMap<String, Arc<dyn EvmEvidenceSource>>,
}

impl EvmReceiptRegistry {
    /// Builds an immutable network-to-source registry.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed or duplicate EIP-155 network binding.
    pub fn new(sources: impl IntoIterator<Item = (String, Arc<dyn EvmEvidenceSource>)>) -> Result<Self> {
        let mut registered = BTreeMap::new();
        for (network, source) in sources {
            validate_network(&network).map_err(Error::Config)?;
            if registered.insert(network.clone(), source).is_some() {
                return Err(Error::Config(format!("duplicate EVM receipt source for {network}")));
            }
        }
        Ok(Self { sources: registered })
    }
}

#[async_trait]
impl EvmReceiptSource for EvmReceiptRegistry {
    async fn lookup(&self, query: &EvmTransferQuery) -> Result<EvmReceiptObservation> {
        self.sources
            .get(&query.network)
            .ok_or_else(|| Error::Settlement(format!("no receipt source configured for {}", query.network)))?
            .lookup(query)
            .await
    }
}

#[async_trait]
impl EvmAuthorizationSource for EvmReceiptRegistry {
    async fn latest_block(&self, network: &str) -> Result<u64> {
        self.sources
            .get(network)
            .ok_or_else(|| Error::Settlement(format!("no receipt source configured for {network}")))?
            .latest_block(network)
            .await
    }

    async fn lookup_authorization(&self, query: &EvmAuthorizationQuery) -> Result<EvmAuthorizationObservation> {
        self.sources
            .get(&query.network)
            .ok_or_else(|| Error::Settlement(format!("no receipt source configured for {}", query.network)))?
            .lookup_authorization(query)
            .await
    }
}

/// Receipt verifier for one fixed EIP-155 network.
pub struct EvmRpcReceiptSource {
    network: String,
    chain_id: u128,
    authority: String,
    minimum_confirmations: u64,
    rpc: FixedEvmRpcClient,
}

pub struct EvmRpcReceiptConfig {
    pub endpoint: String,
    pub secret_headers: BTreeMap<String, String>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub minimum_confirmations: u64,
    pub allow_insecure_loopback: bool,
}

impl EvmRpcReceiptSource {
    /// Builds a verifier for one operator-controlled EVM RPC endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the network, confirmation limit, endpoint, secret
    /// headers, timeout, or response-size boundary is invalid.
    pub fn new(network: &str, config: EvmRpcReceiptConfig) -> Result<Self> {
        let chain_id = validate_network(network).map_err(Error::Config)?;
        if config.minimum_confirmations > 10_000 {
            return Err(Error::Config(
                "EVM receipt confirmation threshold is invalid".to_owned(),
            ));
        }
        Ok(Self {
            network: network.to_owned(),
            chain_id,
            authority: format!("evm-rpc:{network}"),
            minimum_confirmations: config.minimum_confirmations,
            rpc: FixedEvmRpcClient::new(
                &config.endpoint,
                config.secret_headers,
                config.timeout,
                config.max_response_bytes,
                config.allow_insecure_loopback,
            )?,
        })
    }

    async fn load_receipt(&self, transaction: &str) -> Result<Option<RpcReceipt>> {
        let value = self
            .rpc
            .request("eth_getTransactionReceipt", json!([transaction]))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| Error::Settlement("EVM RPC returned a malformed transaction receipt".to_owned()))
    }

    async fn canonical_block(&self, block_number: &str) -> Result<Option<RpcBlock>> {
        let value = self
            .rpc
            .request("eth_getBlockByNumber", json!([block_number, false]))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| Error::Settlement("EVM RPC returned a malformed block".to_owned()))
    }

    async fn validated_latest_block(&self) -> Result<u64> {
        let observed_chain = self.rpc.request("eth_chainId", json!([])).await?;
        let observed_chain = observed_chain
            .as_str()
            .ok_or_else(|| Error::Settlement("EVM RPC chain ID is missing".to_owned()))
            .and_then(parse_hex_u128)?;
        if observed_chain != self.chain_id {
            return Err(Error::Settlement(
                "RPC chain ID does not match the configured payment network".to_owned(),
            ));
        }
        let latest = self.rpc.request("eth_blockNumber", json!([])).await?;
        latest
            .as_str()
            .ok_or_else(|| Error::Settlement("EVM RPC latest block number is missing".to_owned()))
            .and_then(parse_hex_u64)
    }
}

#[async_trait]
impl EvmReceiptSource for EvmRpcReceiptSource {
    async fn lookup(&self, query: &EvmTransferQuery) -> Result<EvmReceiptObservation> {
        validate_query(query, &self.network)?;
        let observed_chain = self.rpc.request("eth_chainId", json!([])).await?;
        let observed_chain = observed_chain
            .as_str()
            .ok_or_else(|| Error::Settlement("EVM RPC chain ID is missing".to_owned()))
            .and_then(parse_hex_u128)?;
        if observed_chain != self.chain_id {
            return Ok(mismatch("RPC chain ID does not match the payment network"));
        }

        let Some(receipt) = self.load_receipt(&query.transaction).await? else {
            return Ok(EvmReceiptObservation::Pending);
        };
        if !receipt.transaction_hash.eq_ignore_ascii_case(&query.transaction) {
            return Ok(mismatch("receipt transaction hash does not match the merchant hint"));
        }
        if receipt.status.as_deref().and_then(parse_hex_u64_option) != Some(1) {
            return Ok(mismatch("payment transaction is not successful"));
        }
        let Some(block_number_wire) = receipt.block_number.as_deref() else {
            return Ok(EvmReceiptObservation::Pending);
        };
        let Some(block_hash) = receipt.block_hash.as_deref() else {
            return Ok(EvmReceiptObservation::Pending);
        };
        let block_number = parse_hex_u64(block_number_wire)?;
        let transfer = match exact_transfer(&receipt, query)? {
            TransferMatch::Found(transfer) => transfer,
            TransferMatch::Removed => return Ok(EvmReceiptObservation::Pending),
            TransferMatch::Missing => return Ok(mismatch("receipt has no exact ERC-20 Transfer effect")),
            TransferMatch::Ambiguous => return Ok(mismatch("receipt contains ambiguous exact Transfer effects")),
        };

        let latest = self.rpc.request("eth_blockNumber", json!([])).await?;
        let latest = latest
            .as_str()
            .ok_or_else(|| Error::Settlement("EVM RPC latest block number is missing".to_owned()))
            .and_then(parse_hex_u64)?;
        if latest < block_number {
            return Ok(EvmReceiptObservation::Pending);
        }
        let confirmations = latest.saturating_sub(block_number).saturating_add(1);
        let Some(block) = self.canonical_block(block_number_wire).await? else {
            return Ok(EvmReceiptObservation::Pending);
        };
        if !block.hash.eq_ignore_ascii_case(block_hash)
            || parse_hex_u64(&block.number)? != block_number
            || !transfer.block_hash.eq_ignore_ascii_case(block_hash)
            || parse_hex_u64(&transfer.block_number)? != block_number
        {
            return Ok(EvmReceiptObservation::Pending);
        }
        if confirmations < self.minimum_confirmations {
            return Ok(EvmReceiptObservation::Pending);
        }
        let timestamp = parse_hex_u64(&block.timestamp)?;
        let effective_at = i64::try_from(timestamp)
            .map_err(|_| Error::Settlement("EVM block timestamp exceeds the supported range".to_owned()))?;
        let log_index = parse_hex_u64(&transfer.log_index)?;
        Ok(EvmReceiptObservation::Confirmed(ConfirmedEvmTransfer {
            authority: self.authority.clone(),
            transaction: query.transaction.clone(),
            effect_ref: format!("log:{log_index}"),
            confirmations,
            minimum_confirmations: self.minimum_confirmations,
            effective_at,
        }))
    }
}

#[async_trait]
impl EvmAuthorizationSource for EvmRpcReceiptSource {
    async fn latest_block(&self, network: &str) -> Result<u64> {
        if network != self.network {
            return Err(Error::Settlement(
                "EVM authorization checkpoint network is invalid".to_owned(),
            ));
        }
        self.validated_latest_block().await
    }

    async fn lookup_authorization(&self, query: &EvmAuthorizationQuery) -> Result<EvmAuthorizationObservation> {
        validate_authorization_query(query, &self.network)?;
        let latest = self.validated_latest_block().await?;
        let Some(confirmed_through) = confirmed_through_block(latest, self.minimum_confirmations) else {
            return Ok(EvmAuthorizationObservation::Pending);
        };
        if confirmed_through < query.from_block {
            return Ok(EvmAuthorizationObservation::Pending);
        }

        let event_topic = authorization_used_topic();
        let payer_topic = address_topic(&query.payer)?;
        let mut logs = Vec::new();
        for (from_block, to_block) in bounded_log_ranges(query.from_block, confirmed_through)? {
            let value = self
                .rpc
                .request(
                    "eth_getLogs",
                    json!([{
                        "fromBlock": hex_quantity(from_block),
                        "toBlock": hex_quantity(to_block),
                        "address": &query.token_contract,
                        "topics": [&event_topic, &payer_topic, &query.nonce]
                    }]),
                )
                .await?;
            let mut batch: Vec<RpcLog> = serde_json::from_value(value)
                .map_err(|_| Error::Settlement("EVM RPC returned malformed AuthorizationUsed logs".to_owned()))?;
            logs.append(&mut batch);
        }
        let candidates = authorization_transactions(&logs, query)?;
        let transaction = match candidates.as_slice() {
            [] => {
                let latest_wire = hex_quantity(confirmed_through);
                let block = self
                    .canonical_block(&latest_wire)
                    .await?
                    .ok_or_else(|| Error::Settlement("EVM RPC confirmed canonical block is unavailable".to_owned()))?;
                if parse_hex_u64(&block.number)? != confirmed_through {
                    return Err(Error::Settlement(
                        "EVM RPC confirmed canonical block does not match the requested height".to_owned(),
                    ));
                }
                let observed_at = i64::try_from(parse_hex_u64(&block.timestamp)?)
                    .map_err(|_| Error::Settlement("EVM block timestamp exceeds the supported range".to_owned()))?;
                return Ok(EvmAuthorizationObservation::NoEffect {
                    observed_block: confirmed_through,
                    observed_at,
                });
            }
            [transaction] => transaction.clone(),
            _ => {
                return Ok(EvmAuthorizationObservation::Mismatch {
                    reason: "authorization nonce has ambiguous on-chain events".to_owned(),
                })
            }
        };
        let transfer = EvmTransferQuery {
            network: query.network.clone(),
            transaction: transaction.clone(),
            token_contract: query.token_contract.clone(),
            payer: query.payer.clone(),
            recipient: query.recipient.clone(),
            amount: query.amount,
        };
        let observation = self.lookup(&transfer).await?;
        let evidence = match observation {
            EvmReceiptObservation::Confirmed(evidence) => evidence,
            EvmReceiptObservation::Pending => return Ok(EvmAuthorizationObservation::Pending),
            EvmReceiptObservation::Mismatch { reason } => return Ok(EvmAuthorizationObservation::Mismatch { reason }),
        };
        let receipt = self
            .load_receipt(&transaction)
            .await?
            .ok_or_else(|| Error::Settlement("confirmed authorization receipt disappeared".to_owned()))?;
        if !receipt_contains_authorization(&receipt, query)? {
            return Ok(EvmAuthorizationObservation::Mismatch {
                reason: "settlement receipt does not contain the expected AuthorizationUsed event".to_owned(),
            });
        }
        Ok(EvmAuthorizationObservation::Confirmed(evidence))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcReceipt {
    transaction_hash: String,
    block_hash: Option<String>,
    block_number: Option<String>,
    status: Option<String>,
    #[serde(default)]
    logs: Vec<RpcLog>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcLog {
    address: String,
    #[serde(default)]
    topics: Vec<String>,
    data: String,
    log_index: Option<String>,
    transaction_hash: Option<String>,
    block_hash: Option<String>,
    block_number: Option<String>,
    #[serde(default)]
    removed: bool,
}

#[derive(Deserialize)]
struct RpcBlock {
    hash: String,
    number: String,
    timestamp: String,
}

struct MatchedTransfer {
    log_index: String,
    block_hash: String,
    block_number: String,
}

enum TransferMatch {
    Found(MatchedTransfer),
    Removed,
    Missing,
    Ambiguous,
}

fn exact_transfer(receipt: &RpcReceipt, query: &EvmTransferQuery) -> Result<TransferMatch> {
    let mut matched = Vec::new();
    let mut removed_match = false;
    for log in &receipt.logs {
        if !log.address.eq_ignore_ascii_case(&query.token_contract)
            || log
                .topics
                .first()
                .is_none_or(|topic| !topic.eq_ignore_ascii_case(ERC20_TRANSFER_TOPIC))
        {
            continue;
        }
        if log.topics.len() != 3 {
            return Err(Error::Settlement(
                "token receipt contains a malformed Transfer log".to_owned(),
            ));
        }
        let payer = topic_address(&log.topics[1])?;
        let recipient = topic_address(&log.topics[2])?;
        let amount = data_amount(&log.data)?;
        if !payer.eq_ignore_ascii_case(&query.payer)
            || !recipient.eq_ignore_ascii_case(&query.recipient)
            || amount != query.amount
        {
            continue;
        }
        if log.removed {
            removed_match = true;
            continue;
        }
        let Some(log_index) = &log.log_index else {
            return Err(Error::Settlement("Transfer log index is missing".to_owned()));
        };
        let Some(transaction_hash) = &log.transaction_hash else {
            return Err(Error::Settlement("Transfer transaction hash is missing".to_owned()));
        };
        let Some(block_hash) = &log.block_hash else {
            return Err(Error::Settlement("Transfer block hash is missing".to_owned()));
        };
        let Some(block_number) = &log.block_number else {
            return Err(Error::Settlement("Transfer block number is missing".to_owned()));
        };
        if !transaction_hash.eq_ignore_ascii_case(&query.transaction) {
            return Err(Error::Settlement(
                "Transfer log belongs to another transaction".to_owned(),
            ));
        }
        parse_hex_u64(log_index)?;
        matched.push(MatchedTransfer {
            log_index: log_index.clone(),
            block_hash: block_hash.clone(),
            block_number: block_number.clone(),
        });
    }
    match matched.len() {
        0 if removed_match => Ok(TransferMatch::Removed),
        0 => Ok(TransferMatch::Missing),
        1 => Ok(TransferMatch::Found(matched.remove(0))),
        _ => Ok(TransferMatch::Ambiguous),
    }
}

fn authorization_transactions(logs: &[RpcLog], query: &EvmAuthorizationQuery) -> Result<Vec<String>> {
    let mut transactions = Vec::new();
    for log in logs {
        if !authorization_log_matches(log, query)? || log.removed {
            continue;
        }
        let transaction = log
            .transaction_hash
            .as_deref()
            .ok_or_else(|| Error::Settlement("AuthorizationUsed transaction hash is missing".to_owned()))?;
        if !valid_hash(transaction) {
            return Err(Error::Settlement(
                "AuthorizationUsed transaction hash is invalid".to_owned(),
            ));
        }
        if !transactions
            .iter()
            .any(|seen: &String| seen.eq_ignore_ascii_case(transaction))
        {
            transactions.push(transaction.to_ascii_lowercase());
        }
    }
    Ok(transactions)
}

fn receipt_contains_authorization(receipt: &RpcReceipt, query: &EvmAuthorizationQuery) -> Result<bool> {
    let mut matched = 0_u8;
    for log in &receipt.logs {
        if authorization_log_matches(log, query)? && !log.removed {
            if log
                .transaction_hash
                .as_deref()
                .is_none_or(|transaction| !transaction.eq_ignore_ascii_case(&receipt.transaction_hash))
            {
                return Err(Error::Settlement(
                    "AuthorizationUsed log belongs to another transaction".to_owned(),
                ));
            }
            matched = matched.saturating_add(1);
        }
    }
    Ok(matched == 1)
}

fn authorization_log_matches(log: &RpcLog, query: &EvmAuthorizationQuery) -> Result<bool> {
    if !log.address.eq_ignore_ascii_case(&query.token_contract)
        || log
            .topics
            .first()
            .is_none_or(|topic| !topic.eq_ignore_ascii_case(&authorization_used_topic()))
    {
        return Ok(false);
    }
    if log.topics.len() != 3 {
        return Err(Error::Settlement(
            "token receipt contains a malformed AuthorizationUsed log".to_owned(),
        ));
    }
    let payer = topic_address(&log.topics[1])?;
    if !valid_hash(&log.topics[2]) {
        return Err(Error::Settlement("AuthorizationUsed nonce topic is invalid".to_owned()));
    }
    Ok(payer.eq_ignore_ascii_case(&query.payer) && log.topics[2].eq_ignore_ascii_case(&query.nonce))
}

fn validate_query(query: &EvmTransferQuery, expected_network: &str) -> Result<()> {
    if query.network != expected_network
        || !valid_hash(&query.transaction)
        || !valid_address(&query.token_contract)
        || !valid_address(&query.payer)
        || !valid_address(&query.recipient)
        || query.amount.is_zero()
    {
        return Err(Error::Settlement("EVM transfer evidence query is invalid".to_owned()));
    }
    Ok(())
}

fn validate_authorization_query(query: &EvmAuthorizationQuery, expected_network: &str) -> Result<()> {
    if query.network != expected_network
        || !valid_address(&query.token_contract)
        || !valid_address(&query.payer)
        || !valid_hash(&query.nonce)
        || !valid_address(&query.recipient)
        || query.amount.is_zero()
    {
        return Err(Error::Settlement(
            "EVM authorization evidence query is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn bounded_log_ranges(from_block: u64, to_block: u64) -> Result<Vec<(u64, u64)>> {
    if to_block < from_block {
        return Ok(Vec::new());
    }
    let span = to_block
        .checked_sub(from_block)
        .and_then(|distance| distance.checked_add(1))
        .ok_or_else(|| Error::Settlement("EVM authorization recovery block range overflowed".to_owned()))?;
    if span > MAX_AUTHORIZATION_RECOVERY_BLOCKS {
        return Err(Error::Settlement(format!(
            "EVM authorization recovery exceeds the bounded {MAX_AUTHORIZATION_RECOVERY_BLOCKS}-block window"
        )));
    }
    let mut ranges = Vec::new();
    let mut cursor = from_block;
    loop {
        let end = cursor
            .saturating_add(MAX_AUTHORIZATION_LOG_RANGE.saturating_sub(1))
            .min(to_block);
        ranges.push((cursor, end));
        if end == to_block {
            break;
        }
        cursor = end.saturating_add(1);
    }
    Ok(ranges)
}

fn confirmed_through_block(latest: u64, minimum_confirmations: u64) -> Option<u64> {
    latest.checked_add(1)?.checked_sub(minimum_confirmations.max(1))
}

fn hex_quantity(value: u64) -> String {
    format!("0x{value:x}")
}

fn authorization_used_topic() -> String {
    format!(
        "0x{}",
        hex::encode(Keccak256::digest(b"AuthorizationUsed(address,bytes32)"))
    )
}

fn address_topic(address: &str) -> Result<String> {
    if !valid_address(address) {
        return Err(Error::Settlement("EVM address topic is invalid".to_owned()));
    }
    Ok(format!("0x{}{}", "0".repeat(24), &address[2..].to_ascii_lowercase()))
}

fn validate_network(network: &str) -> std::result::Result<u128, String> {
    let chain = network
        .strip_prefix("eip155:")
        .filter(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| "EVM receipt network is invalid".to_owned())?;
    chain
        .parse::<u128>()
        .map_err(|_| "EVM receipt chain ID is invalid".to_owned())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 66 && value.starts_with("0x") && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_address(value: &str) -> bool {
    value.len() == 42 && value.starts_with("0x") && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn topic_address(value: &str) -> Result<String> {
    if value.len() != 66
        || !value.starts_with("0x")
        || !value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        || value[2..26].bytes().any(|byte| byte != b'0')
    {
        return Err(Error::Settlement("ERC-20 Transfer address topic is invalid".to_owned()));
    }
    Ok(format!("0x{}", value[26..].to_ascii_lowercase()))
}

fn data_amount(value: &str) -> Result<Amount> {
    if value.len() != 66
        || !value.starts_with("0x")
        || !value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        || value[2..34].bytes().any(|byte| byte != b'0')
    {
        return Err(Error::Settlement("ERC-20 Transfer amount is invalid".to_owned()));
    }
    u128::from_str_radix(&value[34..], 16)
        .map(Amount::new)
        .map_err(|_| Error::Settlement("ERC-20 Transfer amount is invalid".to_owned()))
}

fn parse_hex_u128(value: &str) -> Result<u128> {
    parse_hex(value).and_then(|digits| {
        u128::from_str_radix(digits, 16).map_err(|_| Error::Settlement("EVM hex quantity is too large".to_owned()))
    })
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    parse_hex(value).and_then(|digits| {
        u64::from_str_radix(digits, 16).map_err(|_| Error::Settlement("EVM hex quantity is too large".to_owned()))
    })
}

fn parse_hex_u64_option(value: &str) -> Option<u64> {
    parse_hex_u64(value).ok()
}

fn parse_hex(value: &str) -> Result<&str> {
    value
        .strip_prefix("0x")
        .filter(|digits| {
            !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                && (digits == &"0" || !digits.starts_with('0'))
        })
        .ok_or_else(|| Error::Settlement("EVM hex quantity is invalid".to_owned()))
}

fn mismatch(reason: &str) -> EvmReceiptObservation {
    EvmReceiptObservation::Mismatch {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
#[path = "../../tests/handler/evm_receipt_test.rs"]
mod tests;
