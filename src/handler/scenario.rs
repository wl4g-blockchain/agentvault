use crate::{
    handler::PayClientHandler,
    model::{scenario::UNRESTRICTED_PRINCIPAL, AccessContext, PaymentPolicy, PaymentScenario},
    storage::{IClientRepository, IScenarioRepository, IdempotentWrite, Page, PageRequest, ScenarioConfiguration},
    Error, Result,
};
use std::sync::Arc;

/// Enforces principal -> client -> scenario ownership for every payment use
/// case. A request body never supplies `client_id`.
pub struct ScenarioHandler {
    clients: Arc<PayClientHandler>,
    scenarios: Arc<dyn IScenarioRepository>,
}

impl ScenarioHandler {
    #[must_use]
    pub fn new(clients: Arc<dyn IClientRepository>, scenarios: Arc<dyn IScenarioRepository>) -> Self {
        Self {
            clients: Arc::new(PayClientHandler::new(clients)),
            scenarios,
        }
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
        let client = self.clients.authenticate(principal).await?;
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

    /// # Errors
    /// Returns persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentScenario>> {
        self.scenarios.get(id).await
    }

    /// # Errors
    /// Returns persistence errors.
    pub async fn list_for_client(&self, client: &str, page: PageRequest) -> Result<Page<PaymentScenario>> {
        self.scenarios.list_for_client(client, page).await
    }

    /// # Errors
    /// Rejects a conflicting registration or invalid initial policy.
    pub async fn register_with_policy(
        &self,
        scenario: &PaymentScenario,
        policy: &PaymentPolicy,
    ) -> Result<IdempotentWrite<ScenarioConfiguration>> {
        self.scenarios.register_with_policy(scenario, policy).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/scenario_test.rs"]
mod tests;
