use super::*;
use crate::payment::validation::PaymentValidation;

impl SettlementCoordinator {
    /// Reconstructs settlement material exclusively from encrypted durable
    /// events. An x402 settlement that was durably requested is recovered from
    /// the canonical EIP-3009 `AuthorizationUsed` event plus the exact ERC-20
    /// transfer receipt; other states use the provider's read-only verify API.
    pub(crate) async fn reconciliation_observation(
        &self,
        intent: &PaymentIntent,
        observed_at: i64,
    ) -> Result<ReconciliationProviderObservation> {
        let settlement = self.load_reconciliation_settlement(intent).await?;
        if SettlementFacts::uses_eip3009_recovery(intent) {
            let Some(requested) = self.settlement_requested_event(intent).await? else {
                return Err(Error::Storage(
                    "inbound x402 reconciliation has no durable settlement operation".to_owned(),
                ));
            };
            return self
                .authorization_reconciliation_observation(intent, &settlement, &requested, observed_at)
                .await;
        }
        let request = SettlementProviderRequest {
            protocol: settlement.protocol,
            run_id: intent.run_id.clone(),
            payment_payload: settlement.payment_payload,
            payment_requirements: settlement.payment_requirements,
        };
        let binding = self.providers.get(&intent.protocol)?;
        let result = binding.provider.execute(SettlementOperation::Verify, &request).await?;
        let minimum_confirmations = binding.confirmation_depth(&intent.asset.network)?;
        Ok(ReconciliationProviderObservation::Observed {
            authority: binding.authority,
            minimum_confirmations,
            result: Box::new(result),
            effective_at: None,
        })
    }

    pub(super) async fn authorization_reconciliation_observation(
        &self,
        intent: &PaymentIntent,
        settlement: &crate::protocol::ProtocolSettlement,
        requested: &PaymentEvent,
        observed_at: i64,
    ) -> Result<ReconciliationProviderObservation> {
        let from_block = SettlementFacts::settlement_request_from_block(intent, requested)?
            .ok_or_else(|| Error::Storage("x402 settlement checkpoint is missing".to_owned()))?;
        let (query, valid_before) = SettlementFacts::authorization_query(intent, settlement, from_block)?;
        let expiry = SettlementFacts::authorization_expiry_deadline(intent, valid_before);
        match self.authorizations.lookup_authorization(&query).await? {
            EvmAuthorizationObservation::Confirmed(evidence) => Ok(ReconciliationProviderObservation::Observed {
                authority: evidence.authority,
                minimum_confirmations: evidence.minimum_confirmations,
                result: Box::new(SettlementProviderResult {
                    valid: true,
                    settled: true,
                    payer: Some(query.payer),
                    transaction: Some(evidence.transaction),
                    settlement_ref: None,
                    effect_ref: Some(evidence.effect_ref),
                    invalid_reason: None,
                    asset: Some(intent.asset.clone()),
                    amount: Some(intent.amount),
                    recipient: Some(intent.recipient.clone()),
                    confirmations: Some(evidence.confirmations),
                    extensions: BTreeMap::new(),
                }),
                effective_at: Some(evidence.effective_at),
            }),
            EvmAuthorizationObservation::NoEffect {
                observed_at: chain_observed_at,
                ..
            } if SettlementFacts::authorization_is_definitively_expired(observed_at, chain_observed_at, expiry) => {
                Ok(ReconciliationProviderObservation::Expired {
                    reason: "EIP-3009 authorization expired without an on-chain AuthorizationUsed effect".to_owned(),
                })
            }
            EvmAuthorizationObservation::Pending | EvmAuthorizationObservation::NoEffect { .. } => {
                Ok(ReconciliationProviderObservation::Observed {
                    authority: format!("evm-rpc:{}", intent.asset.network),
                    minimum_confirmations: 0,
                    result: Box::new(SettlementProviderResult {
                        valid: true,
                        settled: false,
                        payer: Some(query.payer),
                        transaction: None,
                        settlement_ref: None,
                        effect_ref: None,
                        invalid_reason: None,
                        asset: Some(intent.asset.clone()),
                        amount: Some(intent.amount),
                        recipient: Some(intent.recipient.clone()),
                        confirmations: None,
                        extensions: BTreeMap::new(),
                    }),
                    effective_at: None,
                })
            }
            EvmAuthorizationObservation::Mismatch { reason } => Ok(ReconciliationProviderObservation::Observed {
                authority: format!("evm-rpc:{}", intent.asset.network),
                minimum_confirmations: 0,
                result: Box::new(SettlementProviderResult {
                    valid: false,
                    settled: false,
                    payer: Some(query.payer),
                    transaction: None,
                    settlement_ref: None,
                    effect_ref: None,
                    invalid_reason: Some(reason),
                    asset: None,
                    amount: None,
                    recipient: None,
                    confirmations: None,
                    extensions: BTreeMap::new(),
                }),
                effective_at: None,
            }),
        }
    }

    pub(super) async fn load_reconciliation_settlement(
        &self,
        intent: &PaymentIntent,
    ) -> Result<crate::protocol::ProtocolSettlement> {
        if self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Submitted, 0)
            .await?
            .is_some()
        {
            return self.load_submission(intent).await.map(|(_, settlement)| settlement);
        }
        if intent.direction != PaymentDirection::Outbound {
            return Err(Error::Storage(
                "inbound reconciliation has no durable provider submission".to_owned(),
            ));
        }

        let challenge = self.load_challenge(intent).await?;
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Signed, 0)
            .await?
            .ok_or_else(|| Error::Storage("outbound reconciliation has no durable signed credential".to_owned()))?;
        let sealed = event
            .payload
            .get("sealed_credential")
            .cloned()
            .map(serde_json::from_value::<SealedSensitiveMaterial>)
            .transpose()?
            .ok_or_else(|| Error::Storage("signed event has no sealed credential".to_owned()))?;
        let plaintext = self
            .material_protector
            .open(&PaymentValidation::outbound_credential_context(&intent.id), &sealed)?;
        let credential: PaymentCredential = serde_json::from_slice(&plaintext)?;
        let adapter = self.protocols.get(&intent.protocol)?;
        let fingerprint = adapter.credential_fingerprint(&challenge, &credential)?;
        if event.signature_ref.as_deref() != Some(fingerprint.as_str()) || credential.credential_hash != fingerprint {
            return Err(Error::PaymentConflict(
                "durable outbound credential fingerprint is inconsistent".to_owned(),
            ));
        }
        adapter.settle_payment(&challenge, &credential).await
    }

    pub(super) async fn load_settlement_material(
        &self,
        intent: &PaymentIntent,
    ) -> Result<(String, crate::protocol::ProtocolSettlement)> {
        let event = self
            .events
            .find_latest_for_intent_type(&intent.id, PaymentEventType::CredentialVerified)
            .await?
            .ok_or_else(|| Error::Storage("credential verification event is missing".to_owned()))?;
        let verification_ref = event
            .payload
            .get("verification_ref")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Storage("verification reference is missing".to_owned()))?;
        let (_, settlement) = self.load_submission(intent).await?;
        Ok((verification_ref, settlement))
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/settlement/recovery_test.rs"]
mod tests;
