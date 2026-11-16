use crate::model::{Amount, AssetIdentity};

/// Current trusted balances observed immediately before a payment signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletBalanceSnapshot {
    pub asset_available: Amount,
    pub native_available: Amount,
    pub minimum_native_required: Amount,
}

/// Summary of one bounded managed-wallet observation pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalletBalanceSyncRun {
    pub scanned: u32,
    pub observed: u32,
    pub skipped: u32,
    pub failed: u32,
}

#[cfg(test)]
#[path = "../../../tests/model/balance_test.rs"]
mod tests;

/// Last successful chain observation for one managed wallet and asset.
/// This record is operational inventory, never authorization input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletBalanceRecord {
    pub wallet_id: String,
    pub asset: AssetIdentity,
    pub asset_available: Amount,
    pub native_available: Amount,
    pub observed_at: i64,
    pub updated_at: i64,
}
impl WalletBalanceRecord {
    /// Validates the persisted observation identity and timestamps.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.wallet_id.trim().is_empty() {
            return Err("wallet_id");
        }
        self.asset.validate()?;
        if self.observed_at < 0 || self.updated_at < self.observed_at {
            return Err("observed_at");
        }
        Ok(())
    }
}
