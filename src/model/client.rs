use serde::{Deserialize, Serialize};

/// A registered business application and its trusted authentication binding.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentClient {
    pub id: String,
    pub name: String,
    pub auth_principal: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PaymentClient {
    #[must_use]
    pub fn authenticates(&self, principal: &str) -> bool {
        self.enabled && !principal.is_empty() && self.auth_principal == principal
    }

    /// Checks fields required for an unambiguous principal binding.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.name.trim().is_empty() {
            return Err("name");
        }
        if self.auth_principal.trim().is_empty()
            || self.auth_principal == crate::handler::UNRESTRICTED_PRINCIPAL
            || self.auth_principal.chars().any(char::is_control)
        {
            return Err("auth_principal");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/model/client_test.rs"]
mod tests;
