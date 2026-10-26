//! Shared approval SQL transactions; concrete providers supply only driver types and locks.

macro_rules! approval_repository {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
use crate::model::ApprovalDecisionKind;
use crate::model::ApprovalRequest;
use crate::model::ApprovalStatus;
use crate::model::PaymentStatus;
use crate::model::ReservationStatus;
use crate::storage::approval::ApprovalStorageRules;
use crate::storage::payment::ReservationMutation;
use crate::storage::ApprovalConsumption;
use crate::storage::IApprovalRepository;
use crate::storage::ApprovalWrite;
use crate::storage::IdempotentWrite;
use crate::storage::Page;
use crate::storage::PageRequest;
use crate::utils::secure_material::SealedSensitiveMaterial;
use crate::utils::storage::AmountEncoding as _;
use crate::utils::storage::PaymentStorageCodec;
use crate::Error;
use crate::Result;
use async_trait::async_trait;

#[async_trait]
impl IApprovalRepository for $repository {
    async fn request(
        &self,
        approval: &ApprovalRequest,
    ) -> Result<IdempotentWrite<ApprovalRequest>> {
        let approval = approval.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    Self::lock_approval_intent(connection, &approval.intent_id).await?;
                    let rows = sqlx::query(concat!(
                        "SELECT * FROM pay_approval WHERE id = $1 OR \
                     (intent_id = $2 AND status IN ('pending', 'approved'))",
                        $update
                    ))
                    .bind(&approval.id)
                    .bind(&approval.intent_id)
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| {
                        BaseRepository::storage_error("find approval request replay", error)
                    })?;
                    if !rows.is_empty() {
                        if rows.len() != 1 {
                            return Err(crate::Error::conflict(
                                "approval identifiers collide with multiple rows",
                            ));
                        }
                        let existing = Self::decode_row(&rows[0])?;
                        return if existing == approval {
                            Ok(IdempotentWrite::Replay(existing))
                        } else {
                            Err(crate::Error::conflict(
                                "approval request identity is bound to different content",
                            ))
                        };
                    }
                    if approval.status != ApprovalStatus::Pending
                        || approval.approved_amount.is_some()
                        || approval.approved_by.is_some()
                        || approval.has_persisted_grant()
                        || approval.consumed_at.is_some()
                        || approval.approved_at.is_some()
                    {
                        return Err(Error::InvalidRequest(
                            "new approval request must have a pending lifecycle".to_owned(),
                        ));
                    }
                    PaymentRepository::insert_approval_request(connection, &approval).await?;
                    Ok(IdempotentWrite::Inserted(approval))
                })
            })
            .await
    }

    async fn get(&self, approval_id: &str) -> Result<Option<ApprovalRequest>> {
        let row = dynamic_query!(
            select,
            crate::model::ApprovalRequest,
            crate::storage::EntityQuery::new().eq("id", approval_id)
        )?
        .build()
        .fetch_optional(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("get approval request", error))?;
        row.as_ref().map(Self::decode_row).transpose()
    }

    async fn get_active_for_intent(&self, intent_id: &str) -> Result<Option<ApprovalRequest>> {
        let row = sqlx::query(
            "SELECT * FROM pay_approval WHERE intent_id = $1 AND status IN ('pending', 'approved')",
        )
        .bind(intent_id)
        .fetch_optional(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("get active payment approval", error))?;
        row.as_ref().map(Self::decode_row).transpose()
    }

    async fn list_pending(
        &self,
        scenario_id: &str,
        now: i64,
        page: PageRequest,
    ) -> Result<Page<ApprovalRequest>> {
        let (limit, offset) = BaseRepository::page_bounds(page)?;
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pay_approval a JOIN pay_intent i ON i.id = a.intent_id \
             WHERE i.scenario_id = $1 AND a.status = 'pending' AND a.expires_at > $2",
        )
        .bind(scenario_id)
        .bind(now)
        .fetch_one(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("count pending approvals", error))?;
        let rows = sqlx::query(
            "SELECT a.* FROM pay_approval a JOIN pay_intent i ON i.id = a.intent_id \
             WHERE i.scenario_id = $1 AND a.status = 'pending' AND a.expires_at > $2 \
             ORDER BY a.created_at, a.id LIMIT $3 OFFSET $4",
        )
        .bind(scenario_id)
        .bind(now)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("list pending approvals", error))?;
        Ok(Page {
            items: rows
                .iter()
                .map(Self::decode_row)
                .collect::<Result<Vec<_>>>()?,
            total: u64::try_from(total)
                .map_err(|_| Error::Storage("negative approval count".to_owned()))?,
        })
    }

    // Keeping this transaction in one method makes its lifecycle branches and
    // compare-and-swap boundary visible together.
    #[allow(clippy::too_many_lines)]
    async fn decide(&self, write: ApprovalWrite<'_>) -> Result<ApprovalRequest> {
        let decision = write.decision.clone();
        let grant = write.grant.cloned();
        let transition = write
            .intent_transition
            .map(|transition| (transition.intent.clone(), transition.expected_updated_at));
        let event = write.event.clone();
        let outbox = write.outbox.to_vec();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let row = sqlx::query(concat!("SELECT * FROM pay_approval WHERE id = $1", $update))
                        .bind(&decision.approval_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("load approval decision target", error))?
                        .ok_or_else(|| Error::NotFound(format!("payment approval {}", decision.approval_id)))?;
                    let approval = Self::decode_row(&row)?;
                    approval
                        .validate_decision(&decision, decision.decided_at)
                        .map_err(|reason| crate::Error::conflict(format!("approval decision is invalid: {reason}")))?;
                    let intent_row = sqlx::query(concat!("SELECT * FROM pay_intent WHERE id = $1", $update))
                        .bind(&approval.intent_id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|error| {
                            BaseRepository::storage_error("load approval payment intent", error)
                        })?;
                    let intent = PaymentRepository::decode_row(&intent_row)?;
                    if approval.intent_hash != intent.request_hash || event.intent_id != intent.id {
                        return Err(crate::Error::conflict(
                            "approval no longer matches its immutable payment intent",
                        ));
                    }

                    let persisted = match decision.decision {
                        ApprovalDecisionKind::Approve => {
                            let grant = grant
                                .as_ref()
                                .ok_or_else(|| crate::Error::conflict("approval decision is missing its JWT grant"))?;
                            ApprovalStorageRules::validate_grant(&approval, &intent, &decision, grant)?;
                            if transition.is_some() {
                                return Err(crate::Error::conflict(
                                    "approval decision cannot carry a rejection transition",
                                ));
                            }
                            let sealed = BaseRepository::encode_json(
                                &grant.sealed_token,
                                "pay_approval.sealed_grant_token",
                            )?;
                            let result = sqlx::query(
                                "UPDATE pay_approval SET approved_amount = $1, approved_by = $2, status = 'approved', \
                             grant_jti = $3, grant_claims_hash = $4, sealed_grant_token = $5, expires_at = $6, \
                             approved_at = $7 WHERE id = $8 AND status = 'pending' AND expires_at > $7",
                            )
                            .bind(AmountCodec::encode_amount(grant.claims.amount)?)
                            .bind(&grant.claims.approver)
                            .bind(&grant.claims.jti)
                            .bind(&grant.claims_hash)
                            .bind(sealed)
                            .bind(grant.claims.exp)
                            .bind(decision.decided_at)
                            .bind(&approval.id)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| {
                                crate::Error::conflict(format!("approval grant update rejected: {error}"))
                            })?;
                            if result.rows_affected() != 1 {
                                return Err(crate::Error::conflict(
                                    "approval decision lost its pending-state compare-and-swap",
                                ));
                            }
                            let mut persisted = approval.clone();
                            persisted.approved_amount = Some(grant.claims.amount);
                            persisted.approved_by = Some(grant.claims.approver.clone());
                            persisted.status = ApprovalStatus::Approved;
                            persisted.grant_jti = Some(grant.claims.jti.clone());
                            persisted.grant_claims_hash = Some(grant.claims_hash.clone());
                            persisted.sealed_grant_token = Some(grant.sealed_token.clone());
                            persisted.expires_at = grant.claims.exp;
                            persisted.approved_at = Some(decision.decided_at);
                            persisted
                        }
                        ApprovalDecisionKind::Reject => {
                            if grant.is_some() {
                                return Err(crate::Error::conflict(
                                    "rejected decision cannot carry an approval grant",
                                ));
                            }
                            let (failed, expected) = transition.as_ref().ok_or_else(|| {
                                crate::Error::conflict("rejected decision is missing its intent transition")
                            })?;
                            PaymentRepository::update_intent(
                                connection,
                                failed,
                                *expected,
                                ReservationMutation::Unchanged,
                            )
                            .await?;
                            let result = sqlx::query(
                                "UPDATE pay_approval SET approved_by = $1, status = 'rejected', approved_at = $2 \
                             WHERE id = $3 AND status = 'pending' AND expires_at > $2",
                            )
                            .bind(&decision.approved_by)
                            .bind(decision.decided_at)
                            .bind(&approval.id)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| {
                                crate::Error::conflict(format!("approval rejection update rejected: {error}"))
                            })?;
                            if result.rows_affected() != 1 {
                                return Err(crate::Error::conflict(
                                    "approval rejection lost its pending-state compare-and-swap",
                                ));
                            }
                            let mut persisted = approval.clone();
                            persisted.approved_by = Some(decision.approved_by.clone());
                            persisted.status = ApprovalStatus::Rejected;
                            persisted.approved_at = Some(decision.decided_at);
                            persisted
                        }
                    };
                    EventRepository::insert_event(connection, &event).await?;
                    for message in &outbox {
                        OutboxRepository::insert_outbox(connection, message).await?;
                    }
                    Ok(persisted)
                })
            })
            .await
    }

    async fn consume_and_reserve(
        &self,
        consumption: ApprovalConsumption<'_>,
    ) -> Result<crate::model::PaymentIntent> {
        let grant = consumption.grant.clone();
        let intent = consumption.intent.clone();
        let origin = consumption.guard.origin.to_owned();
        let day_started_at = consumption.guard.day_started_at;
        let evaluated_at = consumption.guard.evaluated_at;
        let expected_updated_at = consumption.expected_intent_updated_at;
        let consumed_at = consumption.consumed_at;
        let event = consumption.event.clone();
        let outbox = consumption.outbox.to_vec();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let row = sqlx::query(concat!("SELECT * FROM pay_approval WHERE id = $1", $update))
                        .bind(&grant.claims.approval_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("load approval grant", error))?
                        .ok_or_else(|| Error::NotFound(format!("payment approval {}", grant.claims.approval_id)))?;
                    let approval = Self::decode_row(&row)?;
                    ApprovalStorageRules::validate_verified_grant(&approval, &intent, &grant, evaluated_at)?;
                    PaymentRepository::validate_current_payment_scope(connection, &intent, Some(&origin))
                        .await?;
                    PaymentRepository::enforce_budget(connection, &intent, day_started_at, evaluated_at, true)
                        .await?;
                    let result = sqlx::query(
                        "UPDATE pay_approval SET status = 'consumed', consumed_at = $1 WHERE id = $2 \
                     AND status = 'approved' AND grant_jti = $3 AND grant_claims_hash = $4 AND expires_at > $5",
                    )
                    .bind(consumed_at)
                    .bind(&approval.id)
                    .bind(&grant.claims.jti)
                    .bind(&grant.claims_hash)
                    .bind(evaluated_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| crate::Error::conflict(format!("approval grant consumption rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict("approval grant was already consumed or expired"));
                    }
                    PaymentRepository::update_intent(
                        connection,
                        &intent,
                        expected_updated_at,
                        ReservationMutation::ApprovalReserve,
                    )
                    .await?;
                    PaymentRepository::append_side_effects(connection, &[event], &outbox).await?;
                    Ok(intent)
                })
            })
            .await
    }

    async fn expire_due(&self, now: i64, limit: u32) -> Result<Vec<ApprovalRequest>> {
        if now < 0 || limit == 0 {
            return Err(Error::InvalidRequest(
                "invalid approval expiry batch".to_owned(),
            ));
        }
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        concat!("SELECT * FROM pay_approval WHERE status IN ('pending', 'approved') AND expires_at <= $1 \
                     ORDER BY expires_at, created_at, id LIMIT $2", $skip),
                    )
                    .bind(now)
                    .bind(i64::from(limit))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("select due approval expirations", error))?;
                    let mut expired = Vec::with_capacity(rows.len());
                    for row in rows {
                        let mut approval = Self::decode_row(&row)?;
                        let result = sqlx::query(
                            "UPDATE pay_approval SET status = 'expired' WHERE id = $1 \
                         AND status IN ('pending', 'approved') AND expires_at <= $2",
                        )
                        .bind(&approval.id)
                        .bind(now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("expire approval", error))?;
                        if result.rows_affected() != 1 {
                            continue;
                        }
                        let intent_row = sqlx::query(concat!("SELECT * FROM pay_intent WHERE id = $1", $update))
                            .bind(&approval.intent_id)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| {
                                BaseRepository::storage_error("load expiring approval intent", error)
                            })?;
                        let mut intent = PaymentRepository::decode_row(&intent_row)?;
                        if intent.reservation_status != ReservationStatus::Unreserved {
                            return Err(Error::Storage(
                                "expiring approval unexpectedly owns a payment reservation".to_owned(),
                            ));
                        }
                        intent.status = PaymentStatus::Expired;
                        intent.updated_at = now.max(intent.updated_at.saturating_add(1));
                        PaymentRepository::update_intent(
                            connection,
                            &intent,
                            PaymentRepository::decode_row(&intent_row)?.updated_at,
                            ReservationMutation::Unchanged,
                        )
                        .await?;
                        let event = ApprovalStorageRules::expiry_event(&approval, &intent, now);
                        EventRepository::insert_event(connection, &event).await?;
                        OutboxRepository::insert_outbox(
                            connection,
                            &ApprovalStorageRules::expiry_outbox(&approval, &intent, now),
                        )
                        .await?;
                        approval.status = ApprovalStatus::Expired;
                        expired.push(approval);
                    }
                    Ok(expired)
                })
            })
            .await
    }
}
impl $repository {
    pub(crate) fn decode_row(row: &DbRow) -> Result<ApprovalRequest> {
        Ok(ApprovalRequest {
            id: BaseRepository::field(row, "id")?,
            intent_id: BaseRepository::field(row, "intent_id")?,
            run_id: BaseRepository::field(row, "run_id")?,
            requested_amount: AmountCodec::decode_amount(
                &BaseRepository::field::<AmountValue>(row, "requested_amount")?,
                "pay_approval.requested_amount",
            )?,
            approved_amount: AmountCodec::decode_optional_amount(
                BaseRepository::field::<Option<AmountValue>>(row, "approved_amount")?.as_ref(),
                "pay_approval.approved_amount",
            )?,
            requested_by: BaseRepository::field(row, "requested_by")?,
            approved_by: BaseRepository::field(row, "approved_by")?,
            status: PaymentStorageCodec::approval_status(&BaseRepository::field::<String>(
                row, "status",
            )?)?,
            intent_hash: BaseRepository::field(row, "intent_hash")?,
            grant_jti: BaseRepository::field(row, "grant_jti")?,
            grant_claims_hash: BaseRepository::field(row, "grant_claims_hash")?,
            sealed_grant_token: BaseRepository::field::<Option<JsonValue>>(
                row,
                "sealed_grant_token",
            )?
            .map(|value| {
                BaseRepository::decode_json::<SealedSensitiveMaterial>(
                    value,
                    "pay_approval.sealed_grant_token",
                )
            })
            .transpose()?,
            expires_at: BaseRepository::field(row, "expires_at")?,
            consumed_at: BaseRepository::field(row, "consumed_at")?,
            created_at: BaseRepository::field(row, "created_at")?,
            approved_at: BaseRepository::field(row, "approved_at")?,
        })
    }
}



};
}
pub(crate) use approval_repository;

#[cfg(test)]
#[path = "../../../tests/storage/approval/transactions_test.rs"]
mod tests;
