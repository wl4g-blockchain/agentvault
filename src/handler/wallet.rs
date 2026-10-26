//! Wallet domain service. External integrations belong to dedicated components.
use crate::{
    model::Wallet,
    storage::{IWalletRepository, Page, PageRequest, PaymentWallet},
    Result,
};
use std::sync::Arc;

pub struct WalletHandler {
    repository: Arc<dyn IWalletRepository>,
}
impl WalletHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IWalletRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<Wallet>> {
        self.repository.get(id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list(&self, page: PageRequest) -> Result<Page<Wallet>> {
        self.repository.list(page).await
    }
    /// # Errors
    /// Returns persistence errors.
    pub async fn get_payment_wallet(&self, id: &str) -> Result<Option<PaymentWallet>> {
        self.repository.get_payment_wallet(id).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/wallet_test.rs"]
mod tests;
