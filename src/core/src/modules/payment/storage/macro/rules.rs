use crate::{
    model::{
        payment_recipients_match, Amount, PaymentChallenge, PaymentDirection, PaymentEvent, PaymentEventType,
        PaymentIntent, PaymentLedgerEntry, PaymentStatus, ReservationStatus,
    },
    Error, Result,
};

pub(crate) struct PaymentStorageRules;
impl PaymentStorageRules {
    pub(crate) fn allowed_status_transition(from: PaymentStatus, to: PaymentStatus) -> bool {
        from == to
            || matches!(
                (from, to),
                (
                    PaymentStatus::Created,
                    PaymentStatus::PendingApproval
                        | PaymentStatus::Authorized
                        | PaymentStatus::Submitted
                        | PaymentStatus::Failed
                        | PaymentStatus::Expired
                ) | (
                    PaymentStatus::PendingApproval,
                    PaymentStatus::Authorized | PaymentStatus::Failed | PaymentStatus::Expired
                ) | (
                    PaymentStatus::Authorized,
                    PaymentStatus::Signed
                        | PaymentStatus::Submitted
                        | PaymentStatus::PendingSettlement
                        | PaymentStatus::Failed
                        | PaymentStatus::Expired
                        | PaymentStatus::Unknown
                ) | (
                    PaymentStatus::Signed,
                    PaymentStatus::Submitted
                        | PaymentStatus::PendingSettlement
                        | PaymentStatus::Settled
                        | PaymentStatus::Failed
                        | PaymentStatus::Unknown
                ) | (
                    PaymentStatus::Submitted,
                    PaymentStatus::PendingSettlement
                        | PaymentStatus::Settled
                        | PaymentStatus::Failed
                        | PaymentStatus::Unknown
                ) | (
                    PaymentStatus::PendingSettlement,
                    PaymentStatus::Settled | PaymentStatus::Failed | PaymentStatus::Expired | PaymentStatus::Unknown
                ) | (
                    PaymentStatus::Unknown,
                    PaymentStatus::Submitted
                        | PaymentStatus::PendingSettlement
                        | PaymentStatus::Settled
                        | PaymentStatus::Failed
                        | PaymentStatus::Expired
                )
            )
    }
    pub(crate) fn challenge_matches_intent(
        challenge: &PaymentChallenge,
        intent: &PaymentIntent,
        direction: PaymentDirection,
    ) -> bool {
        intent.direction == direction
            && challenge.scenario_id == intent.scenario_id
            && challenge.price_id == intent.price_id
            && challenge.resource == intent.resource
            && challenge.protocol == intent.protocol
            && challenge.asset == intent.asset
            && challenge.amount == intent.amount
            && payment_recipients_match(&intent.asset.network, &challenge.recipient, &intent.recipient)
            && !challenge.nonce.trim().is_empty()
            && !challenge.challenge_hash.trim().is_empty()
            && intent.challenge_hash.as_deref() == Some(challenge.challenge_hash.as_str())
            && intent.expires_at == Some(challenge.expires_at)
    }
    pub(crate) fn checked_usage(reserved: Amount, confirmed: Amount, scope: &str) -> Result<Amount> {
        reserved
            .checked_add(confirmed)
            .ok_or_else(|| Error::Storage(format!("{scope} payment usage overflowed u128")))
    }
    pub(crate) fn validate_challenge_created_event(
        events: &[PaymentEvent],
        intent: &PaymentIntent,
        expected: &PaymentChallenge,
    ) -> Result<()> {
        let matching = events
            .iter()
            .filter(|event| event.event_type == PaymentEventType::ChallengeCreated)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(crate::Error::conflict(
                "inbound issuance must contain exactly one created challenge event",
            ));
        }
        let event = matching[0];
        let challenge: PaymentChallenge = event
            .payload
            .get("challenge")
            .cloned()
            .ok_or_else(|| crate::Error::conflict("created challenge event has no complete challenge"))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|_| crate::Error::conflict("created challenge event contains an invalid challenge"))
            })?;
        if event.intent_id != intent.id
            || event.attempt_no != 0
            || event.signature_ref.is_some()
            || event.tx_hash.is_some()
            || event.created_at != intent.created_at
            || challenge != *expected
            || !Self::challenge_matches_intent(&challenge, intent, PaymentDirection::Inbound)
        {
            return Err(crate::Error::conflict(
                "created challenge event is not bound to the inbound payment intent",
            ));
        }
        Ok(())
    }
    pub(crate) fn validate_challenge_received_event(events: &[PaymentEvent], intent: &PaymentIntent) -> Result<()> {
        let matching = events
            .iter()
            .filter(|event| event.event_type == PaymentEventType::ChallengeReceived)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(crate::Error::conflict(
                "outbound authorization must contain exactly one received challenge event",
            ));
        }
        let event = matching[0];
        let challenge: PaymentChallenge = event
            .payload
            .get("challenge")
            .cloned()
            .ok_or_else(|| crate::Error::conflict("received challenge event has no complete challenge"))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|_| crate::Error::conflict("received challenge event contains an invalid challenge"))
            })?;
        if event.intent_id != intent.id
            || event.attempt_no != 0
            || event.signature_ref.is_some()
            || event.tx_hash.is_some()
            || event.created_at != intent.created_at
            || event.payload.get("challenge_hash").and_then(serde_json::Value::as_str)
                != Some(challenge.challenge_hash.as_str())
            || !Self::challenge_matches_intent(&challenge, intent, PaymentDirection::Outbound)
        {
            return Err(crate::Error::conflict(
                "received challenge event is not bound to the outbound payment intent",
            ));
        }
        Ok(())
    }
    pub(crate) fn validate_progress_transition(
        existing: &PaymentIntent,
        intent: &PaymentIntent,
        expected_updated_at: i64,
        events: &[PaymentEvent],
    ) -> Result<ReservationMutation> {
        if existing.updated_at != expected_updated_at
            || !existing.same_immutable_identity_as(intent)
            || !Self::allowed_status_transition(existing.status, intent.status)
            || !matches!(
                intent.status,
                PaymentStatus::PendingSettlement
                    | PaymentStatus::Unknown
                    | PaymentStatus::Failed
                    | PaymentStatus::Expired
            )
            || intent.settled_amount != existing.settled_amount
            || intent.tx_hash != existing.tx_hash
            || intent.settlement_ref != existing.settlement_ref
            || intent.settled_at != existing.settled_at
            || events.is_empty()
            || events.iter().any(|event| {
                event.intent_id != intent.id
                    || !matches!(
                        event.event_type,
                        PaymentEventType::CredentialVerified
                            | PaymentEventType::SettlementRequested
                            | PaymentEventType::Retried
                            | PaymentEventType::Failed
                    )
            })
        {
            return Err(crate::Error::conflict("non-financial payment progress is invalid"));
        }
        if intent.reservation_status == existing.reservation_status
            && intent.reserved_amount == existing.reserved_amount
        {
            return Ok(ReservationMutation::Unchanged);
        }
        if existing.reservation_status == ReservationStatus::Reserved
            && intent.reservation_status == ReservationStatus::Released
            && intent.reserved_amount == existing.reserved_amount
            && matches!(intent.status, PaymentStatus::Failed | PaymentStatus::Expired)
        {
            return Ok(ReservationMutation::ReleaseOnly);
        }
        Err(crate::Error::conflict(
            "non-financial payment progress cannot consume or replace a reservation",
        ))
    }
    pub(crate) fn validate_settlement_transition(
        existing: &PaymentIntent,
        intent: &PaymentIntent,
        expected_updated_at: i64,
        entries: &[PaymentLedgerEntry],
    ) -> Result<()> {
        if existing.updated_at != expected_updated_at
            || !existing.same_immutable_identity_as(intent)
            || !Self::allowed_status_transition(existing.status, intent.status)
        {
            return Err(crate::Error::conflict(
                "settlement payment identity or revision changed",
            ));
        }
        for entry in entries {
            if entry.intent_id.as_deref() != Some(intent.id.as_str())
                || entry.scenario_id.as_deref() != Some(intent.scenario_id.as_str())
                || Some(entry.wallet_id.as_str()) != intent.wallet_id.as_deref()
            {
                return Err(crate::Error::conflict(
                    "settlement ledger entry is not bound to its payment intent",
                ));
            }
        }
        if existing.reservation_status == ReservationStatus::Reserved {
            if intent.reservation_status != ReservationStatus::Consumed {
                return Err(crate::Error::conflict(
                    "settlement must consume the existing reservation",
                ));
            }
            let matching_fact = entries.iter().any(|entry| {
                entry.direction == intent.direction
                    && entry.asset.budget_unit() == intent.asset.budget_unit()
                    && entry.intent_id.as_deref() == Some(intent.id.as_str())
            });
            if !matching_fact {
                return Err(crate::Error::conflict(
                    "reservation consumption requires a matching confirmed ledger fact",
                ));
            }
        } else if intent.reservation_status == ReservationStatus::Consumed
            && existing.reservation_status != ReservationStatus::Consumed
        {
            return Err(crate::Error::conflict("an unreserved payment cannot become consumed"));
        }
        if intent.status == PaymentStatus::Settled
            && (intent.settled_at.is_none() || intent.settled_amount.is_zero() || entries.is_empty())
        {
            return Err(crate::Error::conflict(
                "settled payment is missing confirmed settlement facts",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ReservationMutation {
    Unchanged,
    ReleaseOnly,
    Settlement,
    ApprovalReserve,
}
