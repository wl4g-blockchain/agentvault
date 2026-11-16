//! Shared wallet SQL transactions; concrete providers supply only driver types and locks.

macro_rules! wallet_repository {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        use crate::{
            model::wallet::Wallet, model::Amount, model::AssetIdentity, model::BudgetUnit,
            storage::codec::AmountEncoding as _, storage::codec::PaymentStorageCodec,
            storage::wallet::WalletStorageRules, storage::EncryptedWalletKeyVersion, storage::IWalletBalanceRepository,
            storage::IWalletRepository, storage::Page, storage::PageRequest, storage::PaymentWallet,
            storage::WalletBalanceRecord, storage::WalletCreationConfig, storage::WalletCustody,
            storage::WalletKeyStatus, Error, Result,
        };

        use async_trait::async_trait;

        use sqlx::Row as _;

        const PAYMENT_WALLET_SELECT: &str = "SELECT w.*, active.version AS active_key_version FROM pay_wallet w \
     LEFT JOIN wallet_key_version active ON active.wallet_id = w.id AND active.status = 'active'";
        crate::storage::wallet::r#macro::operations::wallet_operations!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
        crate::storage::wallet::r#macro::balances::wallet_balances!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
        crate::storage::wallet::r#macro::rows::wallet_rows!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
    };
}
pub(crate) use wallet_repository;

#[cfg(test)]
#[path = "../../../../tests/storage/wallet/transactions_test.rs"]
mod tests;

#[path = "macro/balances.rs"]
pub(crate) mod balances;
#[path = "macro/operations.rs"]
pub(crate) mod operations;
#[path = "macro/rows.rs"]
pub(crate) mod rows;
