use std::sync::Arc;

use crate::{model::PaymentClient, storage::ClientStorage, Error, Result};

/// Trusted-principal to registered-client boundary.
pub struct ClientHandler {
    storage: Arc<dyn ClientStorage>,
}

impl ClientHandler {
    #[must_use]
    pub fn new(storage: Arc<dyn ClientStorage>) -> Self {
        Self { storage }
    }

    /// Resolves an authenticated principal. Disabled clients fail exactly like
    /// unknown clients so lifecycle state is not disclosed.
    ///
    /// # Errors
    ///
    /// Returns an error when the principal is empty, is not bound to an enabled
    /// client, or the client lookup fails.
    pub async fn authenticate(&self, principal: &str) -> Result<PaymentClient> {
        if principal.trim().is_empty() {
            return Err(Error::Unauthenticated);
        }
        self.storage
            .find_by_principal(principal)
            .await?
            .filter(|client| client.authenticates(principal))
            .ok_or(Error::Forbidden)
    }

    /// Registers a validated business application.
    ///
    /// # Errors
    ///
    /// Returns an error when a required client field is invalid or persistence
    /// rejects the registration.
    pub async fn register(&self, client: &PaymentClient) -> Result<PaymentClient> {
        client
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment client field: {field}")))?;
        self.storage.insert(client).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/client_handler_test.rs"]
mod tests;
