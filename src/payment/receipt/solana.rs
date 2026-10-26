use super::*;
/// Receipt verifier for one fixed Solana CAIP-2 network.
pub struct SolanaRpcReceiptSource {
    network: String,
    chain_reference: SolanaChainReference,
    authority: String,
    minimum_confirmations: u64,
    rpc: Arc<RpcClient>,
}

pub struct SolanaRpcReceiptConfig {
    pub endpoint: String,
    pub secret_headers: BTreeMap<String, String>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub minimum_confirmations: u64,
    pub allow_insecure_loopback: bool,
}

impl SolanaRpcReceiptSource {
    /// Builds a verifier using the official Solana RPC client and fixed
    /// operator-owned chain binding.
    ///
    /// # Errors
    ///
    /// Rejects malformed chain IDs, unsafe transport settings, and invalid
    /// confirmation/response limits.
    pub fn new(network: &str, config: &SolanaRpcReceiptConfig) -> Result<Self> {
        let chain_id = network
            .parse::<ChainId>()
            .map_err(|_| Error::Config("Solana receipt network is invalid".to_owned()))?;
        let chain_reference = SolanaChainReference::try_from(chain_id)
            .map_err(|_| Error::Config("Solana receipt network is invalid".to_owned()))?;
        if config.minimum_confirmations == 0 || config.minimum_confirmations > 10_000 || config.max_response_bytes == 0
        {
            return Err(Error::Config(
                "Solana receipt confirmation or response limit is invalid".to_owned(),
            ));
        }
        Ok(Self {
            network: network.to_owned(),
            chain_reference,
            authority: format!("solana-rpc:{network}"),
            minimum_confirmations: config.minimum_confirmations,
            rpc: Arc::new(crate::utils::solana_rpc::SolanaRpcClient::build(
                &config.endpoint,
                config.timeout,
                config.max_response_bytes,
                &config.secret_headers,
                config.allow_insecure_loopback,
            )?),
        })
    }

    fn validate_genesis(&self, genesis: &str) -> Result<()> {
        if genesis.get(..32) != Some(self.chain_reference.as_str()) {
            return Err(Error::Settlement(format!(
                "Solana RPC genesis hash does not match {}",
                self.network
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl ReceiptSource for SolanaRpcReceiptSource {
    async fn lookup(&self, query: &TransferQuery) -> Result<ReceiptObservation> {
        SolanaRpcReceiptSource::validate_solana_query(query, &self.network)?;
        let signature = query
            .transaction
            .parse::<SolanaSignature>()
            .map_err(|_| Error::Settlement("Solana transaction signature is invalid".to_owned()))?;
        let (genesis, statuses, current_slot) = tokio::try_join!(
            self.rpc.get_genesis_hash(),
            self.rpc.get_signature_statuses(std::slice::from_ref(&signature)),
            self.rpc.get_slot(),
        )
        .map_err(|error| Error::Settlement(format!("Solana receipt RPC failed: {error}")))?;
        self.validate_genesis(&genesis.to_string())?;
        let Some(status) = statuses.value.into_iter().next().flatten() else {
            return Ok(ReceiptObservation::Pending);
        };
        if status.err.is_some() {
            return Ok(SolanaRpcReceiptSource::solana_mismatch(
                "Solana transaction execution failed",
            ));
        }
        if status.confirmation_status() == TransactionConfirmationStatus::Processed {
            return Ok(ReceiptObservation::Pending);
        }
        let confirmations = status.confirmations.map_or_else(
            || current_slot.saturating_sub(status.slot).saturating_add(1),
            |confirmations| u64::try_from(confirmations).unwrap_or(u64::MAX).saturating_add(1),
        );
        if confirmations < self.minimum_confirmations {
            return Ok(ReceiptObservation::Pending);
        }

        let confirmed: Option<EncodedConfirmedTransactionWithStatusMeta> = self
            .rpc
            .send(
                RpcRequest::GetTransaction,
                json!([
                    signature.to_string(),
                    RpcTransactionConfig {
                        encoding: Some(UiTransactionEncoding::Base64),
                        commitment: Some(CommitmentConfig::confirmed()),
                        max_supported_transaction_version: Some(0),
                    }
                ]),
            )
            .await
            .map_err(|error| Error::Settlement(format!("load Solana transaction: {error}")))?;
        let Some(confirmed) = confirmed else {
            return Ok(ReceiptObservation::Pending);
        };
        if let Some(reason) = Self::validate_confirmed_transfer(&confirmed, &signature, query)? {
            return Ok(Self::solana_mismatch(reason));
        }
        let effective_at = match confirmed.block_time {
            Some(block_time) => block_time,
            None => self
                .rpc
                .get_block_time(confirmed.slot)
                .await
                .map_err(|error| Error::Settlement(format!("load Solana block time: {error}")))?,
        };
        Ok(ReceiptObservation::Confirmed(ConfirmedTransfer {
            authority: self.authority.clone(),
            transaction: query.transaction.clone(),
            effect_ref: SolanaRpcReceiptSource::solana_effect_ref(query),
            confirmations,
            minimum_confirmations: self.minimum_confirmations,
            effective_at,
        }))
    }
}

impl SolanaRpcReceiptSource {
    fn validate_confirmed_transfer(
        confirmed: &EncodedConfirmedTransactionWithStatusMeta,
        signature: &SolanaSignature,
        query: &TransferQuery,
    ) -> Result<Option<&'static str>> {
        let Some(meta) = confirmed.transaction.meta.as_ref() else {
            return Ok(Some("Solana transaction metadata is missing"));
        };
        if meta.err.is_some() {
            return Ok(Some("Solana transaction metadata reports failure"));
        }
        let Some(transaction) = confirmed.transaction.transaction.decode() else {
            return Ok(Some("Solana transaction encoding is invalid"));
        };
        let message = transaction.message.serialize();
        let required = usize::from(transaction.message.header().num_required_signatures);
        let keys = transaction.message.static_account_keys();
        let payer = query
            .payer
            .parse::<Pubkey>()
            .map_err(|_| Error::Settlement("Solana receipt payer is invalid".to_owned()))?;
        let payer_index = keys
            .get(..required)
            .and_then(|signers| signers.iter().position(|candidate| *candidate == payer));
        let payer_signature_valid = payer_index
            .and_then(|index| transaction.signatures.get(index))
            .is_some_and(|payer_signature| payer_signature.verify(payer.as_ref(), &message));
        if transaction.signatures.first() != Some(signature) || !payer_signature_valid {
            return Ok(Some("Solana transaction does not carry the expected payer signature"));
        }

        let pre_payer = SolanaRpcReceiptSource::solana_token_balance(
            &meta.pre_token_balances,
            &query.payer,
            &query.token_contract,
        )?;
        let post_payer = SolanaRpcReceiptSource::solana_token_balance(
            &meta.post_token_balances,
            &query.payer,
            &query.token_contract,
        )?;
        let pre_recipient = SolanaRpcReceiptSource::solana_token_balance(
            &meta.pre_token_balances,
            &query.recipient,
            &query.token_contract,
        )?;
        let post_recipient = SolanaRpcReceiptSource::solana_token_balance(
            &meta.post_token_balances,
            &query.recipient,
            &query.token_contract,
        )?;
        let payer_debit = pre_payer.checked_sub(post_payer);
        let recipient_credit = post_recipient.checked_sub(pre_recipient);
        if payer_debit != Some(query.amount.value()) || recipient_credit != Some(query.amount.value()) {
            return Ok(Some("Solana token balance deltas do not match the payment"));
        }
        Ok(None)
    }

    pub(crate) fn validate_solana_query(query: &TransferQuery, expected_network: &str) -> Result<()> {
        let valid_network =
            query.network == expected_network && ReceiptRegistry::validate_receipt_network(&query.network).is_ok();
        let valid_addresses = query.token_contract.parse::<Pubkey>().is_ok()
            && query.payer.parse::<Pubkey>().is_ok()
            && query.recipient.parse::<Pubkey>().is_ok()
            && query.payer != query.recipient;
        if !valid_network
            || !valid_addresses
            || query.transaction.parse::<SolanaSignature>().is_err()
            || query.amount.is_zero()
        {
            return Err(Error::Settlement(
                "Solana transfer evidence query is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn solana_token_balance(
        balances: &OptionSerializer<Vec<UiTransactionTokenBalance>>,
        owner: &str,
        mint: &str,
    ) -> Result<u128> {
        let balances = match balances {
            OptionSerializer::Some(balances) => balances,
            OptionSerializer::None | OptionSerializer::Skip => {
                return Err(Error::Settlement(
                    "Solana receipt has no token balance evidence".to_owned(),
                ))
            }
        };
        let mut total = 0_u128;
        for balance in balances {
            let OptionSerializer::Some(balance_owner) = &balance.owner else {
                continue;
            };
            if balance_owner != owner || balance.mint != mint {
                continue;
            }
            let amount = balance
                .ui_token_amount
                .amount
                .parse::<u128>()
                .map_err(|_| Error::Settlement("Solana receipt token amount is invalid".to_owned()))?;
            total = total
                .checked_add(amount)
                .ok_or_else(|| Error::Settlement("Solana receipt token amount overflows".to_owned()))?;
        }
        Ok(total)
    }

    pub(crate) fn solana_effect_ref(query: &TransferQuery) -> String {
        let mut hash = Sha256::new();
        hash.update(b"agentvault.x402.solana.exact.effect.v1");
        let amount = query.amount.to_string();
        for value in [
            query.network.as_bytes(),
            query.token_contract.as_bytes(),
            query.payer.as_bytes(),
            query.recipient.as_bytes(),
            amount.as_bytes(),
            query.transaction.as_bytes(),
        ] {
            hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hash.update(value);
        }
        format!("x402-solana-exact:{}", hex::encode(hash.finalize()))
    }

    pub(crate) fn solana_mismatch(reason: &str) -> ReceiptObservation {
        ReceiptObservation::Mismatch {
            reason: reason.to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/receipt/solana_test.rs"]
mod tests;
