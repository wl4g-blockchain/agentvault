use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use solana_signature::Signature as SolanaSignature;
use std::str::FromStr;

use crate::model::{Amount, AssetIdentity, PaymentDirection};

// The official x402 EVM client backdates EIP-3009 authorizations by ten
// minutes. Conversely, consensus and RPC propagation can expose a trusted
// block timestamp slightly ahead of the observing application wall clock.
// Settlement validation and budget aggregation must share these bounds so a
// valid ledger fact cannot temporarily make its own wallet unusable.
pub(crate) const MAX_SETTLEMENT_EFFECTIVE_TIME_PAST_SKEW_SECONDS: i64 = 10 * 60;
pub(crate) const MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS: i64 = 60;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LedgerEntryType {
    Payment,
    Fee,
    Refund,
    Deposit,
    Transfer,
    Reversal,
}

impl LedgerEntryType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Payment => "payment",
            Self::Fee => "fee",
            Self::Refund => "refund",
            Self::Deposit => "deposit",
            Self::Transfer => "transfer",
            Self::Reversal => "reversal",
        }
    }
}

crate::storage::repository_entity! { "pay_ledger", AppendOnly;
    keys: ["id"], immutable: ["created_at"], revision: None;
    /// Immutable, single-sided wallet activity backed by confirmed external
    /// evidence. Corrections are appended as `Reversal` entries.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentLedgerEntry {
        pub id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub intent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub scenario_id: Option<String>,
        pub wallet_id: String,
        pub entry_type: LedgerEntryType,
        pub direction: PaymentDirection,
        #[sqlx(flatten)]
        pub asset: AssetIdentity,
        pub amount: Amount,
        pub posting_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tx_hash: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub settlement_ref: Option<String>,
        /// Provider- or chain-verified locator for one effect inside shared
        /// settlement evidence, for example `log:7`, `gas`, or `effect:abc`.
        /// `None` deliberately treats a transaction/reference as one fact.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub effect_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub transfer_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reversal_of: Option<String>,
        pub effective_at: i64,
        pub created_at: i64,
    }
}

impl PaymentLedgerEntry {
    /// Returns the canonical transaction identity for one supported chain.
    /// EVM hashes are case-insensitive and normalized to lowercase; Solana
    /// signatures are base58 identities and therefore retain their spelling.
    pub(crate) fn canonical_transaction(network: &str, transaction: &str) -> Result<String, &'static str> {
        let transaction = transaction.trim();
        if network.starts_with("eip155:") {
            if transaction.len() != 66
                || !transaction.starts_with("0x")
                || !transaction[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("tx_hash");
            }
            return Ok(transaction.to_ascii_lowercase());
        }
        if network.starts_with("solana:") {
            let signature = SolanaSignature::from_str(transaction).map_err(|_| "tx_hash")?;
            if signature == SolanaSignature::default() {
                return Err("tx_hash");
            }
            return Ok(signature.to_string());
        }
        Err("network")
    }

    /// Validates entry-type-specific fields before append-only persistence.
    ///
    /// # Errors
    ///
    /// Returns the invalid field or invariant name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.wallet_id.trim().is_empty() {
            return Err("wallet_id");
        }
        self.asset.validate()?;
        if self.amount.is_zero() {
            return Err("amount");
        }
        if self.posting_key.trim().is_empty() {
            return Err("posting_key");
        }
        if self.tx_hash.is_none() && self.settlement_ref.is_none() {
            return Err("financial_evidence");
        }
        if let Some(effect_ref) = &self.effect_ref {
            Self::validate_effect_ref(effect_ref)?;
        }
        match self.entry_type {
            LedgerEntryType::Transfer if self.transfer_ref.is_none() => Err("transfer_ref"),
            LedgerEntryType::Reversal if self.reversal_of.is_none() => Err("reversal_of"),
            LedgerEntryType::Reversal => Ok(()),
            _ if self.reversal_of.is_some() => Err("reversal_of"),
            _ => Ok(()),
        }
    }

    #[must_use]
    pub fn same_posting_as(&self, other: &Self) -> bool {
        self.wallet_id == other.wallet_id && self.posting_key == other.posting_key
    }

    /// Returns whether two rows describe exactly the same immutable financial
    /// fact after colliding on `(wallet_id, posting_key)`.
    #[must_use]
    pub fn same_fact_content_as(&self, other: &Self) -> bool {
        self.same_posting_as(other)
            && self.intent_id == other.intent_id
            && self.scenario_id == other.scenario_id
            && self.entry_type == other.entry_type
            && self.direction == other.direction
            && self.asset == other.asset
            && self.amount == other.amount
            && self.tx_hash == other.tx_hash
            && self.settlement_ref == other.settlement_ref
            && self.effect_ref == other.effect_ref
            && self.transfer_ref == other.transfer_ref
            && self.reversal_of == other.reversal_of
            && self.effective_at == other.effective_at
    }

    /// Verifies that this row is the one full, opposite-direction correction
    /// of `original`. Storage must perform this check while locking the
    /// original row and must enforce that an original is reversed at most once.
    ///
    /// # Errors
    ///
    /// Returns `reversal_binding` when either row is structurally invalid or
    /// the correction changes ownership, accounting scope, asset, amount, or
    /// transfer identity.
    pub fn validate_reversal_of(&self, original: &Self) -> Result<(), &'static str> {
        self.validate().map_err(|_| "reversal_binding")?;
        original.validate().map_err(|_| "reversal_binding")?;
        if self.entry_type != LedgerEntryType::Reversal
            || original.entry_type == LedgerEntryType::Reversal
            || self.reversal_of.as_deref() != Some(original.id.as_str())
            || self.wallet_id != original.wallet_id
            || self.intent_id != original.intent_id
            || self.scenario_id != original.scenario_id
            || self.asset != original.asset
            || self.amount != original.amount
            || self.direction == original.direction
            || self.effect_ref != original.effect_ref
            || self.transfer_ref != original.transfer_ref
        {
            return Err("reversal_binding");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/model/domain/ledger_test.rs"]
mod tests;

impl PaymentLedgerEntry {
    /// Builds the stable identity of one external financial fact. Business
    /// interpretation such as amount, asset, or direction is deliberately absent:
    /// if those fields disagree for the same fact, storage must report a conflict.
    /// A transaction without a trusted effect locator is conservatively treated
    /// as one fact. A locator distinguishes independently confirmed effects inside
    /// the same transaction, such as an ERC-20 log and the native gas charge.
    ///
    /// # Errors
    ///
    /// Returns the invalid evidence field when neither a usable provider reference
    /// nor a valid transaction identity is available.
    pub fn settlement_posting_key(
        authority: &str,
        network: &str,
        tx_hash: Option<&str>,
        settlement_ref: Option<&str>,
        effect_ref: Option<&str>,
    ) -> Result<String, &'static str> {
        let network = network.trim();
        let effect_ref = effect_ref.map(Self::validate_effect_ref).transpose()?;
        if let Some(tx_hash) = tx_hash.map(str::trim).filter(|value| !value.is_empty()) {
            if network.is_empty() {
                return Err("network");
            }
            let normalized = PaymentLedgerEntry::canonical_transaction(network, tx_hash)?;
            return Ok(match effect_ref {
                Some(effect_ref) => format!(
                    "chain-effect:{}",
                    Self::posting_digest(
                        b"agentvault.chain-effect.v1",
                        &[network.as_bytes(), normalized.as_bytes(), effect_ref.as_bytes()]
                    )
                ),
                None => format!(
                    "chain:{}",
                    Self::posting_digest(
                        b"agentvault.chain-fact.v1",
                        &[network.as_bytes(), normalized.as_bytes()]
                    )
                ),
            });
        }

        if let Some(reference) = settlement_ref.map(str::trim).filter(|value| !value.is_empty()) {
            let authority = authority.trim();
            if authority.is_empty() {
                return Err("authority");
            }
            return Ok(match effect_ref {
                Some(effect_ref) => format!(
                    "provider-effect:{}",
                    Self::posting_digest(
                        b"agentvault.provider-effect.v1",
                        &[authority.as_bytes(), reference.as_bytes(), effect_ref.as_bytes()]
                    )
                ),
                None => format!(
                    "provider:{}",
                    Self::posting_digest(
                        b"agentvault.provider-fact.v1",
                        &[authority.as_bytes(), reference.as_bytes()]
                    )
                ),
            });
        }

        Err("transaction_evidence")
    }

    fn validate_effect_ref(effect_ref: &str) -> Result<&str, &'static str> {
        let normalized = effect_ref.trim();
        if normalized.is_empty() || normalized.len() > 256 || normalized.chars().any(char::is_control) {
            return Err("effect_ref");
        }
        Ok(normalized)
    }

    fn posting_digest(domain: &[u8], values: &[&[u8]]) -> String {
        let mut digest = Sha256::new();
        digest.update(domain);
        for value in values {
            digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            digest.update(value);
        }
        hex::encode(digest.finalize())
    }
}

impl PaymentLedgerEntry {
    pub(crate) fn validate_trusted_effective_time(
        created_at: i64,
        effective_at: i64,
        observed_at: i64,
    ) -> crate::Result<()> {
        let earliest = created_at.saturating_sub(MAX_SETTLEMENT_EFFECTIVE_TIME_PAST_SKEW_SECONDS);
        let latest = observed_at.saturating_add(MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS);
        if effective_at < 0 || effective_at < earliest || effective_at > latest {
            return Err(crate::Error::Settlement(
                "trusted settlement effective time exceeds bounded clock skew".to_owned(),
            ));
        }
        Ok(())
    }
}
