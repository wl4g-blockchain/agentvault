use super::*;
use crate::payment::validation::PaymentValidation;

pub(crate) struct SettlementFacts;

impl SettlementFacts {
    pub(crate) fn durable_receipt(
        settled: Option<&PaymentEvent>,
        reconciled: Option<&PaymentEvent>,
    ) -> Result<PaymentReceipt> {
        settled
            .or(reconciled)
            .and_then(|event| event.payload.get("receipt").cloned())
            .map(serde_json::from_value)
            .transpose()?
            .ok_or_else(|| Error::Storage("settled intent has no durable receipt".to_owned()))
    }

    pub(crate) fn is_concurrent_settlement_winner(expected: &PaymentIntent, current: &PaymentIntent) -> bool {
        current.status == PaymentStatus::Settled && current.same_immutable_identity_as(expected)
    }

    pub(crate) fn authorization_query(
        intent: &PaymentIntent,
        settlement: &crate::protocol::ProtocolSettlement,
        from_block: u64,
    ) -> Result<(EvmAuthorizationQuery, i64)> {
        if !SettlementFacts::uses_eip3009_recovery(intent) || settlement.protocol != "x402" {
            return Err(Error::Settlement(
                "EIP-3009 recovery requires an inbound x402 intent".to_owned(),
            ));
        }
        let token_contract = intent
            .asset
            .token_contract
            .clone()
            .ok_or_else(|| Error::Settlement("x402 recovery intent has no token contract".to_owned()))?;
        let authorization = X402Evm::authorization_identity(&settlement.payment_payload)?;
        Ok((
            EvmAuthorizationQuery {
                network: intent.asset.network.clone(),
                token_contract,
                payer: authorization.payer,
                nonce: authorization.nonce,
                recipient: intent.recipient.clone(),
                amount: intent.amount,
                from_block,
            },
            authorization.valid_before,
        ))
    }

    pub(crate) fn uses_eip3009_recovery(intent: &PaymentIntent) -> bool {
        intent.direction == PaymentDirection::Inbound
            && intent.protocol == "x402"
            && is_evm_payment_network(&intent.asset.network)
    }

    pub(crate) fn settlement_request_payload(intent: &PaymentIntent, from_block: Option<u64>) -> Value {
        let mut payload = serde_json::Map::from_iter([
            ("run_id".to_owned(), Value::String(intent.run_id.clone())),
            (
                "recovery".to_owned(),
                Value::String(
                    if SettlementFacts::uses_eip3009_recovery(intent) {
                        "eip3009_authorization_event"
                    } else {
                        "provider_verify"
                    }
                    .to_owned(),
                ),
            ),
        ]);
        if let Some(from_block) = from_block {
            payload.insert("from_block".to_owned(), Value::from(from_block));
        }
        Value::Object(payload)
    }

    pub(crate) fn settlement_request_from_block(intent: &PaymentIntent, event: &PaymentEvent) -> Result<Option<u64>> {
        let payload = event
            .payload
            .as_object()
            .ok_or_else(|| Error::Storage("settlement operation payload is malformed".to_owned()))?;
        let expected_recovery = if SettlementFacts::uses_eip3009_recovery(intent) {
            "eip3009_authorization_event"
        } else {
            "provider_verify"
        };
        let exact_fields = payload.len()
            == if SettlementFacts::uses_eip3009_recovery(intent) {
                3
            } else {
                2
            }
            && payload.contains_key("run_id")
            && payload.contains_key("recovery")
            && (!SettlementFacts::uses_eip3009_recovery(intent) || payload.contains_key("from_block"));
        if event.intent_id != intent.id
            || event.event_type != PaymentEventType::SettlementRequested
            || event.attempt_no != 0
            || event.signature_ref.is_some()
            || event.tx_hash.is_some()
            || !exact_fields
            || payload.get("run_id").and_then(Value::as_str) != Some(intent.run_id.as_str())
            || payload.get("recovery").and_then(Value::as_str) != Some(expected_recovery)
        {
            return Err(Error::Storage(
                "durable settlement operation does not match the payment intent".to_owned(),
            ));
        }
        let from_block = payload.get("from_block").map(|value| {
            value
                .as_u64()
                .ok_or_else(|| Error::Storage("settlement recovery checkpoint is malformed".to_owned()))
        });
        from_block.transpose()
    }

    pub(crate) fn authorization_expiry_deadline(intent: &PaymentIntent, valid_before: i64) -> i64 {
        intent.expires_at.unwrap_or(valid_before).max(valid_before)
    }

    pub(crate) fn authorization_is_definitively_expired(observed_at: i64, chain_observed_at: i64, expiry: i64) -> bool {
        observed_at >= expiry && chain_observed_at >= expiry
    }

    pub(crate) fn reservation_after_non_settlement(
        status: PaymentStatus,
        reservation_status: ReservationStatus,
    ) -> ReservationStatus {
        if matches!(status, PaymentStatus::Failed | PaymentStatus::Expired)
            && reservation_status == ReservationStatus::Reserved
        {
            ReservationStatus::Released
        } else {
            reservation_status
        }
    }

    pub(crate) fn validate_issue_command(command: &IssuePaymentChallenge) -> Result<()> {
        PaymentValidation::validate_identifier("scenario_id", &command.scenario_id)?;
        PaymentValidation::validate_identifier("run_id", &command.run_id)?;
        PaymentValidation::validate_identifier("offer_id", &command.offer_id)?;
        PaymentValidation::validate_identifier("protocol", &command.protocol)?;
        PaymentValidation::validate_identifier("request_hash", &command.request_hash)?;
        if command.resource.trim().is_empty()
            || PaymentValidation::canonical_http_origin(&command.resource)? != command.origin
        {
            return Err(Error::InvalidRequest(
                "resource and canonical origin binding is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn build_inbound_intent(
        command: &IssuePaymentChallenge,
        offer: &PaymentPrice,
        wallet: &crate::model::wallet::Wallet,
        challenge: &PaymentChallenge,
        now: i64,
    ) -> Result<PaymentIntent> {
        let intent = PaymentIntent {
            id: Uuid::new_v4().to_string(),
            scenario_id: command.scenario_id.clone(),
            run_id: command.run_id.clone(),
            wallet_id: Some(offer.wallet_id.clone()),
            policy_id: Some(offer.policy_id.clone()),
            offer_id: Some(offer.id.clone()),
            direction: PaymentDirection::Inbound,
            protocol: command.protocol.clone(),
            asset: offer.asset.clone(),
            recipient: wallet.address.clone(),
            amount: offer.amount,
            settled_amount: Amount::ZERO,
            resource: offer.resource.clone(),
            challenge_hash: Some(challenge.challenge_hash.clone()),
            request_hash: command.request_hash.clone(),
            status: PaymentStatus::Created,
            reservation_status: ReservationStatus::Reserved,
            reserved_amount: offer.amount,
            tx_hash: None,
            settlement_ref: None,
            next_reconcile_at: None,
            created_at: now,
            updated_at: now,
            expires_at: Some(challenge.expires_at),
            settled_at: None,
        };
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid inbound intent field: {field}")))?;
        Ok(intent)
    }

    pub(crate) fn validate_verify_command(command: &VerifyIncomingPayment) -> Result<()> {
        PaymentValidation::validate_identifier("scenario_id", &command.scenario_id)?;
        PaymentValidation::validate_identifier("run_id", &command.run_id)?;
        PaymentValidation::validate_identifier("challenge_hash", &command.challenge_hash)?;
        PaymentValidation::validate_identifier("request_hash", &command.request_hash)?;
        PaymentValidation::validate_identifier("credential.protocol", &command.credential.protocol)?;
        PaymentValidation::validate_identifier("credential.challenge_hash", &command.credential.challenge_hash)?;
        PaymentValidation::validate_identifier("credential.credential_hash", &command.credential.credential_hash)?;
        if command.credential.challenge_hash != command.challenge_hash {
            return Err(Error::PaymentConflict(
                "credential is bound to another challenge".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_settle_command(command: &SettleIncomingPayment) -> Result<()> {
        PaymentValidation::validate_identifier("scenario_id", &command.scenario_id)?;
        PaymentValidation::validate_identifier("run_id", &command.run_id)?;
        PaymentValidation::validate_identifier("intent_id", &command.intent_id)?;
        PaymentValidation::validate_identifier("verification_ref", &command.verification_ref)
    }

    pub(crate) fn ensure_settlement_intent_binding(
        intent: &PaymentIntent,
        command: &SettleIncomingPayment,
    ) -> Result<()> {
        if intent.run_id != command.run_id || intent.direction != PaymentDirection::Inbound {
            return Err(Error::Forbidden);
        }
        Ok(())
    }

    pub(crate) fn ensure_replay_identity(intent: &PaymentIntent, command: &IssuePaymentChallenge) -> Result<()> {
        if intent.scenario_id != command.scenario_id
            || intent.run_id != command.run_id
            || intent.offer_id.as_deref() != Some(command.offer_id.as_str())
            || intent.resource != command.resource
            || intent.protocol != command.protocol
            || intent.request_hash != command.request_hash
        {
            return Err(Error::PaymentConflict(
                "run_id was reused for different challenge content".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn ensure_incoming_identity(intent: &PaymentIntent, run_id: &str, request_hash: &str) -> Result<()> {
        if intent.direction != PaymentDirection::Inbound || intent.run_id != run_id {
            return Err(Error::Forbidden);
        }
        match intent.check_replay(request_hash) {
            crate::model::ReplayCheck::Replay => Ok(()),
            crate::model::ReplayCheck::Conflict => Err(Error::PaymentConflict(
                "run_id was reused for different request content".to_owned(),
            )),
        }
    }

    pub(crate) fn validate_payee_challenge(
        challenge: &PaymentChallenge,
        offer: &PaymentPrice,
        wallet: &crate::model::wallet::Wallet,
        now: i64,
    ) -> Result<()> {
        if challenge.scenario_id != offer.scenario_id
            || challenge.offer_id.as_deref() != Some(offer.id.as_str())
            || challenge.resource != offer.resource
            || challenge.asset != offer.asset
            || challenge.amount != offer.amount
            || !wallet.address_matches(&challenge.recipient)
            || challenge.expires_at <= now
            || challenge.challenge_hash.trim().is_empty()
        {
            return Err(Error::PaymentProtocol(
                "adapter produced a challenge inconsistent with the offer".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn evidence_hash(value: &Value) -> Result<String> {
        let encoded = serde_json::to_vec(value)?;
        let mut hash = Sha256::new();
        hash.update(b"agentvault.settlement-evidence.v1");
        hash.update(u64::try_from(encoded.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(encoded);
        Ok(hex::encode(hash.finalize()))
    }

    pub(crate) fn provider_verification_ref(intent_id: &str, result: &SettlementProviderResult) -> Result<String> {
        SettlementFacts::evidence_hash(&json!({
            "intent_id": intent_id,
            "operation": "verify",
            "result": result,
        }))
    }

    pub(crate) fn next_timestamp(previous: i64, now: i64) -> i64 {
        now.max(previous.saturating_add(1))
    }

    pub(crate) fn settlement_material_context(intent: &PaymentIntent) -> String {
        format!("payment-intent:{}:{}", intent.id, intent.protocol)
    }

    pub(crate) fn confirmed_intent(
        intent: &PaymentIntent,
        result: &SettlementProviderResult,
        updated_at: i64,
        effective_at: i64,
    ) -> PaymentIntent {
        let mut settled = intent.clone();
        settled.status = PaymentStatus::Settled;
        settled.settled_amount = intent.amount;
        settled.tx_hash.clone_from(&result.transaction);
        settled.settlement_ref.clone_from(&result.settlement_ref);
        settled.next_reconcile_at = None;
        if settled.reservation_status == ReservationStatus::Reserved {
            settled.reservation_status = ReservationStatus::Consumed;
        }
        settled.updated_at = updated_at;
        // Preserve the provider timestamp in the receipt and ledger, while the
        // local intent lifecycle remains monotonic across bounded clock skew.
        settled.settled_at = Some(effective_at.max(intent.created_at));
        settled
    }

    pub(crate) fn confirmed_receipt(
        intent: &PaymentIntent,
        result: &SettlementProviderResult,
        confirmed_at: i64,
    ) -> Result<PaymentReceipt> {
        let receipt = PaymentReceipt {
            intent_id: intent.id.clone(),
            scenario_id: intent.scenario_id.clone(),
            direction: PaymentDirection::Inbound,
            protocol: intent.protocol.clone(),
            asset: intent.asset.clone(),
            recipient: intent.recipient.clone(),
            payer: result.payer.clone(),
            requested_amount: intent.amount,
            settled_amount: intent.amount,
            status: PaymentStatus::Settled,
            tx_hash: result.transaction.clone(),
            settlement_ref: result.settlement_ref.clone(),
            evidence_hash: SettlementFacts::evidence_hash(&serde_json::to_value(result)?)?,
            confirmed_at,
        };
        receipt
            .validate()
            .map_err(|field| Error::Settlement(format!("invalid confirmed receipt field: {field}")))?;
        Ok(receipt)
    }

    pub(crate) fn confirmed_ledger(
        intent: &PaymentIntent,
        authority: &str,
        result: &SettlementProviderResult,
        effective_at: i64,
        created_at: i64,
    ) -> Result<PaymentLedgerEntry> {
        let posting_key = crate::model::PaymentLedgerEntry::settlement_posting_key(
            authority,
            &intent.asset.network,
            result.transaction.as_deref(),
            result.settlement_ref.as_deref(),
            result.effect_ref.as_deref(),
        )
        .map_err(|field| Error::Settlement(format!("invalid settlement posting evidence: {field}")))?;
        let ledger = PaymentLedgerEntry {
            id: Uuid::new_v4().to_string(),
            intent_id: Some(intent.id.clone()),
            scenario_id: Some(intent.scenario_id.clone()),
            wallet_id: intent
                .wallet_id
                .clone()
                .ok_or_else(|| Error::Storage("inbound intent has no wallet".to_owned()))?,
            entry_type: LedgerEntryType::Payment,
            direction: PaymentDirection::Inbound,
            asset: intent.asset.clone(),
            amount: intent.amount,
            posting_key,
            tx_hash: result.transaction.clone(),
            settlement_ref: result.settlement_ref.clone(),
            effect_ref: result.effect_ref.clone(),
            transfer_ref: None,
            reversal_of: None,
            effective_at,
            created_at,
        };
        ledger
            .validate()
            .map_err(|field| Error::Settlement(format!("invalid ledger field: {field}")))?;
        Ok(ledger)
    }

    pub(crate) fn confirmed_event(
        intent: &PaymentIntent,
        result: &SettlementProviderResult,
        receipt: &PaymentReceipt,
        confirmed_at: i64,
    ) -> PaymentEvent {
        PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Settled,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: result.transaction.clone(),
            payload: json!({"receipt": receipt}),
            created_at: confirmed_at,
        }
    }

    pub(crate) fn confirmed_outbox(intent: &PaymentIntent, confirmed_at: i64) -> OutboxMessage {
        OutboxMessage {
            id: Uuid::new_v4().to_string(),
            aggregate_type: "payment_intent".to_owned(),
            aggregate_id: intent.id.clone(),
            event_type: "payment_settled".to_owned(),
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({"intent_id": intent.id, "scenario_id": intent.scenario_id, "run_id": intent.run_id}),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            available_at: confirmed_at,
            lease_owner: None,
            lease_expires_at: None,
            last_error: None,
            created_at: confirmed_at,
            delivered_at: None,
        }
    }

    pub(crate) fn settled_response(intent: PaymentIntent, receipt: PaymentReceipt) -> SettledIncomingPayment {
        SettledIncomingPayment { intent, receipt }
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/settlement/facts_test.rs"]
mod tests;
