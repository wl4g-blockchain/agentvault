use super::{
    reconciliation_posting::ReconciliationPosting, ExternalSettlementProvider, MerchantEvidenceRecorder,
    SettlementCoordinator, SettlementFacts,
};
use crate::{
    model::ConfirmedSettlement, model::OutboundEvidenceObservation, model::PaymentDirection, model::PaymentIntent,
    model::PaymentStatus, model::ReconciliationEvidence, model::ReconciliationProviderObservation,
    model::SettlementOperation, model::SettlementProviderRequest, model::SettlementProviderResult, Error, Result,
};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait ReconciliationEvidenceSource: Send + Sync {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence>;
}

/// Builds a read-only provider verification request from durable state.
/// Returning `None` explicitly declares that this provider cannot safely
/// recover this intent through a query API.
pub trait ProviderReconciliationRequest: Send + Sync {
    fn authority(&self) -> &str;

    fn minimum_confirmations(&self) -> u64 {
        1
    }

    /// Reconstructs the provider-specific verification request exclusively
    /// from durable data.
    ///
    /// # Errors
    ///
    /// Returns an error when durable protocol data is incomplete or corrupt.
    fn build(&self, intent: &PaymentIntent) -> Result<Option<SettlementProviderRequest>>;
}

/// Evidence adapter for providers whose verification endpoint can report a
/// previously confirmed settlement. It deliberately invokes `Verify` only;
/// reconciliation never repeats the state-changing `Settle` operation.
pub struct VerifyOnlyProviderEvidence {
    provider: Arc<dyn ExternalSettlementProvider>,
    requests: Arc<dyn ProviderReconciliationRequest>,
}

impl VerifyOnlyProviderEvidence {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ExternalSettlementProvider>,
        requests: Arc<dyn ProviderReconciliationRequest>,
    ) -> Self {
        Self { provider, requests }
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for VerifyOnlyProviderEvidence {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        let Some(request) = self.requests.build(intent)? else {
            return Ok(ReconciliationEvidence::Unavailable {
                reason: "provider does not expose a safe reconciliation query".to_owned(),
            });
        };
        let result = self.provider.execute(SettlementOperation::Verify, &request).await?;
        PaymentEvidenceRouter::provider_result_to_evidence(
            intent,
            observed_at,
            self.requests.authority(),
            self.requests.minimum_confirmations(),
            result,
        )
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for SettlementCoordinator {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        match self.reconciliation_observation(intent, observed_at).await? {
            ReconciliationProviderObservation::Observed {
                authority,
                minimum_confirmations,
                result,
                effective_at,
            } => PaymentEvidenceRouter::provider_result_to_evidence(
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

/// Routes reconciliation by durable payment direction. An outbound protocol
/// never reuses the inbound provider settlement path: its merchant receipt is
/// only a transaction hint and must be independently proven by the fixed
/// operator RPC adapter configured for the intent's network.
pub struct PaymentEvidenceRouter {
    provider: Arc<dyn ReconciliationEvidenceSource>,
    outbound: Arc<MerchantEvidenceRecorder>,
}

impl PaymentEvidenceRouter {
    #[must_use]
    pub fn new(provider: Arc<dyn ReconciliationEvidenceSource>, outbound: Arc<MerchantEvidenceRecorder>) -> Self {
        Self { provider, outbound }
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for PaymentEvidenceRouter {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        if intent.direction != PaymentDirection::Outbound {
            return self.provider.lookup(intent, observed_at).await;
        }
        match self.outbound.reconciliation_observation(intent).await? {
            OutboundEvidenceObservation::Confirmed {
                authority,
                transaction,
                payer,
                effect_ref,
                confirmations,
                minimum_confirmations,
                effective_at,
            } => {
                let posting_key = crate::model::PaymentLedgerEntry::settlement_posting_key(
                    &authority,
                    &intent.asset.network,
                    Some(&transaction),
                    None,
                    Some(&effect_ref),
                )
                .map_err(|field| Error::Settlement(format!("invalid RPC settlement evidence: {field}")))?;
                Ok(ReconciliationEvidence::Confirmed(Box::new(ConfirmedSettlement {
                    authority,
                    asset: intent.asset.clone(),
                    recipient: intent.recipient.clone(),
                    payer: Some(payer),
                    settled_amount: intent.amount,
                    posting_key,
                    tx_hash: Some(transaction),
                    settlement_ref: None,
                    effect_ref: Some(effect_ref),
                    confirmations,
                    minimum_confirmations,
                    effective_at,
                })))
            }
            OutboundEvidenceObservation::Pending => Ok(ReconciliationEvidence::Pending),
            OutboundEvidenceObservation::Unknown { reason } => Ok(ReconciliationEvidence::Unknown { reason }),
            OutboundEvidenceObservation::Unavailable { reason } => Ok(ReconciliationEvidence::Unavailable { reason }),
        }
    }
}

impl PaymentEvidenceRouter {
    pub(crate) fn provider_result_to_evidence(
        intent: &PaymentIntent,
        observed_at: i64,
        authority: &str,
        minimum_confirmations: u64,
        result: SettlementProviderResult,
    ) -> Result<ReconciliationEvidence> {
        if !result.valid {
            return Ok(ReconciliationEvidence::Unknown {
                reason: result
                    .invalid_reason
                    .unwrap_or_else(|| "provider could not validate the durable payment evidence".to_owned()),
            });
        }
        if !result.settled {
            if intent.status == PaymentStatus::Submitted {
                return Ok(ReconciliationEvidence::Verified {
                    verification_ref: SettlementFacts::provider_verification_ref(&intent.id, &result)?,
                });
            }
            return Ok(ReconciliationEvidence::Pending);
        }
        result.validate_confirmed_for(intent, minimum_confirmations)?;
        let _observed_asset = result
            .asset
            .as_ref()
            .ok_or_else(|| Error::Settlement("provider result has no confirmed asset".to_owned()))?;
        let asset = intent.asset.clone();
        let settled_amount = result
            .amount
            .ok_or_else(|| Error::Settlement("provider result has no confirmed amount".to_owned()))?;
        let _observed_recipient = result
            .recipient
            .as_ref()
            .ok_or_else(|| Error::Settlement("provider result has no confirmed recipient".to_owned()))?;
        let recipient = intent.recipient.clone();
        let payer = result.payer;
        let confirmations = result.confirmations.unwrap_or_default();
        let tx_hash = ReconciliationPosting::nonempty(result.transaction);
        let settlement_ref = ReconciliationPosting::nonempty(result.settlement_ref);
        let effect_ref = ReconciliationPosting::nonempty(result.effect_ref);
        let posting_key = crate::model::PaymentLedgerEntry::settlement_posting_key(
            authority,
            &intent.asset.network,
            tx_hash.as_deref(),
            settlement_ref.as_deref(),
            effect_ref.as_deref(),
        )
        .map_err(|field| Error::Settlement(format!("invalid settlement posting evidence: {field}")))?;
        Ok(ReconciliationEvidence::Confirmed(Box::new(ConfirmedSettlement {
            authority: authority.to_owned(),
            asset,
            recipient,
            payer,
            settled_amount,
            posting_key,
            tx_hash,
            settlement_ref,
            effect_ref,
            confirmations,
            minimum_confirmations,
            effective_at: observed_at,
        })))
    }
}

#[cfg(test)]
#[path = "../../tests/payment/recovery_test.rs"]
mod tests;
