use super::*;

impl SettlementCoordinator {
    /// Requests external settlement and commits the confirmed financial fact,
    /// intent transition, event, and notification atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for unauthorized access, invalid state/bindings,
    /// provider failures, missing evidence, or persistence failures.
    pub async fn settle_payment(
        &self,
        principal: &str,
        command: &SettleIncomingPayment,
        now: i64,
    ) -> Result<SettledIncomingPayment> {
        SettlementFacts::validate_settle_command(command)?;
        let _access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        let intent = self
            .payments
            .get_for_scenario(&command.scenario_id, &command.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        SettlementFacts::ensure_settlement_intent_binding(&intent, command)?;
        if intent.status == PaymentStatus::Settled {
            let receipt = self.load_receipt(&intent).await?;
            return Ok(SettlementFacts::settled_response(intent, receipt));
        }
        if intent.status != PaymentStatus::PendingSettlement {
            return Err(Error::InvalidTransition(format!(
                "cannot settle inbound payment in {}",
                intent.status.as_str()
            )));
        }
        let (stored_ref, settlement) = self.load_settlement_material(&intent).await?;
        if stored_ref != command.verification_ref {
            return Err(Error::PaymentConflict(
                "verification reference does not match the verified credential".to_owned(),
            ));
        }
        let intent = match self.record_settlement_requested(&intent, now).await? {
            SettlementRequestRecord::Inserted(intent) => intent,
            SettlementRequestRecord::Replay(intent) => {
                if intent.status == PaymentStatus::Settled {
                    let receipt = self.load_receipt(&intent).await?;
                    return Ok(SettlementFacts::settled_response(intent, receipt));
                }
                let recovered = self.recover_requested_settlement(&intent, now).await?;
                return Ok(recovered);
            }
        };
        let request = SettlementProviderRequest {
            protocol: settlement.protocol,
            run_id: intent.run_id.clone(),
            payment_payload: settlement.payment_payload,
            payment_requirements: settlement.payment_requirements,
        };
        let provider = self.providers.get(&intent.protocol)?;
        let minimum_confirmations = provider.confirmation_depth(&intent.asset.network)?;
        let result = provider.provider.execute(SettlementOperation::Settle, &request).await?;
        if !result.valid || !result.settled || (result.transaction.is_none() && result.settlement_ref.is_none()) {
            let status = if !result.valid && result.invalid_reason.is_some() {
                PaymentStatus::Failed
            } else {
                PaymentStatus::Unknown
            };
            let provider_evidence_hash = SettlementFacts::evidence_hash(&serde_json::to_value(&result)?)?;
            let updated = self
                .record_non_settlement(
                    &intent,
                    status,
                    if status == PaymentStatus::Failed {
                        PaymentEventType::Failed
                    } else {
                        PaymentEventType::Retried
                    },
                    json!({"stage": "settle", "provider_evidence_hash": provider_evidence_hash}),
                    now,
                )
                .await?;
            return Err(Error::Settlement(format!(
                "settlement is not confirmed; intent {} is {}",
                updated.id,
                updated.status.as_str()
            )));
        }
        if let Err(error) = result.validate_confirmed_for(&intent, minimum_confirmations) {
            let provider_evidence_hash = SettlementFacts::evidence_hash(&serde_json::to_value(&result)?)?;
            self.record_non_settlement(
                &intent,
                PaymentStatus::Unknown,
                PaymentEventType::Retried,
                json!({
                    "stage": "settle_evidence_validation",
                    "provider_evidence_hash": provider_evidence_hash,
                }),
                now,
            )
            .await?;
            return Err(error);
        }
        let settled = self
            .commit_confirmed(&intent, &provider.authority, &result, now, None)
            .await?;
        Ok(settled)
    }

    pub(super) async fn recover_requested_settlement(
        &self,
        intent: &PaymentIntent,
        now: i64,
    ) -> Result<SettledIncomingPayment> {
        match self.reconciliation_observation(intent, now).await? {
            ReconciliationProviderObservation::Expired { reason } => {
                let expired = self
                    .record_non_settlement(
                        intent,
                        PaymentStatus::Expired,
                        PaymentEventType::Failed,
                        json!({"stage": "settlement_recovery", "reason": reason}),
                        now,
                    )
                    .await?;
                Err(Error::Settlement(format!(
                    "settlement authorization expired; intent {} is {}",
                    expired.id,
                    expired.status.as_str()
                )))
            }
            ReconciliationProviderObservation::Observed {
                authority,
                minimum_confirmations,
                result,
                effective_at,
            } => {
                if result.valid && result.settled {
                    result.validate_confirmed_for(intent, minimum_confirmations)?;
                    return self
                        .commit_confirmed(intent, &authority, &result, now, effective_at)
                        .await;
                }
                if !result.valid {
                    let evidence = SettlementFacts::evidence_hash(&serde_json::to_value(&result)?)?;
                    self.record_non_settlement(
                        intent,
                        PaymentStatus::Unknown,
                        PaymentEventType::Retried,
                        json!({"stage": "settlement_recovery", "provider_evidence_hash": evidence}),
                        now,
                    )
                    .await?;
                }
                Err(Error::Settlement(
                    "durably requested settlement is awaiting trusted chain or provider evidence".to_owned(),
                ))
            }
        }
    }

    pub(super) async fn record_settlement_requested(
        &self,
        intent: &PaymentIntent,
        now: i64,
    ) -> Result<SettlementRequestRecord> {
        if let Some(event) = self.settlement_requested_event(intent).await? {
            SettlementFacts::settlement_request_from_block(intent, &event)?;
            let current = self
                .payments
                .get(&intent.id)
                .await?
                .ok_or_else(|| Error::Storage("settlement intent disappeared".to_owned()))?;
            return Ok(SettlementRequestRecord::Replay(current));
        }
        let from_block = if SettlementFacts::uses_eip3009_recovery(intent) {
            Some(
                self.authorizations
                    .latest_block(&intent.asset.network)
                    .await?
                    .saturating_sub(1),
            )
        } else {
            None
        };
        let mut requested = intent.clone();
        requested.updated_at = SettlementFacts::next_timestamp(intent.updated_at, now);
        requested.next_reconcile_at = Some(now.saturating_add(self.reconciliation_delay_seconds));
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::SettlementRequested,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: SettlementFacts::settlement_request_payload(intent, from_block),
            created_at: requested.updated_at,
        };
        match self
            .payments
            .commit_progress(PaymentProgressWrite {
                intent: &requested,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await
        {
            Ok(stored) => Ok(SettlementRequestRecord::Inserted(stored)),
            Err(error) => {
                let Some(winner) = self.settlement_requested_event(intent).await? else {
                    return Err(error);
                };
                SettlementFacts::settlement_request_from_block(intent, &winner)?;
                let current = self
                    .payments
                    .get(&intent.id)
                    .await?
                    .ok_or_else(|| Error::Storage("settlement intent disappeared".to_owned()))?;
                Ok(SettlementRequestRecord::Replay(current))
            }
        }
    }

    pub(super) async fn settlement_requested_event(&self, intent: &PaymentIntent) -> Result<Option<PaymentEvent>> {
        self.events
            .find_for_intent_type(&intent.id, PaymentEventType::SettlementRequested, 0)
            .await
    }

    pub(super) async fn record_non_settlement(
        &self,
        intent: &PaymentIntent,
        status: PaymentStatus,
        event_type: PaymentEventType,
        payload: Value,
        now: i64,
    ) -> Result<PaymentIntent> {
        let mut updated = intent.clone();
        updated.status = status;
        updated.reservation_status =
            SettlementFacts::reservation_after_non_settlement(status, updated.reservation_status);
        updated.updated_at = SettlementFacts::next_timestamp(intent.updated_at, now);
        updated.next_reconcile_at = if status == PaymentStatus::Unknown || status == PaymentStatus::PendingSettlement {
            Some(now.saturating_add(self.reconciliation_delay_seconds))
        } else {
            None
        };
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload,
            created_at: updated.updated_at,
        };
        self.payments
            .commit_progress(PaymentProgressWrite {
                intent: &updated,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await
    }

    pub(super) async fn commit_confirmed(
        &self,
        intent: &PaymentIntent,
        authority: &str,
        result: &SettlementProviderResult,
        now: i64,
        effective_at: Option<i64>,
    ) -> Result<SettledIncomingPayment> {
        let confirmed_at = SettlementFacts::next_timestamp(intent.updated_at, now);
        let effective_at = effective_at.unwrap_or(confirmed_at);
        crate::model::PaymentLedgerEntry::validate_trusted_effective_time(intent.created_at, effective_at, now)?;
        let settled = SettlementFacts::confirmed_intent(intent, result, confirmed_at, effective_at);
        let receipt = SettlementFacts::confirmed_receipt(intent, result, effective_at)?;
        let ledger = SettlementFacts::confirmed_ledger(intent, authority, result, effective_at, confirmed_at)?;
        let event = SettlementFacts::confirmed_event(intent, result, &receipt, confirmed_at);
        let outbox = SettlementFacts::confirmed_outbox(intent, confirmed_at);
        let stored = match self
            .payments
            .commit_settlement(SettlementWrite {
                intent: &settled,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                ledger_entries: std::slice::from_ref(&ledger),
                outbox: std::slice::from_ref(&outbox),
            })
            .await
        {
            Ok(stored) => stored,
            Err(Error::PaymentConflict(reason)) => {
                let current = self
                    .payments
                    .get(&intent.id)
                    .await?
                    .ok_or_else(|| Error::Storage("concurrently settled payment intent disappeared".to_owned()))?;
                if !SettlementFacts::is_concurrent_settlement_winner(intent, &current) {
                    return Err(Error::PaymentConflict(reason));
                }
                let receipt = self.load_receipt(&current).await?;
                tracing::info!(
                    event_name = "agentvault.payee.settlement_race_recovered",
                    intent_id = %current.id,
                    run_id = %current.run_id,
                    "returned the durable receipt committed by a concurrent reconciliation worker"
                );
                return Ok(SettlementFacts::settled_response(current, receipt));
            }
            Err(error) => return Err(error),
        };
        Ok(SettlementFacts::settled_response(stored, receipt))
    }

    pub(super) async fn load_receipt(&self, intent: &PaymentIntent) -> Result<PaymentReceipt> {
        let settled = self
            .events
            .find_latest_for_intent_type(&intent.id, PaymentEventType::Settled)
            .await?;
        let reconciled = if settled.is_some() {
            None
        } else {
            self.events
                .find_latest_for_intent_type(&intent.id, PaymentEventType::Reconciled)
                .await?
        };
        SettlementFacts::durable_receipt(settled.as_ref(), reconciled.as_ref())
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/settlement/commit_test.rs"]
mod tests;
