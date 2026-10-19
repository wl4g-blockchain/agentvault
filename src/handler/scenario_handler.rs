use std::sync::Arc;

use crate::{
    model::{PaymentClient, PaymentScenario},
    storage::{ClientStorage, ScenarioStorage},
    Error, Result,
};

/// Internal marker used only by transports explicitly configured without an
/// `AuthGuard` identity boundary. Registration and edge extraction both reject
/// this reserved value, so it cannot collide with an external principal.
pub(crate) const UNRESTRICTED_PRINCIPAL: &str = "agentvault:internal:unrestricted";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessContext {
    pub principal: String,
    pub client: PaymentClient,
    pub scenario: PaymentScenario,
}

/// Enforces principal -> client -> scenario ownership for every payment use
/// case. A request body never supplies `client_id`.
pub struct ScenarioHandler {
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
}

impl ScenarioHandler {
    #[must_use]
    pub fn new(clients: Arc<dyn ClientStorage>, scenarios: Arc<dyn ScenarioStorage>) -> Self {
        Self { clients, scenarios }
    }

    /// Resolves a principal through its client and enabled scenario ownership.
    ///
    /// # Errors
    ///
    /// Returns an error when authentication or storage lookup fails, or when
    /// the scenario is disabled, unknown, or owned by a different client.
    pub async fn authorize(&self, principal: &str, scenario_id: &str) -> Result<AccessContext> {
        if principal == UNRESTRICTED_PRINCIPAL {
            let scenario = self
                .scenarios
                .get(scenario_id)
                .await?
                .filter(|scenario| scenario.enabled)
                .ok_or(Error::Forbidden)?;
            let client = self
                .clients
                .get(&scenario.client_id)
                .await?
                .filter(|client| client.enabled)
                .ok_or(Error::Forbidden)?;
            return Ok(AccessContext {
                principal: client.auth_principal.clone(),
                client,
                scenario,
            });
        }
        let client = self.authenticated_client(principal).await?;
        let scenario = self
            .scenarios
            .get_for_client(&client.id, scenario_id)
            .await?
            .filter(|scenario| scenario.enabled)
            .ok_or(Error::Forbidden)?;
        Ok(AccessContext {
            principal: principal.to_owned(),
            client,
            scenario,
        })
    }

    async fn authenticated_client(&self, principal: &str) -> Result<PaymentClient> {
        if principal.trim().is_empty() {
            return Err(Error::Unauthenticated);
        }
        self.clients
            .find_by_principal(principal)
            .await?
            .filter(|client| client.authenticates(principal))
            .ok_or(Error::Forbidden)
    }
}

#[cfg(test)]
#[path = "../../tests/handler/scenario_handler_test.rs"]
mod tests;
