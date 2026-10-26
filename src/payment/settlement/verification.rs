use super::*;

impl SettlementCoordinator {
    /// Structurally verifies and durably binds a credential before invoking
    /// the fixed external provider's verify endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for unauthorized access, replay conflicts, invalid
    /// credentials, provider failures, or atomic persistence failures.
    pub async fn verify_payment(
        &self,
        principal: &str,
        command: &VerifyIncomingPayment,
        now: i64,
    ) -> Result<IncomingVerification> {
        SettlementFacts::validate_verify_command(command)?;
        let _access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        let intent = self
            .payments
            .get_for_challenge(&command.scenario_id, &command.challenge_hash)
            .await?
            .ok_or_else(|| Error::NotFound("payment challenge".to_owned()))?;
        SettlementFacts::ensure_incoming_identity(&intent, &command.run_id, &command.request_hash)?;
        if intent.status == PaymentStatus::Settled || intent.status == PaymentStatus::PendingSettlement {
            self.ensure_credential_replay(&intent, &command.credential).await?;
            return self.load_verification(&intent).await;
        }
        if intent.status == PaymentStatus::Failed || intent.status == PaymentStatus::Expired {
            return Ok(IncomingVerification {
                intent,
                valid: false,
                verification_ref: None,
            });
        }
        let (submitted, request) = match intent.status {
            PaymentStatus::Created => self.submit_credential(&intent, &command.credential, now).await?,
            PaymentStatus::Submitted => self.resume_submitted(&intent, &command.credential, now).await?,
            _ => {
                return Err(Error::InvalidTransition(format!(
                    "cannot verify inbound payment in {}",
                    intent.status.as_str()
                )))
            }
        };
        let result = self
            .providers
            .get(&submitted.protocol)?
            .provider
            .execute(SettlementOperation::Verify, &request)
            .await?;
        if !result.valid {
            let provider_evidence_hash = SettlementFacts::evidence_hash(&serde_json::to_value(&result)?)?;
            let failed = self
                .record_non_settlement(
                    &submitted,
                    PaymentStatus::Failed,
                    PaymentEventType::Failed,
                    json!({"stage": "verify", "provider_evidence_hash": provider_evidence_hash}),
                    now,
                )
                .await?;
            return Ok(IncomingVerification {
                intent: failed,
                valid: false,
                verification_ref: None,
            });
        }
        let verification_ref = SettlementFacts::provider_verification_ref(&submitted.id, &result)?;
        let pending = self
            .record_non_settlement(
                &submitted,
                PaymentStatus::PendingSettlement,
                PaymentEventType::CredentialVerified,
                json!({"verification_ref": verification_ref}),
                now,
            )
            .await?;
        Ok(IncomingVerification {
            intent: pending,
            valid: true,
            verification_ref: Some(verification_ref),
        })
    }

    pub(super) async fn submit_credential(
        &self,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
        now: i64,
    ) -> Result<(PaymentIntent, SettlementProviderRequest)> {
        let challenge = self.load_challenge(intent).await?;
        let adapter = self.protocols.get(&intent.protocol)?;
        let mut credential = credential.clone();
        adapter.verify_credential(&challenge, &credential, now).await?;
        credential.credential_hash = adapter.credential_fingerprint(&challenge, &credential)?;
        let settlement = adapter.settle_payment(&challenge, &credential).await?;
        let settlement_bytes = Zeroizing::new(serde_json::to_vec(&settlement)?);
        let sealed_settlement = self
            .material_protector
            .seal(&SettlementFacts::settlement_material_context(intent), &settlement_bytes)?;
        let mut submitted = intent.clone();
        submitted.status = PaymentStatus::Submitted;
        submitted.next_reconcile_at = Some(now.saturating_add(self.reconciliation_delay_seconds));
        submitted.updated_at = SettlementFacts::next_timestamp(intent.updated_at, now);
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Submitted,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({
                "credential_fingerprint": credential.credential_hash.clone(),
                "sealed_settlement": sealed_settlement,
            }),
            created_at: submitted.updated_at,
        };
        let submitted = self
            .payments
            .verify_incoming(VerificationWrite {
                intent: &submitted,
                credential: &credential,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await?;
        let request = SettlementProviderRequest {
            protocol: settlement.protocol,
            run_id: submitted.run_id.clone(),
            payment_payload: settlement.payment_payload,
            payment_requirements: settlement.payment_requirements,
        };
        Ok((submitted, request))
    }

    pub(super) async fn resume_submitted(
        &self,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
        now: i64,
    ) -> Result<(PaymentIntent, SettlementProviderRequest)> {
        let challenge = self.load_challenge(intent).await?;
        let adapter = self.protocols.get(&intent.protocol)?;
        adapter.verify_credential(&challenge, credential, now).await?;
        let fingerprint = adapter.credential_fingerprint(&challenge, credential)?;
        let (stored_fingerprint, settlement) = self.load_submission(intent).await?;
        if fingerprint != stored_fingerprint {
            return Err(Error::PaymentConflict(
                "credential retry does not match the submitted payment proof".to_owned(),
            ));
        }
        Ok((
            intent.clone(),
            SettlementProviderRequest {
                protocol: settlement.protocol,
                run_id: intent.run_id.clone(),
                payment_payload: settlement.payment_payload,
                payment_requirements: settlement.payment_requirements,
            },
        ))
    }

    pub(super) async fn load_verification(&self, intent: &PaymentIntent) -> Result<IncomingVerification> {
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::CredentialVerified, 0)
            .await?
            .ok_or_else(|| Error::Storage("credential verification event is missing".to_owned()))?;
        let verification_ref = event
            .payload
            .get("verification_ref")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Storage("verification reference is missing".to_owned()))?;
        Ok(IncomingVerification {
            intent: intent.clone(),
            valid: true,
            verification_ref: Some(verification_ref),
        })
    }

    pub(super) async fn ensure_credential_replay(
        &self,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
    ) -> Result<()> {
        let challenge = self.load_challenge(intent).await?;
        if credential.protocol != intent.protocol || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentConflict(
                "credential retry is bound to another protocol or challenge".to_owned(),
            ));
        }
        let adapter = self.protocols.get(&intent.protocol)?;
        let fingerprint = adapter.credential_fingerprint(&challenge, credential)?;
        let (stored_fingerprint, _) = self.load_submission(intent).await?;
        if fingerprint != stored_fingerprint {
            return Err(Error::PaymentConflict(
                "credential retry does not match the submitted payment proof".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn load_submission(
        &self,
        intent: &PaymentIntent,
    ) -> Result<(String, crate::protocol::ProtocolSettlement)> {
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Submitted, 0)
            .await?
            .ok_or_else(|| Error::Storage("durable submission event is missing".to_owned()))?;
        let fingerprint = event
            .payload
            .get("credential_fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Storage("durable credential fingerprint is missing".to_owned()))?;
        let sealed = event
            .payload
            .get("sealed_settlement")
            .cloned()
            .map(serde_json::from_value::<SealedSensitiveMaterial>)
            .transpose()?
            .ok_or_else(|| Error::Storage("durable settlement material is missing".to_owned()))?;
        let plaintext = self
            .material_protector
            .open(&SettlementFacts::settlement_material_context(intent), &sealed)?;
        Ok((fingerprint, serde_json::from_slice(&plaintext)?))
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/settlement/verification_test.rs"]
mod tests;
