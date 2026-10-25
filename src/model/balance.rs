use super::Amount;

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
#[path = "../../tests/model/balance_test.rs"]
mod tests;
