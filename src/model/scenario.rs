use super::{PaymentClient, PaymentScenario};

/// Internal marker used only by transports explicitly configured without an
/// `AuthGuard` identity boundary. Registration rejects this reserved value.
pub(crate) const UNRESTRICTED_PRINCIPAL: &str = "agentvault:internal:unrestricted";

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
