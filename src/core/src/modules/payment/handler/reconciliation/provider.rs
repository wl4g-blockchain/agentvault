use super::{
    Arc, ConfirmedSettlement, Error, ExternalSettlementProvider, PaymentDirection, PaymentIntent, PaymentStatus,
    ProviderReconciliationRequest, ReconciliationEvidence, ReconciliationEvidenceSource, ReconciliationPosting, Result,
    SettlementOperation, SettlementProviderResult,
};
use async_trait::async_trait;
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
        ProviderEvidence::provider_result_to_evidence(
            intent,
            observed_at,
            self.requests.authority(),
            self.requests.minimum_confirmations(),
            result,
        )
    }
}

pub struct ProviderEvidence;
impl ProviderEvidence {
    ///
    /// # Errors
    /// Returns a settlement error when the provider result lacks intent-bound confirmation evidence.
    pub fn provider_result_to_evidence(
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
                    verification_ref: crate::storage::reconciliation::SettlementEvidence::verification_ref(
                        &intent.id, &result,
                    )?,
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

/// Routes reconciliation by durable payment direction. An outbound protocol
/// never reuses the inbound provider settlement path: its merchant receipt is
/// only a transaction hint and must be independently proven by the fixed
/// operator RPC adapter configured for the intent's network.
pub struct PaymentEvidenceRouter {
    provider: Option<Arc<dyn ReconciliationEvidenceSource>>,
    outbound: Option<Arc<dyn ReconciliationEvidenceSource>>,
}

impl PaymentEvidenceRouter {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ReconciliationEvidenceSource>,
        outbound: Arc<dyn ReconciliationEvidenceSource>,
    ) -> Self {
        Self {
            provider: Some(provider),
            outbound: Some(outbound),
        }
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for PaymentEvidenceRouter {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        if intent.direction != PaymentDirection::Outbound {
            return match &self.provider {
                Some(provider) => provider.lookup(intent, observed_at).await,
                None => Ok(ReconciliationEvidence::Unavailable {
                    reason: "inbound recovery is owned by the payee service".into(),
                }),
            };
        }
        match &self.outbound {
            Some(outbound) => outbound.lookup(intent, observed_at).await,
            None => Ok(ReconciliationEvidence::Unavailable {
                reason: "outbound recovery is owned by the payer service".into(),
            }),
        }
    }
}

impl PaymentEvidenceRouter {
    #[must_use]
    pub fn with_roles(
        provider: Option<Arc<dyn ReconciliationEvidenceSource>>,
        outbound: Option<Arc<dyn ReconciliationEvidenceSource>>,
    ) -> Self {
        Self { provider, outbound }
    }
}
