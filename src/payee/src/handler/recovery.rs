use super::SettlementCoordinator;
use crate::{
    model::{PaymentIntent, ReconciliationEvidence, ReconciliationProviderObservation},
    Result,
};
pub use agentvault_common::model::settlement::ProviderReconciliationRequest;
pub use agentvault_common::storage::reconciliation::VerifyOnlyProviderEvidence;
use agentvault_common::{
    storage::reconciliation::ProviderEvidence, storage::reconciliation::ReconciliationEvidenceSource,
};
use async_trait::async_trait;
#[async_trait]
impl ReconciliationEvidenceSource for SettlementCoordinator {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        match self.reconciliation_observation(intent, observed_at).await? {
            ReconciliationProviderObservation::Observed {
                authority,
                minimum_confirmations,
                result,
                effective_at,
            } => ProviderEvidence::provider_result_to_evidence(
                intent,
                effective_at.unwrap_or(observed_at),
                &authority,
                minimum_confirmations,
                *result,
            ),
            ReconciliationProviderObservation::Expired { reason } => Ok(ReconciliationEvidence::Expired { reason }),
        }
    }
}
