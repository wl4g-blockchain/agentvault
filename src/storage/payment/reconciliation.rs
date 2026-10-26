//! Leased payment reconciliation storage, sharing the same intent transaction authority.
macro_rules! payment_reconciliation {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
impl $repository {
    async fn claim_reconciliation_batch(
        &self,
        request: &ReconciliationLeaseRequest,
    ) -> Result<Vec<ReconciliationClaim>> {
        if request.owner.trim().is_empty()
            || request.lease_until <= request.now
            || request.limit == 0
        {
            return Err(Error::InvalidRequest(
                "invalid reconciliation lease request".to_owned(),
            ));
        }
        let request = request.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let ids: Vec<String> = sqlx::query_scalar(concat!(
                        "SELECT id FROM pay_intent WHERE ( \
                       (status IN ('signed', 'submitted', 'pending_settlement', 'unknown') \
                        AND next_reconcile_at IS NOT NULL AND next_reconcile_at <= $1) \
                       OR (status = 'created' AND expires_at IS NOT NULL AND expires_at <= $1) \
                     ) \
                     AND (lease_expires_at IS NULL OR lease_expires_at <= $1) \
                     ORDER BY COALESCE(next_reconcile_at, expires_at), created_at, id \
                     LIMIT $2",
                        $skip
                    ))
                    .bind(request.now)
                    .bind(i64::from(request.limit))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| {
                        BaseRepository::storage_error("select due payment reconciliation", error)
                    })?;
                    let mut intents = Vec::with_capacity(ids.len());
                    for id in ids {
                        let result = sqlx::query(
                            "UPDATE pay_intent SET lease_owner = $1, lease_expires_at = $2, \
                         reconcile_attempts = reconcile_attempts + 1 WHERE id = $3 \
                         AND ( \
                           (status IN ('signed', 'submitted', 'pending_settlement', 'unknown') \
                            AND next_reconcile_at IS NOT NULL AND next_reconcile_at <= $4) \
                           OR (status = 'created' AND expires_at IS NOT NULL AND expires_at <= $4) \
                         ) AND (lease_expires_at IS NULL OR lease_expires_at <= $4) \
                         AND reconcile_attempts < 4294967295",
                        )
                        .bind(&request.owner)
                        .bind(request.lease_until)
                        .bind(&id)
                        .bind(request.now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| {
                            BaseRepository::storage_error("claim payment reconciliation", error)
                        })?;
                        if result.rows_affected() != 1 {
                            return Err(crate::Error::conflict(
                                "payment reconciliation claim lost its conditional update",
                            ));
                        }
                        let attempt: i64 = sqlx::query_scalar(
                            "SELECT reconcile_attempts FROM pay_intent WHERE id = $1",
                        )
                        .bind(&id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|error| {
                            BaseRepository::storage_error(
                                "load payment reconciliation attempt",
                                error,
                            )
                        })?;
                        let attempt_no = u32::try_from(attempt).map_err(|_| {
                            Error::Storage("payment reconciliation attempt is invalid".to_owned())
                        })?;
                        let intent = Self::load_intent_connection(connection, &id)
                            .await?
                            .ok_or_else(|| {
                                Error::Storage("claimed payment intent disappeared".to_owned())
                            })?;
                        intents.push(ReconciliationClaim { intent, attempt_no });
                    }
                    Ok(intents)
                })
            })
            .await
    }

    async fn load_outbound_evidence_event(
        connection: &mut Connection,
        intent_id: &str,
    ) -> Result<Option<PaymentEvent>> {
        let row = sqlx::query(concat!(
                    "SELECT * FROM pay_event WHERE intent_id = $1 AND event_type = 'submitted' AND attempt_no = 0",
                    $share
                ))
                .bind(intent_id)
                .fetch_optional(connection)
                .await
                .map_err(|error| BaseRepository::storage_error("load outbound merchant evidence", error))?;
        row.as_ref().map(EventRepository::decode_row).transpose()
    }

    async fn update_reconciliation_lease(
        storage: &$repository,
        intent_id: &str,
        owner: &str,
        lease_until: Option<i64>,
        next_attempt_at: Option<i64>,
    ) -> Result<()> {
        if intent_id.trim().is_empty()
            || owner.trim().is_empty()
            || lease_until == Some(0)
            || next_attempt_at == Some(0)
        {
            return Err(Error::InvalidRequest(
                "invalid reconciliation lease transition".to_owned(),
            ));
        }
        let intent_id = intent_id.to_owned();
        let owner = owner.to_owned();
        storage
                    .base
                    .transaction(|connection| {
                        Box::pin(async move {
                            let result = if let Some(lease_until) = lease_until {
                                sqlx::query(
                                    "UPDATE pay_intent SET lease_expires_at = $1 WHERE id = $2 AND lease_owner = $3 \
                     AND status IN ('created', 'signed', 'submitted', 'pending_settlement', 'unknown')",
                                )
                                .bind(lease_until)
                                .bind(&intent_id)
                                .bind(&owner)
                                .execute(&mut *connection)
                                .await
                            } else {
                                sqlx::query(
                            "UPDATE pay_intent SET lease_owner = NULL, lease_expires_at = NULL, next_reconcile_at = $1 \
                     WHERE id = $2 AND lease_owner = $3 \
                     AND status IN ('created', 'signed', 'submitted', 'pending_settlement', 'unknown')",
                        )
                        .bind(next_attempt_at)
                        .bind(&intent_id)
                        .bind(&owner)
                        .execute(&mut *connection)
                        .await
                            }
                            .map_err(|error| {
                                BaseRepository::storage_error("update payment reconciliation lease", error)
                            })?;
                            if result.rows_affected() != 1 {
                                return Err(crate::Error::conflict(
                                    "payment reconciliation lease is not held by this owner",
                                ));
                            }
                            Ok(())
                        })
                    })
                    .await
    }
}


    };
}
pub(crate) use payment_reconciliation;

#[cfg(test)]
#[path = "../../../tests/storage/payment/reconciliation_test.rs"]
mod tests;
