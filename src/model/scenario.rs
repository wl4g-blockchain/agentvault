use serde::{Deserialize, Serialize};

/// A payment-isolated policy and wallet boundary owned by one client.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentScenario {
    pub id: String,
    pub client_id: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PaymentScenario {
    #[must_use]
    pub fn belongs_to(&self, client_id: &str) -> bool {
        self.client_id == client_id
    }

    /// Checks the fields used for ownership and idempotent registration.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.client_id.trim().is_empty() {
            return Err("client_id");
        }
        if self.name.trim().is_empty() {
            return Err("name");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/model/scenario_test.rs"]
mod tests;
