mod local;
mod mqtt;

use async_trait::async_trait;

use crate::{
    model::sign::{PaymentSignRequest, PaymentSignResponse},
    Result,
};

pub use local::LocalPaymentSigner;
pub use mqtt::MqttPaymentSigner;

/// Transport-neutral payment signing client used by agent runtimes.
#[async_trait]
pub trait IPaymentSigner: Send + Sync {
    /// Submits one bounded payment-sign request and waits for its response.
    ///
    /// # Errors
    ///
    /// Returns an error when transport setup, I/O, timeout, or response decoding fails.
    async fn execute(&self, request: PaymentSignRequest) -> Result<PaymentSignResponse>;
}
