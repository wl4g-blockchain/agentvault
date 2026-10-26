//! Official Tempo transaction builder adapted to the authorized Vault two-stage signer.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TempoSigningPayload {
    pub(super) kind: String,
    pub(super) payer: String,
    pub(super) transaction: TempoTransaction,
}

impl TempoSigningPayload {
    pub(super) fn prepare(integrity: &MppChallengeIntegrity, payer: &str, now: i64) -> Result<Self> {
        if now < 0 || now >= integrity.expires_at {
            return Err(Error::PaymentProtocol(
                "MPP Tempo challenge expired before signing".to_owned(),
            ));
        }
        let payer = canonicalize_wallet_address(EVM_EOA_ALGORITHM, payer)
            .map_err(|_| Error::PaymentProtocol("MPP Tempo payer is not a valid EVM address".to_owned()))?;
        let valid_before = now
            .saturating_add(TEMPO_CHARGE_VALIDITY_SECONDS)
            .min(integrity.expires_at);
        let valid_before = u64::try_from(valid_before)
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo validity window is invalid".to_owned()))?;
        let payload = Self {
            kind: TEMPO_SIGNING_PAYLOAD_KIND.to_owned(),
            payer,
            transaction: Self::build_transaction(integrity, valid_before)?,
        };
        payload.validate(integrity)?;
        Ok(payload)
    }

    pub(super) fn build_transaction(
        integrity: &MppChallengeIntegrity,
        valid_before: NonZeroU64,
    ) -> Result<TempoTransaction> {
        let charge = TempoCharge::from_challenge(&integrity.official).map_err(MppWire::error)?;
        if charge.fee_payer() || charge.splits().is_some() {
            return Err(Error::PaymentProtocol(
                "MPP Tempo fee sponsorship and split charges are unsupported".to_owned(),
            ));
        }
        let chain_id = integrity
            .asset
            .network
            .strip_prefix("eip155:")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo chain ID is invalid".to_owned()))?;
        let token_contract = integrity
            .asset
            .token_contract
            .as_deref()
            .and_then(|value| value.parse::<Address>().ok())
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo token contract is invalid".to_owned()))?;
        let recipient = integrity
            .recipient
            .parse::<Address>()
            .map_err(|_| Error::PaymentProtocol("MPP Tempo recipient is invalid".to_owned()))?;
        if charge.chain_id() != chain_id
            || charge.currency() != token_contract
            || charge.recipient() != recipient
            || charge.amount() != U256::from(integrity.amount.value())
        {
            return Err(Error::PaymentProtocol(
                "MPP Tempo charge projection does not match the canonical challenge".to_owned(),
            ));
        }
        let memo = charge
            .memo()
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo attribution memo is missing".to_owned()))?;
        let call = Call {
            to: TxKind::Call(token_contract),
            value: U256::ZERO,
            input: Bytes::from(
                ITIP20::transferWithMemoCall {
                    to: recipient,
                    amount: U256::from(integrity.amount.value()),
                    memo: memo.into(),
                }
                .abi_encode(),
            ),
        };
        Ok(build_tempo_tx(TempoTxOptions {
            calls: vec![call],
            chain_id,
            fee_token: token_contract,
            nonce: 0,
            nonce_key: U256::MAX,
            gas_limit: TEMPO_CHARGE_GAS_LIMIT,
            max_fee_per_gas: MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            fee_payer: false,
            valid_before: Some(valid_before.get()),
            key_authorization: None,
        }))
    }

    pub(super) fn validate(&self, integrity: &MppChallengeIntegrity) -> Result<B256> {
        if self.kind != TEMPO_SIGNING_PAYLOAD_KIND {
            return Err(Error::PaymentProtocol(
                "MPP Tempo signing payload kind is unsupported".to_owned(),
            ));
        }
        let canonical_payer = canonicalize_wallet_address(EVM_EOA_ALGORITHM, &self.payer)
            .map_err(|_| Error::PaymentProtocol("MPP Tempo payer binding is invalid".to_owned()))?;
        if canonical_payer != self.payer {
            return Err(Error::PaymentProtocol(
                "MPP Tempo payer binding is not canonical".to_owned(),
            ));
        }
        let valid_before = self
            .transaction
            .valid_before
            .ok_or_else(|| Error::PaymentProtocol("MPP Tempo transaction validity window is missing".to_owned()))?;
        if i64::try_from(valid_before.get()).unwrap_or(i64::MAX) > integrity.expires_at {
            return Err(Error::PaymentProtocol(
                "MPP Tempo transaction outlives its payment challenge".to_owned(),
            ));
        }
        let expected = Self::build_transaction(integrity, valid_before)?;
        if self.transaction != expected {
            return Err(Error::PaymentProtocol(
                "MPP Tempo signing transaction was modified".to_owned(),
            ));
        }
        Ok(self.transaction.signature_hash())
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/mpp_tempo_test.rs"]
mod tests;
