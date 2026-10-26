//! Intent aggregate write primitives; callable only within the authoritative storage transactions.
macro_rules! payment_mutations {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
impl $repository {
    pub(crate) async fn insert_intent(
        connection: &mut Connection,
        intent: &PaymentIntent,
        credential_hash: Option<&str>,
    ) -> Result<()> {
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        Self::validate_intent_authority(connection, intent).await?;
        sqlx::query(
            "INSERT INTO pay_intent (id, scenario_id, run_id, wallet_id, policy_id, price_id, direction, protocol, \
     network, asset, token_contract, asset_decimals, recipient, amount, settled_amount, resource, \
     challenge_hash, credential_hash, request_hash, status, reservation_status, \
     reserved_amount, tx_hash, settlement_ref, next_reconcile_at, reconcile_attempts, lease_owner, \
     lease_expires_at, created_at, updated_at, expires_at, settled_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, \
             $19, $20, $21, $22, $23, $24, $25, 0, NULL, NULL, $26, $27, $28, $29)",
        )
        .bind(&intent.id)
        .bind(&intent.scenario_id)
        .bind(&intent.run_id)
        .bind(&intent.wallet_id)
        .bind(&intent.policy_id)
        .bind(&intent.price_id)
        .bind(intent.direction.as_str())
        .bind(&intent.protocol)
        .bind(&intent.asset.network)
        .bind(&intent.asset.asset)
        .bind(&intent.asset.token_contract)
        .bind(i16::from(intent.asset.decimals))
        .bind(&intent.recipient)
        .bind(AmountCodec::encode_amount(intent.amount)?)
        .bind(AmountCodec::encode_amount(intent.settled_amount)?)
        .bind(&intent.resource)
        .bind(&intent.challenge_hash)
        .bind(credential_hash)
        .bind(&intent.request_hash)
        .bind(intent.status.as_str())
        .bind(intent.reservation_status.as_str())
        .bind(AmountCodec::encode_amount(intent.reserved_amount)?)
        .bind(&intent.tx_hash)
        .bind(&intent.settlement_ref)
        .bind(intent.next_reconcile_at)
        .bind(intent.created_at)
        .bind(intent.updated_at)
        .bind(intent.expires_at)
        .bind(intent.settled_at)
        .execute(connection)
        .await
        .map_err(|error| crate::Error::conflict(format!("payment intent insert rejected: {error}")))?;
        Ok(())
    }

    pub(crate) async fn insert_approval_request(
        connection: &mut Connection,
        approval: &ApprovalRequest,
    ) -> Result<()> {
        if approval.id.trim().is_empty()
            || approval.intent_id.trim().is_empty()
            || approval.run_id.trim().is_empty()
            || approval.requested_amount.is_zero()
            || approval.requested_by.trim().is_empty()
            || approval.intent_hash.trim().is_empty()
        {
            return Err(Error::InvalidRequest(
                "approval request identity is incomplete".to_owned(),
            ));
        }
        let sealed_token = approval
            .sealed_grant_token
            .as_ref()
            .map(|material| {
                BaseRepository::encode_json(material, "pay_approval.sealed_grant_token")
            })
            .transpose()?;
        sqlx::query(
    "INSERT INTO pay_approval (id, intent_id, run_id, requested_amount, approved_amount, requested_by, approved_by, \
     status, intent_hash, grant_jti, grant_claims_hash, sealed_grant_token, expires_at, consumed_at, \
     created_at, approved_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
)
.bind(&approval.id)
.bind(&approval.intent_id)
.bind(&approval.run_id)
.bind(AmountCodec::encode_amount(approval.requested_amount)?)
.bind(AmountCodec::encode_optional_amount(approval.approved_amount)?)
.bind(&approval.requested_by)
.bind(&approval.approved_by)
.bind(approval.status.as_str())
.bind(&approval.intent_hash)
.bind(&approval.grant_jti)
.bind(&approval.grant_claims_hash)
.bind(sealed_token)
.bind(approval.expires_at)
.bind(approval.consumed_at)
.bind(approval.created_at)
.bind(approval.approved_at)
.execute(connection)
.await
.map_err(|error| crate::Error::conflict(format!("payment approval insert rejected: {error}")))?;
        Ok(())
    }

    pub(crate) async fn update_intent(
        connection: &mut Connection,
        intent: &PaymentIntent,
        expected_updated_at: i64,
        reservation_mutation: ReservationMutation,
    ) -> Result<()> {
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let existing = Self::load_intent_connection(connection, &intent.id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
        if existing.updated_at != expected_updated_at
            || intent.updated_at <= expected_updated_at
            || !existing.same_immutable_identity_as(intent)
            || intent.settled_amount < existing.settled_amount
            || !PaymentStorageRules::allowed_status_transition(existing.status, intent.status)
        {
            return Err(crate::Error::conflict(
                "payment intent identity, lifecycle, or revision changed",
            ));
        }
        match reservation_mutation {
            ReservationMutation::Unchanged
                if intent.reservation_status != existing.reservation_status
                    || intent.reserved_amount != existing.reserved_amount =>
            {
                return Err(crate::Error::conflict(
                    "generic payment update cannot change a reservation",
                ));
            }
            ReservationMutation::ReleaseOnly
                if existing.reservation_status != ReservationStatus::Reserved
                    || intent.reservation_status != ReservationStatus::Released
                    || intent.reserved_amount != existing.reserved_amount =>
            {
                return Err(crate::Error::conflict(
                    "reservation update must release one existing reservation",
                ));
            }
            ReservationMutation::Settlement
                if !matches!(
                    (existing.reservation_status, intent.reservation_status),
                    (
                        ReservationStatus::Reserved | ReservationStatus::Consumed,
                        ReservationStatus::Consumed
                    ) | (ReservationStatus::Unreserved, ReservationStatus::Unreserved)
                        | (ReservationStatus::Released, ReservationStatus::Released)
                ) =>
            {
                return Err(crate::Error::conflict(
                    "settlement has an invalid reservation transition",
                ));
            }
            ReservationMutation::ApprovalReserve
                if existing.reservation_status != ReservationStatus::Unreserved
                    || intent.reservation_status != ReservationStatus::Reserved
                    || intent.reserved_amount != intent.amount =>
            {
                return Err(crate::Error::conflict(
                    "approved payment must create exactly one full reservation",
                ));
            }
            _ => {}
        }
        let result = sqlx::query(
            "UPDATE pay_intent SET settled_amount = $1, status = $2, reservation_status = $3, reserved_amount = $4, \
     tx_hash = $5, settlement_ref = $6, next_reconcile_at = $7, updated_at = $8, settled_at = $9 \
     WHERE id = $10 AND updated_at = $11",
        )
        .bind(AmountCodec::encode_amount(intent.settled_amount)?)
        .bind(intent.status.as_str())
        .bind(intent.reservation_status.as_str())
        .bind(AmountCodec::encode_amount(intent.reserved_amount)?)
        .bind(&intent.tx_hash)
        .bind(&intent.settlement_ref)
        .bind(intent.next_reconcile_at)
        .bind(intent.updated_at)
        .bind(intent.settled_at)
        .bind(&intent.id)
        .bind(expected_updated_at)
        .execute(connection)
        .await
        .map_err(|error| crate::Error::conflict(format!("payment intent update rejected: {error}")))?;
        if result.rows_affected() != 1 {
            return Err(crate::Error::conflict("payment intent revision changed"));
        }
        Ok(())
    }

    async fn bind_credential_transition(
        connection: &mut Connection,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
        expected_updated_at: i64,
    ) -> Result<()> {
        if credential.credential_hash.trim().is_empty()
            || credential.protocol != intent.protocol
            || intent.challenge_hash.as_deref() != Some(credential.challenge_hash.as_str())
            || intent.direction != PaymentDirection::Inbound
        {
            return Err(crate::Error::conflict(
                "payment credential is not bound to the inbound challenge",
            ));
        }
        let existing_hash: Option<String> =
            sqlx::query_scalar("SELECT credential_hash FROM pay_intent WHERE id = $1")
                .bind(&intent.id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| {
                    BaseRepository::storage_error("load payment credential binding", error)
                })?
                .flatten();
        if existing_hash.is_some() {
            return Err(crate::Error::conflict(
                "payment credential is already bound",
            ));
        }
        Self::update_intent(
            connection,
            intent,
            expected_updated_at,
            ReservationMutation::Unchanged,
        )
        .await?;
        let result = sqlx::query(
            "UPDATE pay_intent SET credential_hash = $1 WHERE id = $2 AND credential_hash IS NULL",
        )
        .bind(&credential.credential_hash)
        .bind(&intent.id)
        .execute(connection)
        .await
        .map_err(|error| {
            crate::Error::conflict(format!("payment credential binding rejected: {error}"))
        })?;
        if result.rows_affected() != 1 {
            return Err(crate::Error::conflict(
                "payment credential binding lost its conditional update",
            ));
        }
        Ok(())
    }
}

};
}
pub(crate) use payment_mutations;

#[cfg(test)]
#[path = "../../../tests/storage/payment/mutations_test.rs"]
mod tests;
