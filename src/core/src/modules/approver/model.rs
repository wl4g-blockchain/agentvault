use serde::{Deserialize, Serialize};
crate::storage::repository_entity! { "pay_scenario_approver", Catalog;
    keys: ["scenario_id", "auth_principal"], immutable: ["created_at"], revision: Some("updated_at");
    /// Trusted administrator mapping persisted for one payment scenario.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentScenarioApprover {
        pub scenario_id: String,
        pub auth_principal: String,
        pub enabled: bool,
        pub created_at: i64,
        pub updated_at: i64,
    }
}

impl PaymentScenarioApprover {
    /// Validates the durable authorization identity.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.scenario_id.trim().is_empty() {
            return Err("scenario_id");
        }
        if self.auth_principal.trim().is_empty() {
            return Err("auth_principal");
        }
        if self.updated_at < self.created_at {
            return Err("updated_at");
        }
        Ok(())
    }
}
