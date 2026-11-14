use super::PaymentClient;

/// Resolved principal and durable ownership entities for one request.
/// Entity fields are reused directly rather than copied into another shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessContext {
    pub principal: String,
    pub client: PaymentClient,
    pub scenario: PaymentScenario,
}

#[cfg(test)]
#[path = "../../tests/model/scenario_test.rs"]
mod tests;

mod entity {
    use serde::{Deserialize, Serialize};

    crate::storage::repository_entity! { "pay_scenario", Catalog;
        keys: ["id"], immutable: ["created_at"], revision: Some("updated_at");
        /// A payment-isolated policy and wallet boundary owned by one client.
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
        #[serde(deny_unknown_fields)]
        pub struct PaymentScenario {
            pub id: String,
            pub client_id: String,
            pub name: String,
            pub enabled: bool,
            pub created_at: i64,
            pub updated_at: i64,
        }
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
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/model/scenario_entity_test.rs"
        ));
    }
}
pub use entity::*;
