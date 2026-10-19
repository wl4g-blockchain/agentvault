//! `SQLite` durable-storage adapter.

mod approval {
    use async_trait::async_trait;
    use serde_json::json;

    use crate::{
        model::{
            ApprovalDecisionKind, ApprovalRequest, ApprovalStatus, PaymentEvent, PaymentEventType, PaymentStatus,
            ReservationStatus,
        },
        storage::{
            ApprovalConsumption, ApprovalStorage, ApprovalWrite, IdempotentWrite, OutboxMessage, OutboxStatus, Page,
            PageRequest,
        },
        Error, Result,
    };

    use super::{
        conflict,
        outbox::insert_outbox,
        payment::{
            append_side_effects, enforce_budget, insert_approval_request, update_intent,
            validate_current_payment_scope, ReservationMutation,
        },
        records, storage_error, SqliteStorage,
    };

    #[async_trait]
    impl ApprovalStorage for SqliteStorage {
        async fn request(&self, approval: &ApprovalRequest) -> Result<IdempotentWrite<ApprovalRequest>> {
            let approval = approval.clone();
            self.immediate(|connection| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT * FROM pay_approval WHERE id = ?1 OR \
                         (intent_id = ?2 AND status IN ('pending', 'approved'))",
                    )
                    .bind(&approval.id)
                    .bind(&approval.intent_id)
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| storage_error("find approval request replay", error))?;
                    if !rows.is_empty() {
                        if rows.len() != 1 {
                            return Err(conflict("approval identifiers collide with multiple rows"));
                        }
                        let existing = records::approval(&rows[0])?;
                        return if existing == approval {
                            Ok(IdempotentWrite::Replay(existing))
                        } else {
                            Err(conflict("approval request identity is bound to different content"))
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
                    insert_approval_request(connection, &approval).await?;
                    Ok(IdempotentWrite::Inserted(approval))
                })
            })
            .await
        }

        async fn get(&self, approval_id: &str) -> Result<Option<ApprovalRequest>> {
            let row = sqlx::query("SELECT * FROM pay_approval WHERE id = ?1")
                .bind(approval_id)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("get approval request", error))?;
            row.as_ref().map(records::approval).transpose()
        }

        async fn get_active_for_intent(&self, intent_id: &str) -> Result<Option<ApprovalRequest>> {
            let row =
                sqlx::query("SELECT * FROM pay_approval WHERE intent_id = ?1 AND status IN ('pending', 'approved')")
                    .bind(intent_id)
                    .fetch_optional(self.pool())
                    .await
                    .map_err(|error| storage_error("get active payment approval", error))?;
            row.as_ref().map(records::approval).transpose()
        }

        async fn list_pending(&self, scenario_id: &str, now: i64, page: PageRequest) -> Result<Page<ApprovalRequest>> {
            let (limit, offset) = super::codec::page_bounds(page)?;
            let total: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pay_approval a JOIN pay_intent i ON i.id = a.intent_id \
                 WHERE i.scenario_id = ?1 AND a.status = 'pending' AND a.expires_at > ?2",
            )
            .bind(scenario_id)
            .bind(now)
            .fetch_one(self.pool())
            .await
            .map_err(|error| storage_error("count pending approvals", error))?;
            let rows = sqlx::query(
                "SELECT a.* FROM pay_approval a JOIN pay_intent i ON i.id = a.intent_id \
                 WHERE i.scenario_id = ?1 AND a.status = 'pending' AND a.expires_at > ?2 \
                 ORDER BY a.created_at, a.id LIMIT ?3 OFFSET ?4",
            )
            .bind(scenario_id)
            .bind(now)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(|error| storage_error("list pending approvals", error))?;
            Ok(Page {
                items: rows.iter().map(records::approval).collect::<Result<Vec<_>>>()?,
                total: u64::try_from(total).map_err(|_| Error::Storage("negative approval count".to_owned()))?,
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
            self.immediate(|connection| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT * FROM pay_approval WHERE id = ?1")
                        .bind(&decision.approval_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| storage_error("load approval decision target", error))?
                        .ok_or_else(|| Error::NotFound(format!("payment approval {}", decision.approval_id)))?;
                    let approval = records::approval(&row)?;
                    approval
                        .validate_decision(&decision, decision.decided_at)
                        .map_err(|reason| conflict(format!("approval decision is invalid: {reason}")))?;
                    let intent_row = sqlx::query("SELECT * FROM pay_intent WHERE id = ?1")
                        .bind(&approval.intent_id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|error| storage_error("load approval payment intent", error))?;
                    let intent = records::intent(&intent_row)?;
                    if approval.intent_hash != intent.request_hash || event.intent_id != intent.id {
                        return Err(conflict("approval no longer matches its immutable payment intent"));
                    }

                    let persisted = match decision.decision {
                        ApprovalDecisionKind::Approve => {
                            let grant = grant
                                .as_ref()
                                .ok_or_else(|| conflict("approval decision is missing its JWT grant"))?;
                            validate_grant(&approval, &intent, &decision, grant)?;
                            if transition.is_some() {
                                return Err(conflict("approval decision cannot carry a rejection transition"));
                            }
                            let sealed =
                                super::codec::encode_json(&grant.sealed_token, "pay_approval.sealed_grant_token")?;
                            let result = sqlx::query(
                                "UPDATE pay_approval SET approved_amount = ?1, approved_by = ?2, status = 'approved', \
                                 grant_jti = ?3, grant_claims_hash = ?4, sealed_grant_token = ?5, expires_at = ?6, \
                                 approved_at = ?7 WHERE id = ?8 AND status = 'pending' AND expires_at > ?7",
                            )
                            .bind(grant.claims.amount.to_string())
                            .bind(&grant.claims.approver)
                            .bind(&grant.claims.jti)
                            .bind(&grant.claims_hash)
                            .bind(sealed)
                            .bind(grant.claims.exp)
                            .bind(decision.decided_at)
                            .bind(&approval.id)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| conflict(format!("approval grant update rejected: {error}")))?;
                            if result.rows_affected() != 1 {
                                return Err(conflict("approval decision lost its pending-state compare-and-swap"));
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
                                return Err(conflict("rejected decision cannot carry an approval grant"));
                            }
                            let (failed, expected) = transition
                                .as_ref()
                                .ok_or_else(|| conflict("rejected decision is missing its intent transition"))?;
                            update_intent(connection, failed, *expected, ReservationMutation::Unchanged).await?;
                            let result = sqlx::query(
                                "UPDATE pay_approval SET approved_by = ?1, status = 'rejected', approved_at = ?2 \
                                 WHERE id = ?3 AND status = 'pending' AND expires_at > ?2",
                            )
                            .bind(&decision.approved_by)
                            .bind(decision.decided_at)
                            .bind(&approval.id)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| conflict(format!("approval rejection update rejected: {error}")))?;
                            if result.rows_affected() != 1 {
                                return Err(conflict("approval rejection lost its pending-state compare-and-swap"));
                            }
                            let mut persisted = approval.clone();
                            persisted.approved_by = Some(decision.approved_by.clone());
                            persisted.status = ApprovalStatus::Rejected;
                            persisted.approved_at = Some(decision.decided_at);
                            persisted
                        }
                    };
                    super::event::insert_event(connection, &event).await?;
                    for message in &outbox {
                        insert_outbox(connection, message).await?;
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
            self.immediate(|connection| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT * FROM pay_approval WHERE id = ?1")
                        .bind(&grant.claims.approval_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| storage_error("load approval grant", error))?
                        .ok_or_else(|| Error::NotFound(format!("payment approval {}", grant.claims.approval_id)))?;
                    let approval = records::approval(&row)?;
                    validate_verified_grant(&approval, &intent, &grant, evaluated_at)?;
                    validate_current_payment_scope(connection, &intent, Some(&origin)).await?;
                    enforce_budget(connection, &intent, day_started_at, evaluated_at, true).await?;
                    let result = sqlx::query(
                        "UPDATE pay_approval SET status = 'consumed', consumed_at = ?1 WHERE id = ?2 \
                         AND status = 'approved' AND grant_jti = ?3 AND grant_claims_hash = ?4 AND expires_at > ?5",
                    )
                    .bind(consumed_at)
                    .bind(&approval.id)
                    .bind(&grant.claims.jti)
                    .bind(&grant.claims_hash)
                    .bind(evaluated_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| conflict(format!("approval grant consumption rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(conflict("approval grant was already consumed or expired"));
                    }
                    update_intent(
                        connection,
                        &intent,
                        expected_updated_at,
                        ReservationMutation::ApprovalReserve,
                    )
                    .await?;
                    append_side_effects(connection, &[event], &outbox).await?;
                    Ok(intent)
                })
            })
            .await
        }

        async fn expire_due(&self, now: i64, limit: u32) -> Result<Vec<ApprovalRequest>> {
            if now < 0 || limit == 0 {
                return Err(Error::InvalidRequest("invalid approval expiry batch".to_owned()));
            }
            self.immediate(|connection| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "SELECT * FROM pay_approval WHERE status IN ('pending', 'approved') AND expires_at <= ?1 \
                         ORDER BY expires_at, created_at, id LIMIT ?2",
                    )
                    .bind(now)
                    .bind(i64::from(limit))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| storage_error("select due approval expirations", error))?;
                    let mut expired = Vec::with_capacity(rows.len());
                    for row in rows {
                        let mut approval = records::approval(&row)?;
                        let result = sqlx::query(
                            "UPDATE pay_approval SET status = 'expired' WHERE id = ?1 \
                             AND status IN ('pending', 'approved') AND expires_at <= ?2",
                        )
                        .bind(&approval.id)
                        .bind(now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("expire approval", error))?;
                        if result.rows_affected() != 1 {
                            continue;
                        }
                        let intent_row = sqlx::query("SELECT * FROM pay_intent WHERE id = ?1")
                            .bind(&approval.intent_id)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| storage_error("load expiring approval intent", error))?;
                        let mut intent = records::intent(&intent_row)?;
                        if intent.reservation_status != ReservationStatus::Unreserved {
                            return Err(Error::Storage(
                                "expiring approval unexpectedly owns a payment reservation".to_owned(),
                            ));
                        }
                        intent.status = PaymentStatus::Expired;
                        intent.updated_at = now.max(intent.updated_at.saturating_add(1));
                        update_intent(
                            connection,
                            &intent,
                            records::intent(&intent_row)?.updated_at,
                            ReservationMutation::Unchanged,
                        )
                        .await?;
                        let event = expiry_event(&approval, &intent, now);
                        super::event::insert_event(connection, &event).await?;
                        insert_outbox(connection, &expiry_outbox(&approval, &intent, now)).await?;
                        approval.status = ApprovalStatus::Expired;
                        expired.push(approval);
                    }
                    Ok(expired)
                })
            })
            .await
        }
    }

    fn validate_grant(
        approval: &ApprovalRequest,
        intent: &crate::model::PaymentIntent,
        decision: &crate::model::ApprovalDecision,
        grant: &crate::model::ApprovalGrant,
    ) -> Result<()> {
        grant
            .claims
            .validate()
            .map_err(|field| conflict(format!("approval JWT claim {field} is invalid")))?;
        if grant.claims.approval_id != approval.id
            || grant.claims.intent_id != approval.intent_id
            || grant.claims.intent_hash != approval.intent_hash
            || grant.claims.intent_hash != intent.request_hash
            || Some(grant.claims.amount) != decision.approved_amount
            || grant.claims.approver != decision.approved_by
            || grant.claims.iat != decision.decided_at
            || grant.claims.exp > approval.expires_at
            || grant.claims_hash.len() != 64
        {
            return Err(conflict("approval JWT is not bound to the decision and intent"));
        }
        Ok(())
    }

    fn validate_verified_grant(
        approval: &ApprovalRequest,
        intent: &crate::model::PaymentIntent,
        grant: &crate::model::VerifiedApprovalGrant,
        now: i64,
    ) -> Result<()> {
        if approval.status != ApprovalStatus::Approved
            || approval.expires_at <= now
            || grant.claims.exp <= now
            || grant.claims.approval_id != approval.id
            || grant.claims.intent_id != intent.id
            || grant.claims.intent_hash != approval.intent_hash
            || grant.claims.intent_hash != intent.request_hash
            || approval.grant_jti.as_deref() != Some(grant.claims.jti.as_str())
            || approval.grant_claims_hash.as_deref() != Some(grant.claims_hash.as_str())
            || approval.approved_amount != Some(grant.claims.amount)
            || approval.approved_by.as_deref() != Some(grant.claims.approver.as_str())
            || approval.requested_amount != intent.amount
            || grant.claims.amount != intent.amount
            || intent.direction != crate::model::PaymentDirection::Outbound
            || intent.status != PaymentStatus::Authorized
            || intent.reservation_status != ReservationStatus::Reserved
            || intent.reserved_amount != intent.amount
        {
            return Err(conflict(
                "verified approval JWT does not match stored authorization state",
            ));
        }
        Ok(())
    }

    fn expiry_event(approval: &ApprovalRequest, intent: &crate::model::PaymentIntent, now: i64) -> PaymentEvent {
        PaymentEvent {
            id: format!("approval-expired:{}", approval.id),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Failed,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"reason": "approval_expired", "approval_id": approval.id}),
            created_at: now,
        }
    }

    fn expiry_outbox(approval: &ApprovalRequest, intent: &crate::model::PaymentIntent, now: i64) -> OutboxMessage {
        OutboxMessage {
            id: format!("approval-expired:{}", approval.id),
            aggregate_type: "payment_intent".to_owned(),
            aggregate_id: intent.id.clone(),
            event_type: "approval_expired".to_owned(),
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({
                "intent_id": intent.id,
                "approval_id": approval.id,
                "scenario_id": intent.scenario_id
            }),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            available_at: now,
            lease_owner: None,
            lease_expires_at: None,
            last_error: None,
            created_at: now,
            delivered_at: None,
        }
    }
}

mod base {
    //! Shared `SQLite` mechanics used by entity repositories.
    //!
    //! SQL remains fixed at the call site; these macros only remove repetitive
    //! bind/executor plumbing and cannot construct identifiers dynamically.

    macro_rules! sqlite_select {
        (optional $executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query($sql);
            $(let query = query.bind($bind);)*
            query.fetch_optional($executor)
        }};
        (one $executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query($sql);
            $(let query = query.bind($bind);)*
            query.fetch_one($executor)
        }};
        (all $executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query($sql);
            $(let query = query.bind($bind);)*
            query.fetch_all($executor)
        }};
    }

    macro_rules! sqlite_scalar {
        (one $executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query_scalar($sql);
            $(let query = query.bind($bind);)*
            query.fetch_one($executor)
        }};
        (all $executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query_scalar($sql);
            $(let query = query.bind($bind);)*
            query.fetch_all($executor)
        }};
    }

    macro_rules! sqlite_insert {
        ($executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query($sql);
            $(let query = query.bind($bind);)*
            query.execute($executor)
        }};
    }

    macro_rules! sqlite_update {
        ($executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query($sql);
            $(let query = query.bind($bind);)*
            query.execute($executor)
        }};
    }

    macro_rules! sqlite_upsert {
        ($executor:expr, $sql:literal $(, $bind:expr)* $(,)?) => {{
            let query = sqlx::query($sql);
            $(let query = query.bind($bind);)*
            query.execute($executor)
        }};
    }

    pub(crate) use {sqlite_insert, sqlite_scalar, sqlite_select, sqlite_update, sqlite_upsert};
}

mod catalog {
    mod approver {
        use async_trait::async_trait;

        use crate::{
            model::PaymentScenarioApprover,
            storage::{ApproverStorage, Page, PageRequest},
            Error, Result,
        };

        use super::super::{
            base::{sqlite_scalar, sqlite_select, sqlite_update, sqlite_upsert},
            codec::encode_bool,
            conflict, records, storage_error, SqliteStorage,
        };

        #[async_trait]
        impl ApproverStorage for SqliteStorage {
            async fn upsert(&self, approver: &PaymentScenarioApprover) -> Result<PaymentScenarioApprover> {
                approver
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid scenario approver {field}")))?;
                let approver = approver.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let existing = sqlite_select!(
                            optional &mut *connection,
                            "SELECT * FROM pay_scenario_approver WHERE scenario_id = ?1 AND auth_principal = ?2",
                            &approver.scenario_id,
                            &approver.auth_principal,
                        )
                        .await
                        .map_err(|error| storage_error("get scenario approver", error))?;
                        if let Some(row) = existing {
                            let existing = records::approver(&row)?;
                            if existing.created_at != approver.created_at || approver.updated_at <= existing.updated_at {
                                return Err(conflict("scenario approver identity or revision changed"));
                            }
                            sqlite_upsert!(
                                &mut *connection,
                                "UPDATE pay_scenario_approver SET enabled = ?1, updated_at = ?2 \
                                 WHERE scenario_id = ?3 AND auth_principal = ?4 AND updated_at = ?5",
                                encode_bool(approver.enabled),
                                approver.updated_at,
                                &approver.scenario_id,
                                &approver.auth_principal,
                                existing.updated_at,
                            )
                            .await
                            .map_err(|error| storage_error("update scenario approver", error))?;
                        } else {
                            sqlite_upsert!(
                                &mut *connection,
                                "INSERT INTO pay_scenario_approver \
                                 (scenario_id, auth_principal, enabled, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                                &approver.scenario_id,
                                &approver.auth_principal,
                                encode_bool(approver.enabled),
                                approver.created_at,
                                approver.updated_at,
                            )
                            .await
                            .map_err(|error| conflict(format!("scenario approver insert rejected: {error}")))?;
                        }
                        Ok(approver)
                    })
                })
                .await
            }

            async fn get(&self, scenario_id: &str, auth_principal: &str) -> Result<Option<PaymentScenarioApprover>> {
                let row = sqlite_select!(
                    optional self.pool(),
                    "SELECT * FROM pay_scenario_approver WHERE scenario_id = ?1 AND auth_principal = ?2",
                    scenario_id,
                    auth_principal,
                )
                .await
                .map_err(|error| storage_error("get scenario approver", error))?;
                row.as_ref().map(records::approver).transpose()
            }

            async fn is_authorized(&self, scenario_id: &str, auth_principal: &str) -> Result<bool> {
                let authorized: i64 = sqlite_scalar!(
                    one self.pool(),
                    "SELECT EXISTS(SELECT 1 FROM pay_scenario_approver a \
                     JOIN pay_scenario s ON s.id = a.scenario_id JOIN pay_client c ON c.id = s.client_id \
                     WHERE a.scenario_id = ?1 AND a.auth_principal = ?2 \
                     AND a.enabled = 1 AND s.enabled = 1 AND c.enabled = 1)",
                    scenario_id,
                    auth_principal,
                )
                .await
                .map_err(|error| storage_error("authorize scenario approver", error))?;
                Ok(authorized == 1)
            }

            async fn set_enabled(
                &self,
                scenario_id: &str,
                auth_principal: &str,
                enabled: bool,
                updated_at: i64,
            ) -> Result<PaymentScenarioApprover> {
                let scenario_id = scenario_id.to_owned();
                let auth_principal = auth_principal.to_owned();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let result = sqlite_update!(
                            &mut *connection,
                            "UPDATE pay_scenario_approver SET enabled = ?1, updated_at = ?2 \
                             WHERE scenario_id = ?3 AND auth_principal = ?4 AND updated_at < ?2",
                            encode_bool(enabled),
                            updated_at,
                            &scenario_id,
                            &auth_principal,
                        )
                        .await
                        .map_err(|error| storage_error("update scenario approver lifecycle", error))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("scenario approver was not found or revision did not advance"));
                        }
                        let row = sqlite_select!(
                            one &mut *connection,
                            "SELECT * FROM pay_scenario_approver WHERE scenario_id = ?1 AND auth_principal = ?2",
                            &scenario_id,
                            &auth_principal,
                        )
                        .await
                        .map_err(|error| storage_error("reload scenario approver", error))?;
                        records::approver(&row)
                    })
                })
                .await
            }

            async fn list_for_scenario(
                &self,
                scenario_id: &str,
                page: PageRequest,
            ) -> Result<Page<PaymentScenarioApprover>> {
                let (limit, offset) = super::super::codec::page_bounds(page)?;
                let total: i64 = sqlite_scalar!(
                    one self.pool(),
                    "SELECT count(*) FROM pay_scenario_approver WHERE scenario_id = ?1",
                    scenario_id,
                )
                .await
                .map_err(|error| storage_error("count scenario approvers", error))?;
                let rows = sqlite_select!(
                    all self.pool(),
                    "SELECT * FROM pay_scenario_approver WHERE scenario_id = ?1 \
                     ORDER BY created_at, auth_principal LIMIT ?2 OFFSET ?3",
                    scenario_id,
                    limit,
                    offset,
                )
                .await
                .map_err(|error| storage_error("list scenario approvers", error))?;
                Ok(Page {
                    items: rows.iter().map(records::approver).collect::<Result<Vec<_>>>()?,
                    total: u64::try_from(total)
                        .map_err(|_| Error::Storage("negative scenario approver count".to_owned()))?,
                })
            }
        }
    }

    mod client {
        use crate::{
            model::PaymentClient,
            storage::{ClientStorage, Page, PageRequest},
            Error, Result,
        };
        use async_trait::async_trait;

        use super::super::{
            base::{sqlite_insert, sqlite_scalar, sqlite_select, sqlite_update},
            codec::encode_bool,
            conflict, records, storage_error, SqliteStorage,
        };

        #[async_trait]
        impl ClientStorage for SqliteStorage {
            async fn insert(&self, client: &PaymentClient) -> Result<PaymentClient> {
                client
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment client {field}")))?;
                let client = client.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        sqlite_insert!(
                            &mut *connection,
                            "INSERT INTO pay_client (id, name, auth_principal, enabled, created_at, updated_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            &client.id,
                            &client.name,
                            &client.auth_principal,
                            encode_bool(client.enabled),
                            client.created_at,
                            client.updated_at,
                        )
                        .await
                        .map_err(|error| conflict(format!("payment client insert rejected: {error}")))?;
                        Ok(client)
                    })
                })
                .await
            }

            async fn get(&self, client_id: &str) -> Result<Option<PaymentClient>> {
                let row = sqlite_select!(optional self.pool(), "SELECT * FROM pay_client WHERE id = ?1", client_id)
                    .await
                    .map_err(|error| storage_error("get payment client", error))?;
                row.as_ref().map(records::client).transpose()
            }

            async fn find_by_principal(&self, auth_principal: &str) -> Result<Option<PaymentClient>> {
                let row = sqlite_select!(
                    optional self.pool(),
                    "SELECT * FROM pay_client WHERE auth_principal = ?1",
                    auth_principal,
                )
                .await
                .map_err(|error| storage_error("find payment client principal", error))?;
                row.as_ref().map(records::client).transpose()
            }

            async fn set_enabled(&self, client_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentClient> {
                let client_id = client_id.to_owned();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let result = sqlite_update!(
                            &mut *connection,
                            "UPDATE pay_client SET enabled = ?1, updated_at = ?2 \
                             WHERE id = ?3 AND updated_at <= ?2",
                            encode_bool(enabled),
                            updated_at,
                            &client_id,
                        )
                        .await
                        .map_err(|error| storage_error("update payment client", error))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict(
                                "payment client was not found or update timestamp moved backwards",
                            ));
                        }
                        let row = sqlite_select!(
                            one &mut *connection,
                            "SELECT * FROM pay_client WHERE id = ?1",
                            &client_id,
                        )
                        .await
                        .map_err(|error| storage_error("reload payment client", error))?;
                        records::client(&row)
                    })
                })
                .await
            }

            async fn list(&self, page: PageRequest) -> Result<Page<PaymentClient>> {
                let (limit, offset) = super::super::codec::page_bounds(page)?;
                let total: i64 = sqlite_scalar!(one self.pool(), "SELECT count(*) FROM pay_client")
                    .await
                    .map_err(|error| storage_error("count payment clients", error))?;
                let rows = sqlite_select!(
                    all self.pool(),
                    "SELECT * FROM pay_client ORDER BY created_at, id LIMIT ?1 OFFSET ?2",
                    limit,
                    offset,
                )
                .await
                .map_err(|error| storage_error("list payment clients", error))?;
                Ok(Page {
                    items: rows.iter().map(records::client).collect::<Result<Vec<_>>>()?,
                    total: u64::try_from(total)
                        .map_err(|_| Error::Storage("negative payment client count".to_owned()))?,
                })
            }
        }
    }

    mod offer {
        use async_trait::async_trait;

        use crate::{
            model::PaymentOffer,
            storage::{OfferStorage, Page, PageRequest},
            Error, Result,
        };

        use super::super::{
            base::{sqlite_insert, sqlite_scalar, sqlite_select, sqlite_update},
            codec::{encode_bool, encode_json, page_bounds},
            conflict, records, storage_error, SqliteStorage,
        };

        #[async_trait]
        impl OfferStorage for SqliteStorage {
            async fn insert(&self, offer: &PaymentOffer) -> Result<PaymentOffer> {
                validate_offer(offer)?;
                let offer = offer.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        validate_offer_authority(connection, &offer).await?;
                        insert_offer(connection, &offer).await?;
                        Ok(offer)
                    })
                })
                .await
            }

            async fn get(&self, offer_id: &str) -> Result<Option<PaymentOffer>> {
                let row = sqlite_select!(optional self.pool(), "SELECT * FROM pay_offer WHERE id = ?1", offer_id)
                    .await
                    .map_err(|error| storage_error("get payment offer", error))?;
                row.as_ref().map(records::offer).transpose()
            }

            async fn resolve(&self, scenario_id: &str, resource: &str, protocol: &str) -> Result<Option<PaymentOffer>> {
                let rows = sqlite_select!(
                    all self.pool(),
                    "SELECT * FROM pay_offer WHERE scenario_id = ?1 AND resource = ?2 AND enabled = 1 \
                     AND EXISTS (SELECT 1 FROM json_each(protocols) WHERE value = ?3)",
                    scenario_id,
                    resource,
                    protocol,
                )
                .await
                .map_err(|error| storage_error("resolve payment offer", error))?;
                match rows.as_slice() {
                    [] => Ok(None),
                    [row] => records::offer(row).map(Some),
                    _ => Err(Error::Storage(
                        "multiple enabled offers own one scenario resource".to_owned(),
                    )),
                }
            }

            async fn resolve_resource(&self, scenario_id: &str, resource: &str) -> Result<Option<PaymentOffer>> {
                let rows = sqlite_select!(
                    all self.pool(),
                    "SELECT * FROM pay_offer WHERE scenario_id = ?1 AND resource = ?2 AND enabled = 1",
                    scenario_id,
                    resource,
                )
                .await
                .map_err(|error| storage_error("resolve payment offer resource", error))?;
                match rows.as_slice() {
                    [] => Ok(None),
                    [row] => records::offer(row).map(Some),
                    _ => Err(Error::Storage(
                        "multiple enabled offers own one scenario resource".to_owned(),
                    )),
                }
            }

            async fn update(&self, offer: &PaymentOffer, expected_updated_at: i64) -> Result<PaymentOffer> {
                validate_offer(offer)?;
                let offer = offer.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        if offer.updated_at <= expected_updated_at {
                            return Err(conflict("payment offer update timestamp did not advance"));
                        }
                        validate_offer_authority(connection, &offer).await?;
                        let protocols = encode_json(&offer.protocols, "pay_offer.protocols")?;
                        let result = sqlite_update!(
                            &mut *connection,
                            "UPDATE pay_offer SET resource = ?1, protocols = ?2, network = ?3, asset = ?4, \
                             token_contract = ?5, asset_decimals = ?6, amount = ?7, wallet_id = ?8, \
                             enabled = ?9, updated_at = ?10 \
                             WHERE id = ?11 AND scenario_id = ?12 AND policy_id = ?13 \
                             AND created_at = ?14 AND updated_at = ?15",
                            &offer.resource,
                            protocols,
                            &offer.asset.network,
                            &offer.asset.asset,
                            &offer.asset.token_contract,
                            i64::from(offer.asset.decimals),
                            offer.amount.to_string(),
                            &offer.wallet_id,
                            encode_bool(offer.enabled),
                            offer.updated_at,
                            &offer.id,
                            &offer.scenario_id,
                            &offer.policy_id,
                            offer.created_at,
                            expected_updated_at,
                        )
                        .await
                        .map_err(|error| conflict(format!("payment offer update rejected: {error}")))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("payment offer identity or revision changed"));
                        }
                        Ok(offer)
                    })
                })
                .await
            }

            async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentOffer>> {
                let (limit, offset) = page_bounds(page)?;
                let total: i64 = sqlite_scalar!(
                    one self.pool(),
                    "SELECT count(*) FROM pay_offer WHERE scenario_id = ?1",
                    scenario_id,
                )
                .await
                .map_err(|error| storage_error("count payment offers", error))?;
                let rows = sqlite_select!(
                    all self.pool(),
                    "SELECT * FROM pay_offer WHERE scenario_id = ?1 ORDER BY created_at, id LIMIT ?2 OFFSET ?3",
                    scenario_id,
                    limit,
                    offset,
                )
                .await
                .map_err(|error| storage_error("list payment offers", error))?;
                Ok(Page {
                    items: rows.iter().map(records::offer).collect::<Result<Vec<_>>>()?,
                    total: u64::try_from(total)
                        .map_err(|_| Error::Storage("negative payment offer count".to_owned()))?,
                })
            }
        }

        fn validate_offer(offer: &PaymentOffer) -> Result<()> {
            offer
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid payment offer {field}")))
        }

        async fn validate_offer_authority(connection: &mut sqlx::SqliteConnection, offer: &PaymentOffer) -> Result<()> {
            let wallet: Option<String> = sqlx::query_scalar("SELECT id FROM pay_wallet WHERE id = ?1")
                .bind(&offer.wallet_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| storage_error("load offer payment wallet", error))?;
            if wallet.is_none() {
                return Err(Error::NotFound(format!("payment wallet {}", offer.wallet_id)));
            }
            let policy = super::policy::load_policy_connection(connection, &offer.policy_id)
                .await?
                .ok_or_else(|| Error::NotFound(format!("payment policy {}", offer.policy_id)))?;
            let inbound = policy
                .inbound_limits
                .as_ref()
                .ok_or_else(|| conflict("offer policy does not authorize inbound payments"))?;
            if policy.scenario_id != offer.scenario_id
                || !policy.wallet_ids.contains(&offer.wallet_id)
                || !policy.budget_unit.matches(&offer.asset)
                || !policy.networks.contains(&offer.asset.network)
                || !policy.assets.contains(&offer.asset)
                || offer.amount > inbound.max_per_payment
                || offer
                    .protocols
                    .iter()
                    .any(|protocol| !policy.protocols.contains(protocol))
            {
                return Err(conflict("offer exceeds or falls outside its inbound policy scope"));
            }
            Ok(())
        }

        async fn insert_offer(connection: &mut sqlx::SqliteConnection, offer: &PaymentOffer) -> Result<()> {
            let protocols = encode_json(&offer.protocols, "pay_offer.protocols")?;
            sqlite_insert!(
                connection,
                "INSERT INTO pay_offer (id, scenario_id, policy_id, resource, protocols, network, asset, token_contract, \
                 asset_decimals, amount, wallet_id, enabled, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                &offer.id,
                &offer.scenario_id,
                &offer.policy_id,
                &offer.resource,
                protocols,
                &offer.asset.network,
                &offer.asset.asset,
                &offer.asset.token_contract,
                i64::from(offer.asset.decimals),
                offer.amount.to_string(),
                &offer.wallet_id,
                encode_bool(offer.enabled),
                offer.created_at,
                offer.updated_at,
            )
            .await
            .map_err(|error| conflict(format!("payment offer insert rejected: {error}")))?;
            Ok(())
        }
    }

    mod policy {
        use async_trait::async_trait;
        use sqlx::{Row as _, SqliteConnection, SqlitePool};

        use crate::{
            model::{PaymentPolicy, PaymentPolicyLimits},
            storage::{Page, PageRequest, PolicyStorage},
            Error, Result,
        };

        use super::super::{
            base::sqlite_scalar,
            codec::{encode_bool, encode_json, page_bounds},
            conflict, records, storage_error, SqliteStorage,
        };

        #[async_trait]
        impl PolicyStorage for SqliteStorage {
            async fn insert(&self, policy: &PaymentPolicy) -> Result<PaymentPolicy> {
                validate_policy(policy)?;
                let policy = policy.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        insert_policy_aggregate(connection, &policy).await?;
                        Ok(policy)
                    })
                })
                .await
            }

            async fn get(&self, policy_id: &str) -> Result<Option<PaymentPolicy>> {
                load_policy_pool(self.pool(), policy_id).await
            }

            async fn update(&self, policy: &PaymentPolicy, expected_updated_at: i64) -> Result<PaymentPolicy> {
                validate_policy(policy)?;
                let policy = policy.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let existing = load_policy_connection(connection, &policy.id)
                            .await?
                            .ok_or_else(|| Error::NotFound(format!("payment policy {}", policy.id)))?;
                        if existing.scenario_id != policy.scenario_id
                            || existing.wallet_ids != policy.wallet_ids
                            || existing.budget_unit != policy.budget_unit
                            || existing.created_at != policy.created_at
                            || existing.updated_at != expected_updated_at
                            || policy.updated_at <= expected_updated_at
                        {
                            return Err(conflict("payment policy identity or revision changed"));
                        }
                        validate_wallet_bindings(connection, &policy).await?;
                        let encoded = EncodedPolicy::new(&policy)?;
                        let result = sqlx::query(
                            "UPDATE pay_policy SET protocols = ?1, networks = ?2, assets = ?3, \
                             allowed_recipients = ?4, allowed_origins = ?5, \
                             inbound_max_per_payment = ?6, inbound_max_per_day = ?7, \
                             inbound_approval_threshold = ?8, inbound_approval_enabled = ?9, \
                             inbound_approval_overage = ?10, outbound_max_per_payment = ?11, \
                             outbound_max_per_day = ?12, outbound_approval_threshold = ?13, \
                             outbound_approval_enabled = ?14, outbound_approval_overage = ?15, \
                             enabled = ?16, updated_at = ?17 WHERE id = ?18 AND updated_at = ?19",
                        )
                        .bind(encoded.protocols)
                        .bind(encoded.networks)
                        .bind(encoded.assets)
                        .bind(encoded.allowed_recipients)
                        .bind(encoded.allowed_origins)
                        .bind(encoded.inbound.max_per_payment)
                        .bind(encoded.inbound.max_per_day)
                        .bind(encoded.inbound.approval_threshold)
                        .bind(encoded.inbound.approval_enabled)
                        .bind(encoded.inbound.approval_overage)
                        .bind(encoded.outbound.max_per_payment)
                        .bind(encoded.outbound.max_per_day)
                        .bind(encoded.outbound.approval_threshold)
                        .bind(encoded.outbound.approval_enabled)
                        .bind(encoded.outbound.approval_overage)
                        .bind(encode_bool(policy.enabled))
                        .bind(policy.updated_at)
                        .bind(&policy.id)
                        .bind(expected_updated_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| conflict(format!("payment policy update rejected: {error}")))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("payment policy revision changed"));
                        }
                        Ok(policy)
                    })
                })
                .await
            }

            async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>> {
                list_policies(
                    self,
                    "p.scenario_id = ?1",
                    scenario_id,
                    "SELECT count(*) FROM pay_policy p WHERE p.scenario_id = ?1",
                    page,
                )
                .await
            }

            async fn list_for_wallet(&self, wallet_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>> {
                list_policies(
                    self,
                    "EXISTS (SELECT 1 FROM pay_policy_wallet m WHERE m.policy_id = p.id AND m.wallet_id = ?1)",
                    wallet_id,
                    "SELECT count(*) FROM pay_policy p WHERE EXISTS \
                     (SELECT 1 FROM pay_policy_wallet m WHERE m.policy_id = p.id AND m.wallet_id = ?1)",
                    page,
                )
                .await
            }

            async fn has_enabled_wallet_binding(&self, scenario_id: &str, wallet_id: &str) -> Result<bool> {
                let present: i64 = sqlite_scalar!(
                    one self.pool(),
                    "SELECT EXISTS (SELECT 1 FROM pay_policy p JOIN pay_policy_wallet m ON m.policy_id = p.id \
                     WHERE p.scenario_id = ?1 AND m.wallet_id = ?2 AND p.enabled = 1)",
                    scenario_id,
                    wallet_id,
                )
                .await
                .map_err(|error| storage_error("check scenario wallet policy binding", error))?;
                match present {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(Error::Storage("invalid scenario wallet binding result".to_owned())),
                }
            }
        }

        pub(in crate::storage::sqlite) fn validate_policy(policy: &PaymentPolicy) -> Result<()> {
            policy
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid payment policy {field}")))
        }

        struct EncodedLimits {
            max_per_payment: Option<String>,
            max_per_day: Option<String>,
            approval_threshold: Option<String>,
            approval_enabled: i64,
            approval_overage: Option<String>,
        }

        impl EncodedLimits {
            fn new(limits: Option<&PaymentPolicyLimits>) -> Self {
                limits.map_or(
                    Self {
                        max_per_payment: None,
                        max_per_day: None,
                        approval_threshold: None,
                        approval_enabled: 1,
                        approval_overage: None,
                    },
                    |limits| Self {
                        max_per_payment: Some(limits.max_per_payment.to_string()),
                        max_per_day: Some(limits.max_per_day.to_string()),
                        approval_threshold: limits.approval_threshold.map(|amount| amount.to_string()),
                        approval_enabled: encode_bool(limits.approval_enabled),
                        approval_overage: limits.approval_overage.map(|amount| amount.to_string()),
                    },
                )
            }
        }

        struct EncodedPolicy {
            protocols: String,
            networks: String,
            assets: String,
            allowed_recipients: String,
            allowed_origins: String,
            inbound: EncodedLimits,
            outbound: EncodedLimits,
        }

        impl EncodedPolicy {
            fn new(policy: &PaymentPolicy) -> Result<Self> {
                Ok(Self {
                    protocols: encode_json(&policy.protocols, "pay_policy.protocols")?,
                    networks: encode_json(&policy.networks, "pay_policy.networks")?,
                    assets: encode_json(&policy.assets, "pay_policy.assets")?,
                    allowed_recipients: encode_json(&policy.allowed_recipients, "pay_policy.allowed_recipients")?,
                    allowed_origins: encode_json(&policy.allowed_origins, "pay_policy.allowed_origins")?,
                    inbound: EncodedLimits::new(policy.inbound_limits.as_ref()),
                    outbound: EncodedLimits::new(policy.outbound_limits.as_ref()),
                })
            }
        }

        async fn insert_policy(connection: &mut SqliteConnection, policy: &PaymentPolicy) -> Result<()> {
            let encoded = EncodedPolicy::new(policy)?;
            sqlx::query(
                "INSERT INTO pay_policy (id, scenario_id, protocols, networks, assets, budget_asset, budget_decimals, \
                 allowed_recipients, allowed_origins, inbound_max_per_payment, inbound_max_per_day, \
                 inbound_approval_threshold, inbound_approval_enabled, inbound_approval_overage, \
                 outbound_max_per_payment, outbound_max_per_day, outbound_approval_threshold, \
                 outbound_approval_enabled, outbound_approval_overage, enabled, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            )
            .bind(&policy.id)
            .bind(&policy.scenario_id)
            .bind(encoded.protocols)
            .bind(encoded.networks)
            .bind(encoded.assets)
            .bind(&policy.budget_unit.asset)
            .bind(i64::from(policy.budget_unit.decimals))
            .bind(encoded.allowed_recipients)
            .bind(encoded.allowed_origins)
            .bind(encoded.inbound.max_per_payment)
            .bind(encoded.inbound.max_per_day)
            .bind(encoded.inbound.approval_threshold)
            .bind(encoded.inbound.approval_enabled)
            .bind(encoded.inbound.approval_overage)
            .bind(encoded.outbound.max_per_payment)
            .bind(encoded.outbound.max_per_day)
            .bind(encoded.outbound.approval_threshold)
            .bind(encoded.outbound.approval_enabled)
            .bind(encoded.outbound.approval_overage)
            .bind(encode_bool(policy.enabled))
            .bind(policy.created_at)
            .bind(policy.updated_at)
            .execute(connection)
            .await
            .map_err(|error| conflict(format!("payment policy insert rejected: {error}")))?;
            Ok(())
        }

        pub(in crate::storage::sqlite) async fn insert_policy_aggregate(
            connection: &mut SqliteConnection,
            policy: &PaymentPolicy,
        ) -> Result<()> {
            insert_policy(connection, policy).await?;
            validate_wallet_bindings(connection, policy).await?;
            for wallet_id in &policy.wallet_ids {
                sqlx::query("INSERT INTO pay_policy_wallet (policy_id, wallet_id, created_at) VALUES (?1, ?2, ?3)")
                    .bind(&policy.id)
                    .bind(wallet_id)
                    .bind(policy.created_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| conflict(format!("payment policy wallet binding rejected: {error}")))?;
            }
            Ok(())
        }

        /// `SQLite` obtains a database-wide writer reservation before this
        /// helper runs, providing the same deterministic mutation boundary as
        /// `PostgreSQL`'s ordered wallet row locks.
        async fn validate_wallet_bindings(connection: &mut SqliteConnection, policy: &PaymentPolicy) -> Result<()> {
            for wallet_id in &policy.wallet_ids {
                let row = sqlx::query(
                    "SELECT budget_asset, budget_decimals, max_per_payment IS NOT NULL AS has_payment_limit, \
                     max_per_day IS NOT NULL AS has_day_limit FROM pay_wallet WHERE id = ?1",
                )
                .bind(wallet_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| storage_error("load payment policy wallet", error))?
                .ok_or_else(|| conflict("payment policy references an unavailable wallet"))?;
                let budget_asset: Option<String> = row
                    .try_get("budget_asset")
                    .map_err(|error| storage_error("decode wallet budget asset", error))?;
                let budget_decimals: Option<i64> = row
                    .try_get("budget_decimals")
                    .map_err(|error| storage_error("decode wallet budget decimals", error))?;
                let has_hard_limits = row
                    .try_get::<i64, _>("has_payment_limit")
                    .map_err(|error| storage_error("decode wallet payment limit", error))?
                    == 1
                    && row
                        .try_get::<i64, _>("has_day_limit")
                        .map_err(|error| storage_error("decode wallet daily limit", error))?
                        == 1;

                match (budget_asset.as_deref(), budget_decimals) {
                    (None, None) => {
                        let changed = sqlx::query(
                            "UPDATE pay_wallet SET budget_asset = ?1, budget_decimals = ?2, \
                             updated_at = max(updated_at + 1, ?3) \
                             WHERE id = ?4 AND budget_asset IS NULL AND budget_decimals IS NULL",
                        )
                        .bind(&policy.budget_unit.asset)
                        .bind(i64::from(policy.budget_unit.decimals))
                        .bind(policy.created_at)
                        .bind(wallet_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| conflict(format!("initialize wallet budget unit rejected: {error}")))?;
                        if changed.rows_affected() != 1 {
                            return Err(conflict("wallet budget unit changed while binding policy"));
                        }
                    }
                    (Some(asset), Some(decimals))
                        if asset == policy.budget_unit.asset.as_str()
                            && decimals == i64::from(policy.budget_unit.decimals) => {}
                    _ => return Err(conflict("payment policy budget unit does not match its wallet")),
                }

                if !policy.enabled {
                    continue;
                }
                let duplicate: i64 = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pay_policy_wallet membership \
                     JOIN pay_policy other ON other.id = membership.policy_id \
                     WHERE membership.wallet_id = ?1 AND other.enabled = 1 \
                     AND other.scenario_id = ?2 AND other.id <> ?3)",
                )
                .bind(wallet_id)
                .bind(&policy.scenario_id)
                .bind(&policy.id)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| storage_error("validate unique scenario wallet policy", error))?;
                if duplicate == 1 {
                    return Err(conflict("scenario wallet already has an enabled payment policy"));
                }

                let shared: i64 = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pay_policy_wallet membership \
                     JOIN pay_policy other ON other.id = membership.policy_id \
                     WHERE membership.wallet_id = ?1 AND other.enabled = 1 \
                     AND other.scenario_id <> ?2 AND other.id <> ?3)",
                )
                .bind(wallet_id)
                .bind(&policy.scenario_id)
                .bind(&policy.id)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| storage_error("validate shared payment wallet limits", error))?;
                if shared == 1 && !has_hard_limits {
                    return Err(conflict(
                        "wallet shared across scenarios requires aggregate hard limits",
                    ));
                }
            }
            Ok(())
        }

        async fn load_policy_pool(pool: &SqlitePool, policy_id: &str) -> Result<Option<PaymentPolicy>> {
            let row = sqlx::query("SELECT * FROM pay_policy WHERE id = ?1")
                .bind(policy_id)
                .fetch_optional(pool)
                .await
                .map_err(|error| storage_error("get payment policy", error))?;
            let Some(row) = row else { return Ok(None) };
            let wallet_ids =
                sqlx::query_scalar("SELECT wallet_id FROM pay_policy_wallet WHERE policy_id = ?1 ORDER BY wallet_id")
                    .bind(policy_id)
                    .fetch_all(pool)
                    .await
                    .map_err(|error| storage_error("load payment policy wallets", error))?;
            records::policy(&row, wallet_ids).map(Some)
        }

        pub(in crate::storage::sqlite) async fn load_policy_connection(
            connection: &mut SqliteConnection,
            policy_id: &str,
        ) -> Result<Option<PaymentPolicy>> {
            let row = sqlx::query("SELECT * FROM pay_policy WHERE id = ?1")
                .bind(policy_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| storage_error("get payment policy", error))?;
            let Some(row) = row else { return Ok(None) };
            let wallet_ids =
                sqlx::query_scalar("SELECT wallet_id FROM pay_policy_wallet WHERE policy_id = ?1 ORDER BY wallet_id")
                    .bind(policy_id)
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| storage_error("load payment policy wallets", error))?;
            records::policy(&row, wallet_ids).map(Some)
        }

        async fn list_policies(
            storage: &SqliteStorage,
            predicate: &str,
            value: &str,
            count_sql: &str,
            page: PageRequest,
        ) -> Result<Page<PaymentPolicy>> {
            let (limit, offset) = page_bounds(page)?;
            let total: i64 = sqlx::query_scalar(count_sql)
                .bind(value)
                .fetch_one(storage.pool())
                .await
                .map_err(|error| storage_error("count payment policies", error))?;
            let query = format!(
                "SELECT p.* FROM pay_policy p WHERE {predicate} ORDER BY p.created_at, p.id LIMIT ?2 OFFSET ?3"
            );
            let rows = sqlx::query(&query)
                .bind(value)
                .bind(limit)
                .bind(offset)
                .fetch_all(storage.pool())
                .await
                .map_err(|error| storage_error("list payment policies", error))?;
            let mut items = Vec::with_capacity(rows.len());
            for row in rows {
                use sqlx::Row as _;
                let policy_id: String = row
                    .try_get("id")
                    .map_err(|error| storage_error("decode payment policy id", error))?;
                let wallet_ids = sqlx::query_scalar(
                    "SELECT wallet_id FROM pay_policy_wallet WHERE policy_id = ?1 ORDER BY wallet_id",
                )
                .bind(&policy_id)
                .fetch_all(storage.pool())
                .await
                .map_err(|error| storage_error("load payment policy wallets", error))?;
                items.push(records::policy(&row, wallet_ids)?);
            }
            Ok(Page {
                items,
                total: u64::try_from(total).map_err(|_| Error::Storage("negative payment policy count".to_owned()))?,
            })
        }
    }

    mod scenario {
        use async_trait::async_trait;

        use crate::{
            model::{PaymentPolicy, PaymentScenario},
            storage::{IdempotentWrite, Page, PageRequest, ScenarioConfiguration, ScenarioStorage},
            Error, Result,
        };

        use super::super::{
            base::{sqlite_insert, sqlite_scalar, sqlite_select, sqlite_update},
            codec::encode_bool,
            conflict, records, storage_error, SqliteStorage,
        };

        #[async_trait]
        impl ScenarioStorage for SqliteStorage {
            async fn register(&self, scenario: &PaymentScenario) -> Result<IdempotentWrite<PaymentScenario>> {
                scenario
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment scenario {field}")))?;
                let scenario = scenario.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let existing = sqlite_select!(
                            optional &mut *connection,
                            "SELECT * FROM pay_scenario WHERE id = ?1",
                            &scenario.id,
                        )
                        .await
                        .map_err(|error| storage_error("find registered payment scenario", error))?;
                        if let Some(row) = existing {
                            let existing = records::scenario(&row)?;
                            return if existing == scenario {
                                Ok(IdempotentWrite::Replay(existing))
                            } else {
                                Err(conflict(
                                    "scenario registration key is already bound to different content",
                                ))
                            };
                        }

                        sqlite_insert!(
                            &mut *connection,
                            "INSERT INTO pay_scenario \
                             (id, client_id, name, enabled, created_at, updated_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            &scenario.id,
                            &scenario.client_id,
                            &scenario.name,
                            encode_bool(scenario.enabled),
                            scenario.created_at,
                            scenario.updated_at,
                        )
                        .await
                        .map_err(|error| conflict(format!("payment scenario insert rejected: {error}")))?;
                        Ok(IdempotentWrite::Inserted(scenario))
                    })
                })
                .await
            }

            async fn register_with_policy(
                &self,
                scenario: &PaymentScenario,
                policy: &PaymentPolicy,
            ) -> Result<IdempotentWrite<ScenarioConfiguration>> {
                scenario
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment scenario {field}")))?;
                super::policy::validate_policy(policy)?;
                if scenario.id != policy.scenario_id || scenario.created_at != policy.created_at {
                    return Err(Error::InvalidRequest(
                        "initial policy must belong to and be created with its scenario".to_owned(),
                    ));
                }
                let scenario = scenario.clone();
                let policy = policy.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let existing = sqlite_select!(
                            optional &mut *connection,
                            "SELECT * FROM pay_scenario WHERE id = ?1",
                            &scenario.id,
                        )
                        .await
                        .map_err(|error| storage_error("find registered payment scenario", error))?;
                        let scenario_inserted = existing.is_none();
                        let registered_scenario = if let Some(row) = existing {
                            records::scenario(&row)?
                        } else {
                            sqlite_insert!(
                                &mut *connection,
                                "INSERT INTO pay_scenario \
                                 (id, client_id, name, enabled, created_at, updated_at) \
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                                &scenario.id,
                                &scenario.client_id,
                                &scenario.name,
                                encode_bool(scenario.enabled),
                                scenario.created_at,
                                scenario.updated_at,
                            )
                            .await
                            .map_err(|error| conflict(format!("payment scenario insert rejected: {error}")))?;
                            scenario.clone()
                        };
                        if !same_registration(&registered_scenario, &scenario) {
                            return Err(conflict(
                                "scenario registration key is already bound to different content",
                            ));
                        }

                        if let Some(existing) = super::policy::load_policy_connection(connection, &policy.id).await? {
                            if !same_policy_registration(&existing, &policy) {
                                return Err(conflict("initial payment policy is already bound to different content"));
                            }
                            return Ok(IdempotentWrite::Replay(ScenarioConfiguration {
                                scenario: registered_scenario,
                                policy: existing,
                            }));
                        }
                        if !scenario_inserted {
                            return Err(conflict(
                                "scenario already exists without the requested atomic initial policy",
                            ));
                        }
                        super::policy::insert_policy_aggregate(connection, &policy).await?;
                        Ok(IdempotentWrite::Inserted(ScenarioConfiguration {
                            scenario: registered_scenario,
                            policy,
                        }))
                    })
                })
                .await
            }

            async fn get(&self, scenario_id: &str) -> Result<Option<PaymentScenario>> {
                let row = sqlite_select!(optional self.pool(), "SELECT * FROM pay_scenario WHERE id = ?1", scenario_id)
                    .await
                    .map_err(|error| storage_error("get payment scenario", error))?;
                row.as_ref().map(records::scenario).transpose()
            }

            async fn get_for_client(&self, client_id: &str, scenario_id: &str) -> Result<Option<PaymentScenario>> {
                let row = sqlite_select!(
                    optional self.pool(),
                    "SELECT * FROM pay_scenario WHERE id = ?1 AND client_id = ?2",
                    scenario_id,
                    client_id,
                )
                .await
                .map_err(|error| storage_error("get client payment scenario", error))?;
                row.as_ref().map(records::scenario).transpose()
            }

            async fn set_enabled(&self, scenario_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentScenario> {
                let scenario_id = scenario_id.to_owned();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let result = sqlite_update!(
                            &mut *connection,
                            "UPDATE pay_scenario SET enabled = ?1, updated_at = ?2 \
                             WHERE id = ?3 AND updated_at <= ?2",
                            encode_bool(enabled),
                            updated_at,
                            &scenario_id,
                        )
                        .await
                        .map_err(|error| storage_error("update payment scenario", error))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict(
                                "payment scenario was not found or update timestamp moved backwards",
                            ));
                        }
                        let row = sqlite_select!(
                            one &mut *connection,
                            "SELECT * FROM pay_scenario WHERE id = ?1",
                            &scenario_id,
                        )
                        .await
                        .map_err(|error| storage_error("reload payment scenario", error))?;
                        records::scenario(&row)
                    })
                })
                .await
            }

            async fn list_for_client(&self, client_id: &str, page: PageRequest) -> Result<Page<PaymentScenario>> {
                let (limit, offset) = super::super::codec::page_bounds(page)?;
                let total: i64 = sqlite_scalar!(
                    one self.pool(),
                    "SELECT count(*) FROM pay_scenario WHERE client_id = ?1",
                    client_id,
                )
                .await
                .map_err(|error| storage_error("count payment scenarios", error))?;
                let rows = sqlite_select!(
                    all self.pool(),
                    "SELECT * FROM pay_scenario WHERE client_id = ?1 ORDER BY created_at, id LIMIT ?2 OFFSET ?3",
                    client_id,
                    limit,
                    offset,
                )
                .await
                .map_err(|error| storage_error("list payment scenarios", error))?;
                Ok(Page {
                    items: rows.iter().map(records::scenario).collect::<Result<Vec<_>>>()?,
                    total: u64::try_from(total)
                        .map_err(|_| Error::Storage("negative payment scenario count".to_owned()))?,
                })
            }
        }

        fn same_registration(stored: &PaymentScenario, requested: &PaymentScenario) -> bool {
            stored.id == requested.id
                && stored.client_id == requested.client_id
                && stored.name == requested.name
                && stored.enabled == requested.enabled
        }

        fn same_policy_registration(stored: &PaymentPolicy, requested: &PaymentPolicy) -> bool {
            stored.id == requested.id
                && stored.scenario_id == requested.scenario_id
                && stored.wallet_ids == requested.wallet_ids
                && stored.protocols == requested.protocols
                && stored.networks == requested.networks
                && stored.assets == requested.assets
                && stored.budget_unit == requested.budget_unit
                && stored.allowed_recipients == requested.allowed_recipients
                && stored.allowed_origins == requested.allowed_origins
                && stored.inbound_limits == requested.inbound_limits
                && stored.outbound_limits == requested.outbound_limits
                && stored.enabled == requested.enabled
        }
    }

    mod wallet {
        use async_trait::async_trait;
        use sqlx::Row as _;

        use crate::{
            model::{wallet::Wallet, Amount, AssetIdentity, BudgetUnit},
            storage::{
                EncryptedWalletKeyVersion, Page, PageRequest, PaymentWallet, WalletBalanceRecord, WalletBalanceStorage,
                WalletCustody, WalletKeyStatus, WalletProvisioningConfig, WalletStorage,
            },
            Error, Result,
        };

        use super::super::{
            codec::{decode_amount, encode_bool},
            conflict, records, storage_error, SqliteStorage,
        };

        const PAYMENT_WALLET_SELECT: &str = "SELECT w.*, active.version AS active_key_version FROM pay_wallet w \
             LEFT JOIN wallet_key_version active ON active.wallet_id = w.id AND active.status = 'active'";

        pub(in crate::storage::sqlite) async fn load_payment_wallet_connection(
            connection: &mut sqlx::SqliteConnection,
            wallet_id: &str,
        ) -> Result<Option<PaymentWallet>> {
            let query = format!("{PAYMENT_WALLET_SELECT} WHERE w.id = ?1");
            let row = sqlx::query(&query)
                .bind(wallet_id)
                .fetch_optional(connection)
                .await
                .map_err(|error| storage_error("get payment wallet", error))?;
            row.as_ref()
                .map(records::payment_wallet)
                .transpose()?
                .map(PaymentWallet::canonicalized)
                .transpose()
                .map_err(|field| Error::Storage(format!("stored payment wallet has invalid {field}")))
        }

        #[async_trait]
        impl WalletStorage for SqliteStorage {
            async fn create_local(&self, wallet: &Wallet, key: &EncryptedWalletKeyVersion) -> Result<Wallet> {
                self.create_local_configured(
                    wallet,
                    key,
                    &WalletProvisioningConfig {
                        enabled: true,
                        ..WalletProvisioningConfig::default()
                    },
                )
                .await
            }

            async fn create_local_configured(
                &self,
                wallet: &Wallet,
                key: &EncryptedWalletKeyVersion,
                config: &WalletProvisioningConfig,
            ) -> Result<Wallet> {
                let wallet = canonical_wallet(wallet)?;
                validate_key_for_wallet(&wallet, key, true)?;
                validate_provisioning_config(config)?;
                if key.version != 1 {
                    return Err(Error::InvalidKey("initial wallet key version must be 1".to_owned()));
                }
                let key = key.clone();
                let config = config.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        insert_wallet(connection, &wallet, WalletCustody::Local, &config).await?;
                        insert_key(connection, &key).await?;
                        Ok(wallet)
                    })
                })
                .await
            }

            async fn create_external(&self, wallet: &Wallet) -> Result<Wallet> {
                self.create_external_configured(
                    wallet,
                    &WalletProvisioningConfig {
                        enabled: true,
                        ..WalletProvisioningConfig::default()
                    },
                )
                .await
            }

            async fn create_external_configured(
                &self,
                wallet: &Wallet,
                config: &WalletProvisioningConfig,
            ) -> Result<Wallet> {
                let wallet = canonical_wallet(wallet)?;
                validate_provisioning_config(config)?;
                let wallet_for_write = wallet.clone();
                let config = config.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        insert_wallet(connection, &wallet_for_write, WalletCustody::External, &config).await?;
                        Ok(wallet_for_write)
                    })
                })
                .await
            }

            async fn get(&self, wallet_id: &str) -> Result<Option<Wallet>> {
                Ok(self.get_payment_wallet(wallet_id).await?.map(|payment| payment.wallet))
            }

            async fn get_payment_wallet(&self, wallet_id: &str) -> Result<Option<PaymentWallet>> {
                let query = format!("{PAYMENT_WALLET_SELECT} WHERE w.id = ?1");
                let row = sqlx::query(&query)
                    .bind(wallet_id)
                    .fetch_optional(self.pool())
                    .await
                    .map_err(|error| storage_error("get payment wallet", error))?;
                row.as_ref()
                    .map(records::payment_wallet)
                    .transpose()?
                    .map(PaymentWallet::canonicalized)
                    .transpose()
                    .map_err(|field| Error::Storage(format!("stored payment wallet has invalid {field}")))
            }

            async fn list(&self, page: PageRequest) -> Result<Page<Wallet>> {
                let (limit, offset) = super::super::codec::page_bounds(page)?;
                let total: i64 = sqlx::query_scalar("SELECT count(*) FROM pay_wallet")
                    .fetch_one(self.pool())
                    .await
                    .map_err(|error| storage_error("count payment wallets", error))?;
                let query = format!("{PAYMENT_WALLET_SELECT} ORDER BY w.created_at, w.id LIMIT ?1 OFFSET ?2");
                let rows = sqlx::query(&query)
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(self.pool())
                    .await
                    .map_err(|error| storage_error("list payment wallets", error))?;
                let mut items = Vec::with_capacity(rows.len());
                for row in &rows {
                    items.push(
                        records::payment_wallet(row)?
                            .canonicalized()
                            .map_err(|field| Error::Storage(format!("stored payment wallet has invalid {field}")))?
                            .wallet,
                    );
                }
                Ok(Page {
                    items,
                    total: u64::try_from(total)
                        .map_err(|_| Error::Storage("negative payment wallet count".to_owned()))?,
                })
            }

            async fn load_active_key(&self, wallet_id: &str) -> Result<Option<EncryptedWalletKeyVersion>> {
                let row = sqlx::query("SELECT * FROM wallet_key_version WHERE wallet_id = ?1 AND status = 'active'")
                    .bind(wallet_id)
                    .fetch_optional(self.pool())
                    .await
                    .map_err(|error| storage_error("load active wallet key", error))?;
                row.as_ref().map(records::encrypted_key).transpose()
            }

            async fn list_key_versions(&self, wallet_id: &str) -> Result<Vec<EncryptedWalletKeyVersion>> {
                let rows = sqlx::query("SELECT * FROM wallet_key_version WHERE wallet_id = ?1 ORDER BY version")
                    .bind(wallet_id)
                    .fetch_all(self.pool())
                    .await
                    .map_err(|error| storage_error("list wallet key versions", error))?;
                rows.iter().map(records::encrypted_key).collect()
            }

            async fn rotate(
                &self,
                wallet: &Wallet,
                key: &EncryptedWalletKeyVersion,
                retired_at: i64,
            ) -> Result<Wallet> {
                let wallet = canonical_wallet(wallet)?;
                validate_key_for_wallet(&wallet, key, true)?;
                let key = key.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let row = sqlx::query(
                            "SELECT w.custody_type, w.created_at, active.version AS active_version \
                             FROM pay_wallet w LEFT JOIN wallet_key_version active \
                             ON active.wallet_id = w.id AND active.status = 'active' WHERE w.id = ?1",
                        )
                        .bind(&wallet.id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| storage_error("load wallet rotation state", error))?
                        .ok_or_else(|| Error::NotFound(format!("payment wallet {}", wallet.id)))?;
                        let custody: String = row
                            .try_get("custody_type")
                            .map_err(|error| storage_error("decode wallet custody", error))?;
                        let created_at: i64 = row
                            .try_get("created_at")
                            .map_err(|error| storage_error("decode wallet creation time", error))?;
                        let active_version: Option<i64> = row
                            .try_get("active_version")
                            .map_err(|error| storage_error("decode active wallet key version", error))?;
                        let active_version = active_version
                            .and_then(|version| u32::try_from(version).ok())
                            .ok_or_else(|| conflict("local wallet has no active key to rotate"))?;
                        if custody != "local"
                            || created_at != wallet.created_at
                            || key.version != active_version.checked_add(1).ok_or_else(|| conflict("wallet key version overflow"))?
                            || retired_at < key.activated_at
                        {
                            return Err(conflict("wallet key rotation identity or version is invalid"));
                        }

                        let retired = sqlx::query(
                            "UPDATE wallet_key_version SET status = 'retired', retired_at = ?1 \
                             WHERE wallet_id = ?2 AND version = ?3 AND status = 'active'",
                        )
                        .bind(retired_at)
                        .bind(&wallet.id)
                        .bind(i64::from(active_version))
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| conflict(format!("retire active wallet key rejected: {error}")))?;
                        if retired.rows_affected() != 1 {
                            return Err(conflict("active wallet key changed during rotation"));
                        }
                        let updated = sqlx::query(
                            "UPDATE pay_wallet SET algorithm = ?1, address = ?2, public_key = ?3, updated_at = ?4 WHERE id = ?5",
                        )
                        .bind(&wallet.algorithm)
                        .bind(&wallet.address)
                        .bind(&wallet.public_key)
                        .bind(key.activated_at)
                        .bind(&wallet.id)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| conflict(format!("update wallet key metadata rejected: {error}")))?;
                        if updated.rows_affected() != 1 {
                            return Err(conflict("payment wallet changed during key rotation"));
                        }
                        insert_key(connection, &key).await?;
                        Ok(wallet)
                    })
                })
                .await
            }

            async fn mark_key_compromised(&self, wallet_id: &str, version: u32, retired_at: i64) -> Result<()> {
                let wallet_id = wallet_id.to_owned();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let result = sqlx::query(
                            "UPDATE wallet_key_version SET status = 'compromised', \
                             retired_at = CASE WHEN status = 'active' THEN ?1 ELSE retired_at END \
                             WHERE wallet_id = ?2 AND version = ?3 \
                             AND (status = 'active' OR (status IN ('retired', 'compromised') AND retired_at = ?1))",
                        )
                        .bind(retired_at)
                        .bind(&wallet_id)
                        .bind(i64::from(version))
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| conflict(format!("mark wallet key compromised rejected: {error}")))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("wallet key was not found or retirement time conflicts"));
                        }
                        Ok(())
                    })
                })
                .await
            }

            async fn set_spending_limits(
                &self,
                wallet_id: &str,
                budget_unit: &BudgetUnit,
                max_per_payment: Option<Amount>,
                max_per_day: Option<Amount>,
                updated_at: i64,
            ) -> Result<()> {
                budget_unit
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid wallet budget unit {field}")))?;
                if max_per_payment.is_some_and(Amount::is_zero)
                    || max_per_day.is_some_and(Amount::is_zero)
                    || max_per_payment
                        .zip(max_per_day)
                        .is_some_and(|(payment, day)| payment > day)
                {
                    return Err(Error::InvalidRequest("invalid wallet spending limits".to_owned()));
                }
                let wallet_id = wallet_id.to_owned();
                let budget_unit = budget_unit.clone();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let row = sqlx::query(
                            "SELECT budget_asset, budget_decimals, updated_at FROM pay_wallet WHERE id = ?1",
                        )
                        .bind(&wallet_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| storage_error("load payment wallet limits", error))?
                        .ok_or_else(|| Error::NotFound(format!("payment wallet {wallet_id}")))?;
                        let current_asset: Option<String> = row
                            .try_get("budget_asset")
                            .map_err(|error| storage_error("decode wallet budget asset", error))?;
                        let current_decimals: Option<i64> = row
                            .try_get("budget_decimals")
                            .map_err(|error| storage_error("decode wallet budget decimals", error))?;
                        let current_updated_at: i64 = row
                            .try_get("updated_at")
                            .map_err(|error| storage_error("decode wallet update time", error))?;
                        let bound_policies: i64 =
                            sqlx::query_scalar("SELECT count(*) FROM pay_policy_wallet WHERE wallet_id = ?1")
                                .bind(&wallet_id)
                                .fetch_one(&mut *connection)
                                .await
                                .map_err(|error| storage_error("count wallet payment policies", error))?;
                        let enabled_scenarios: i64 = sqlx::query_scalar(
                            "SELECT count(DISTINCT policy.scenario_id) FROM pay_policy_wallet membership \
                             JOIN pay_policy policy ON policy.id = membership.policy_id \
                             WHERE membership.wallet_id = ?1 AND policy.enabled = 1",
                        )
                        .bind(&wallet_id)
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|error| storage_error("count shared wallet scenarios", error))?;
                        if current_updated_at > updated_at
                            || (bound_policies > 0
                                && (current_asset.as_deref() != Some(budget_unit.asset.as_str())
                                    || current_decimals != Some(i64::from(budget_unit.decimals))))
                        {
                            return Err(conflict("wallet policy bindings, revision, or budget unit changed"));
                        }
                        if enabled_scenarios > 1 && (max_per_payment.is_none() || max_per_day.is_none()) {
                            return Err(conflict(
                                "wallet shared across scenarios requires aggregate hard limits",
                            ));
                        }
                        let result = sqlx::query(
                            "UPDATE pay_wallet SET budget_asset = ?1, budget_decimals = ?2, max_per_payment = ?3, \
                             max_per_day = ?4, updated_at = ?5 WHERE id = ?6 AND updated_at = ?7",
                        )
                        .bind(&budget_unit.asset)
                        .bind(i64::from(budget_unit.decimals))
                        .bind(max_per_payment.map(|amount| amount.to_string()))
                        .bind(max_per_day.map(|amount| amount.to_string()))
                        .bind(updated_at)
                        .bind(&wallet_id)
                        .bind(current_updated_at)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| conflict(format!("wallet limit update rejected: {error}")))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict(
                                "wallet was not found, revision moved forward, or budget unit differs",
                            ));
                        }
                        Ok(())
                    })
                })
                .await
            }

            async fn set_enabled(&self, wallet_id: &str, enabled: bool, updated_at: i64) -> Result<()> {
                let wallet_id = wallet_id.to_owned();
                self.immediate(|connection| {
                    Box::pin(async move {
                        let result = sqlx::query(
                            "UPDATE pay_wallet SET enabled = ?1, updated_at = ?2 WHERE id = ?3 AND updated_at <= ?2",
                        )
                        .bind(encode_bool(enabled))
                        .bind(updated_at)
                        .bind(&wallet_id)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("update payment wallet lifecycle", error))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("wallet was not found or update timestamp moved backwards"));
                        }
                        Ok(())
                    })
                })
                .await
            }
        }

        #[async_trait]
        impl WalletBalanceStorage for SqliteStorage {
            async fn upsert_balance(&self, balance: &WalletBalanceRecord) -> Result<WalletBalanceRecord> {
                balance
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid wallet balance {field}")))?;
                let token_contract = balance.asset.token_contract.as_deref().unwrap_or("");
                let result = sqlx::query(
                    "INSERT INTO pay_wallet_balance \
                     (wallet_id, network, asset, token_contract, asset_decimals, asset_available, \
                      native_available, observed_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                     ON CONFLICT(wallet_id, network, asset, token_contract, asset_decimals) DO UPDATE SET \
                       asset_available = excluded.asset_available, native_available = excluded.native_available, \
                       observed_at = excluded.observed_at, updated_at = excluded.updated_at \
                     WHERE excluded.observed_at >= pay_wallet_balance.observed_at",
                )
                .bind(&balance.wallet_id)
                .bind(&balance.asset.network)
                .bind(&balance.asset.asset)
                .bind(token_contract)
                .bind(i64::from(balance.asset.decimals))
                .bind(balance.asset_available.to_string())
                .bind(balance.native_available.to_string())
                .bind(balance.observed_at)
                .bind(balance.updated_at)
                .execute(self.pool())
                .await
                .map_err(|error| storage_error("upsert wallet balance", error))?;
                if result.rows_affected() != 1 {
                    return Err(conflict("wallet balance observation moved backwards"));
                }
                Ok(balance.clone())
            }

            async fn get_balance(&self, wallet_id: &str, asset: &AssetIdentity) -> Result<Option<WalletBalanceRecord>> {
                asset
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid wallet balance asset {field}")))?;
                let row = sqlx::query(
                    "SELECT asset_available, native_available, observed_at, updated_at \
                     FROM pay_wallet_balance WHERE wallet_id = ?1 AND network = ?2 AND asset = ?3 \
                     AND token_contract = ?4 AND asset_decimals = ?5",
                )
                .bind(wallet_id)
                .bind(&asset.network)
                .bind(&asset.asset)
                .bind(asset.token_contract.as_deref().unwrap_or(""))
                .bind(i64::from(asset.decimals))
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("get wallet balance", error))?;
                row.map(|row| {
                    let asset_available: String = row
                        .try_get("asset_available")
                        .map_err(|error| storage_error("decode wallet asset balance", error))?;
                    let native_available: String = row
                        .try_get("native_available")
                        .map_err(|error| storage_error("decode wallet native balance", error))?;
                    Ok(WalletBalanceRecord {
                        wallet_id: wallet_id.to_owned(),
                        asset: asset.clone(),
                        asset_available: decode_amount(&asset_available, "pay_wallet_balance.asset_available")?,
                        native_available: decode_amount(&native_available, "pay_wallet_balance.native_available")?,
                        observed_at: row
                            .try_get("observed_at")
                            .map_err(|error| storage_error("decode wallet balance observed_at", error))?,
                        updated_at: row
                            .try_get("updated_at")
                            .map_err(|error| storage_error("decode wallet balance updated_at", error))?,
                    })
                })
                .transpose()
            }
        }

        fn canonical_wallet(wallet: &Wallet) -> Result<Wallet> {
            if wallet.id.trim().is_empty() || wallet.algorithm.trim().is_empty() || wallet.address.trim().is_empty() {
                return Err(Error::InvalidRequest("wallet identity is incomplete".to_owned()));
            }
            wallet
                .clone()
                .canonicalized()
                .map_err(|field| Error::InvalidRequest(format!("invalid wallet {field}")))
        }

        fn validate_key_for_wallet(
            wallet: &Wallet,
            key: &EncryptedWalletKeyVersion,
            require_active: bool,
        ) -> Result<()> {
            if key.wallet_id != wallet.id
                || key.algorithm != wallet.algorithm
                || key.address != wallet.address
                || key.public_key != wallet.public_key
                || key.version == 0
                || key.encryption_scheme.trim().is_empty()
                || key.kek_id.trim().is_empty()
                || key.wrapped_data_key.is_empty()
                || key.wrap_nonce.is_empty()
                || key.key_ciphertext.is_empty()
                || key.key_nonce.is_empty()
                || key.aad.is_empty()
                || (require_active && key.status != WalletKeyStatus::Active)
                || key.retired_at.is_some()
            {
                return Err(Error::InvalidKey(
                    "encrypted wallet key metadata is inconsistent".to_owned(),
                ));
            }
            Ok(())
        }

        fn validate_provisioning_config(config: &WalletProvisioningConfig) -> Result<()> {
            config
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid initial wallet {field}")))
        }

        async fn insert_wallet(
            connection: &mut sqlx::SqliteConnection,
            wallet: &Wallet,
            custody: WalletCustody,
            config: &WalletProvisioningConfig,
        ) -> Result<()> {
            let public_key = if wallet.public_key.is_empty() {
                None
            } else {
                Some(wallet.public_key.as_str())
            };
            let custody = match custody {
                WalletCustody::Local => "local",
                WalletCustody::External => "external",
            };
            sqlx::query(
                "INSERT INTO pay_wallet \
                 (id, custody_type, algorithm, address, public_key, enabled, budget_asset, budget_decimals, \
                  max_per_payment, max_per_day, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            )
            .bind(&wallet.id)
            .bind(custody)
            .bind(&wallet.algorithm)
            .bind(&wallet.address)
            .bind(public_key)
            .bind(encode_bool(config.enabled))
            .bind(config.budget_unit.as_ref().map(|unit| unit.asset.as_str()))
            .bind(config.budget_unit.as_ref().map(|unit| i64::from(unit.decimals)))
            .bind(config.max_per_payment.map(|amount| amount.to_string()))
            .bind(config.max_per_day.map(|amount| amount.to_string()))
            .bind(wallet.created_at)
            .execute(connection)
            .await
            .map_err(|error| conflict(format!("payment wallet insert rejected: {error}")))?;
            Ok(())
        }

        pub(super) async fn insert_key(
            connection: &mut sqlx::SqliteConnection,
            key: &EncryptedWalletKeyVersion,
        ) -> Result<()> {
            let wallet =
                sqlx::query("SELECT custody_type, algorithm, address, public_key FROM pay_wallet WHERE id = ?1")
                    .bind(&key.wallet_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(|error| storage_error("validate encrypted wallet key owner", error))?
                    .ok_or_else(|| Error::NotFound(format!("payment wallet {}", key.wallet_id)))?;
            let custody: String = wallet
                .try_get("custody_type")
                .map_err(|error| storage_error("decode wallet custody", error))?;
            let algorithm: String = wallet
                .try_get("algorithm")
                .map_err(|error| storage_error("decode wallet algorithm", error))?;
            let address: String = wallet
                .try_get("address")
                .map_err(|error| storage_error("decode wallet address", error))?;
            let public_key: Option<String> = wallet
                .try_get("public_key")
                .map_err(|error| storage_error("decode wallet public key", error))?;
            if custody != "local"
                || (key.status == WalletKeyStatus::Active
                    && (algorithm != key.algorithm
                        || address != key.address
                        || public_key.as_deref() != Some(key.public_key.as_str())))
            {
                return Err(conflict("key version does not match a local-custody wallet"));
            }
            sqlx::query(
                "INSERT INTO wallet_key_version (wallet_id, version, algorithm, address, public_key, encryption_scheme, \
                 kek_id, wrapped_data_key, wrap_nonce, key_ciphertext, key_nonce, aad, status, activated_at, retired_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )
            .bind(&key.wallet_id)
            .bind(i64::from(key.version))
            .bind(&key.algorithm)
            .bind(&key.address)
            .bind(&key.public_key)
            .bind(&key.encryption_scheme)
            .bind(&key.kek_id)
            .bind(&key.wrapped_data_key)
            .bind(&key.wrap_nonce)
            .bind(&key.key_ciphertext)
            .bind(&key.key_nonce)
            .bind(&key.aad)
            .bind(key.status.as_str())
            .bind(key.activated_at)
            .bind(key.retired_at)
            .bind(key.created_at)
            .execute(connection)
            .await
            .map_err(|error| conflict(format!("encrypted wallet key insert rejected: {error}")))?;
            Ok(())
        }
    }

    // SQLite implementations for lifecycle-oriented catalog repositories.

    pub(super) use policy::load_policy_connection;
    pub(super) use wallet::load_payment_wallet_connection;
}

mod codec {
    use std::str::FromStr as _;

    use serde::{de::DeserializeOwned, Serialize};

    use crate::{model::Amount, Error, Result};

    pub(super) fn encode_json<T: Serialize>(value: &T, field: &str) -> Result<String> {
        serde_json::to_string(value).map_err(|error| Error::Storage(format!("encode SQLite {field}: {error}")))
    }

    pub(super) fn decode_json<T: DeserializeOwned>(value: &str, field: &str) -> Result<T> {
        serde_json::from_str(value).map_err(|error| Error::Storage(format!("decode SQLite {field}: {error}")))
    }

    pub(super) fn decode_amount(value: &str, field: &str) -> Result<Amount> {
        Amount::from_str(value).map_err(|_| Error::Storage(format!("SQLite {field} contains an invalid amount")))
    }

    pub(super) fn decode_optional_amount(value: Option<String>, field: &str) -> Result<Option<Amount>> {
        value.map(|amount| decode_amount(&amount, field)).transpose()
    }

    pub(super) const fn encode_bool(value: bool) -> i64 {
        value as i64
    }

    pub(super) fn decode_bool(value: i64, field: &str) -> Result<bool> {
        match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Storage(format!("SQLite {field} contains an invalid boolean"))),
        }
    }

    pub(super) fn decode_u8(value: i64, field: &str) -> Result<u8> {
        u8::try_from(value).map_err(|_| Error::Storage(format!("SQLite {field} is out of range")))
    }

    pub(super) fn decode_u32(value: i64, field: &str) -> Result<u32> {
        u32::try_from(value).map_err(|_| Error::Storage(format!("SQLite {field} is out of range")))
    }

    pub(super) fn page_bounds(page: super::super::PageRequest) -> Result<(i64, i64)> {
        let offset =
            i64::try_from(page.offset).map_err(|_| Error::Storage("SQLite page offset is too large".to_owned()))?;
        Ok((i64::from(page.limit), offset))
    }
}

mod event {
    use async_trait::async_trait;

    use crate::{
        model::{PaymentEvent, PaymentEventType},
        storage::{EventStorage, IdempotentWrite, Page, PageRequest},
        Error, Result,
    };

    use super::{codec::encode_json, conflict, records, storage_error, SqliteStorage};

    #[async_trait]
    impl EventStorage for SqliteStorage {
        async fn append(&self, event: &PaymentEvent) -> Result<IdempotentWrite<PaymentEvent>> {
            let event = event.clone();
            self.immediate(|connection| Box::pin(async move { insert_event(connection, &event).await }))
                .await
        }

        async fn get(&self, event_id: &str) -> Result<Option<PaymentEvent>> {
            let row = sqlx::query("SELECT * FROM pay_event WHERE id = ?1")
                .bind(event_id)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("get payment event", error))?;
            row.as_ref().map(records::event).transpose()
        }

        async fn find_for_intent_type(
            &self,
            intent_id: &str,
            event_type: PaymentEventType,
            attempt_no: u32,
        ) -> Result<Option<PaymentEvent>> {
            let row =
                sqlx::query("SELECT * FROM pay_event WHERE intent_id = ?1 AND event_type = ?2 AND attempt_no = ?3")
                    .bind(intent_id)
                    .bind(event_type.as_str())
                    .bind(i64::from(attempt_no))
                    .fetch_optional(self.pool())
                    .await
                    .map_err(|error| storage_error("find payment event by intent/type", error))?;
            row.as_ref().map(records::event).transpose()
        }

        async fn list_for_intent(&self, intent_id: &str, page: PageRequest) -> Result<Page<PaymentEvent>> {
            let (limit, offset) = super::codec::page_bounds(page)?;
            let total: i64 = sqlx::query_scalar("SELECT count(*) FROM pay_event WHERE intent_id = ?1")
                .bind(intent_id)
                .fetch_one(self.pool())
                .await
                .map_err(|error| storage_error("count payment events", error))?;
            let rows =
                sqlx::query("SELECT * FROM pay_event WHERE intent_id = ?1 ORDER BY created_at, id LIMIT ?2 OFFSET ?3")
                    .bind(intent_id)
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(self.pool())
                    .await
                    .map_err(|error| storage_error("list payment events", error))?;
            Ok(Page {
                items: rows.iter().map(records::event).collect::<Result<Vec<_>>>()?,
                total: u64::try_from(total).map_err(|_| Error::Storage("negative payment event count".to_owned()))?,
            })
        }
    }

    pub(super) async fn insert_event(
        connection: &mut sqlx::SqliteConnection,
        event: &PaymentEvent,
    ) -> Result<IdempotentWrite<PaymentEvent>> {
        event
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment event {field}")))?;
        let existing = sqlx::query(
            "SELECT * FROM pay_event WHERE id = ?1 OR (intent_id = ?2 AND event_type = ?3 AND attempt_no = ?4)",
        )
        .bind(&event.id)
        .bind(&event.intent_id)
        .bind(event.event_type.as_str())
        .bind(i64::from(event.attempt_no))
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| storage_error("find payment event replay", error))?;
        if !existing.is_empty() {
            if existing.len() != 1 {
                return Err(conflict("payment event identifiers collide with multiple rows"));
            }
            let existing = records::event(&existing[0])?;
            return if existing == *event {
                Ok(IdempotentWrite::Replay(existing))
            } else {
                Err(conflict("payment event idempotency identity has different content"))
            };
        }

        let payload = encode_json(&event.payload, "pay_event.payload")?;
        sqlx::query(
            "INSERT INTO pay_event (id, intent_id, event_type, attempt_no, signature_ref, tx_hash, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&event.id)
        .bind(&event.intent_id)
        .bind(event.event_type.as_str())
        .bind(i64::from(event.attempt_no))
        .bind(&event.signature_ref)
        .bind(&event.tx_hash)
        .bind(payload)
        .bind(event.created_at)
        .execute(connection)
        .await
        .map_err(|error| conflict(format!("payment event append rejected: {error}")))?;
        Ok(IdempotentWrite::Inserted(event.clone()))
    }
}

mod key_repository {
    use std::{collections::BTreeMap, fmt};

    use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
    use async_trait::async_trait;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use rand_core::{OsRng, RngCore as _};
    use zeroize::{Zeroize as _, Zeroizing};

    use crate::{
        config::WalletKeyRingConfig,
        model::wallet::{Wallet, EVM_EOA_ALGORITHM},
        storage::{
            EncryptedWalletKeyVersion, IntentBoundPaymentSignatureRequest, WalletKeyManager, WalletKeyStatus,
            WalletPaymentSignature, WalletPaymentSigner, WalletProvisioningConfig, WalletStorage as _,
        },
        utils::{
            aead::{new_uuid_nonce, parse_uuid_nonce, Aes256GcmUuidNonce, UuidNonce, WALLET_KEY_ENCRYPTION_SCHEME},
            crypto,
            secret::read_secret_env,
        },
        Error, Result,
    };

    use super::SqliteStorage;

    const KEY_BYTES: usize = 32;
    const MAX_ENCODED_KEK_BYTES: usize = 256;
    const AAD_DOMAIN: &[u8] = b"agentvault.wallet-key.v2";

    /// Versioned envelope encryption and SQL persistence for local EOA keys.
    ///
    /// KEKs are resolved only from environment-variable references at startup.
    /// The active key encrypts new versions; historical KEKs remain decrypt-only
    /// until no retained wallet version references them.
    pub struct SqliteWalletKeyRepository {
        storage: SqliteStorage,
        active_kek_id: String,
        keks: BTreeMap<String, Zeroizing<[u8; KEY_BYTES]>>,
    }

    #[async_trait]
    impl WalletPaymentSigner for SqliteWalletKeyRepository {
        async fn sign_payment(
            &self,
            request: IntentBoundPaymentSignatureRequest<'_>,
        ) -> Result<WalletPaymentSignature> {
            let intent_wallet_id = request
                .intent()
                .wallet_id
                .as_deref()
                .ok_or_else(|| Error::InvalidRequest("payment intent has no wallet binding".to_owned()))?;
            if intent_wallet_id != request.wallet().wallet.id
                || request.active_key().wallet_id != request.wallet().wallet.id
                || request.wallet().active_key_version != Some(request.active_key().version)
                || request.active_key().status != WalletKeyStatus::Active
                || !request
                    .wallet()
                    .signing_ready_for_payment_network(&request.intent().asset.network)
                || request.intent().challenge_hash.as_deref() != Some(request.challenge().challenge_hash.as_str())
            {
                return Err(Error::PaymentConflict(
                    "intent-bound signer received inconsistent wallet authority".to_owned(),
                ));
            }
            let private_key = self.decrypt_version(request.active_key())?;
            let bytes = crypto::sign_eip712_digest(&private_key, request.digest())?.to_vec();
            Ok(WalletPaymentSignature {
                wallet_id: request.wallet().wallet.id.clone(),
                key_version: request.active_key().version,
                signer_address: request.wallet().wallet.address.clone(),
                bytes,
            })
        }
    }

    #[async_trait]
    impl WalletKeyManager for SqliteWalletKeyRepository {
        async fn create_local(
            &self,
            wallet_id: &str,
            private_key: &[u8],
            config: &WalletProvisioningConfig,
            created_at: i64,
        ) -> Result<Wallet> {
            self.create_local_configured(wallet_id, private_key, config, created_at)
                .await
        }

        async fn rotate(&self, wallet_id: &str, private_key: &[u8], activated_at: i64) -> Result<Wallet> {
            SqliteWalletKeyRepository::rotate(self, wallet_id, private_key, activated_at).await
        }
    }

    impl fmt::Debug for SqliteWalletKeyRepository {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("SqliteWalletKeyRepository")
                .field("active_kek_id", &self.active_kek_id)
                .field("kek_ids", &self.keks.keys().collect::<Vec<_>>())
                .finish_non_exhaustive()
        }
    }

    impl SqliteWalletKeyRepository {
        /// Resolves a KEK ring from environment-only secret references.
        ///
        /// # Errors
        ///
        /// Fails when an inline/file source is selected, an environment variable
        /// is missing, a value is not unpadded URL-safe base64 for exactly 32
        /// bytes, or the active key id is unavailable.
        pub fn from_config(storage: SqliteStorage, config: &WalletKeyRingConfig) -> Result<Self> {
            if config.keys.is_empty() || !config.keys.contains_key(&config.active_key_id) {
                return Err(Error::Config("wallet KEK active version is unavailable".to_owned()));
            }
            let mut keks = BTreeMap::new();
            for (key_id, reference) in &config.keys {
                if key_id.trim().is_empty() || !reference.file.is_empty() || reference.env.trim().is_empty() {
                    return Err(Error::Config(
                        "wallet KEKs must use named environment references".to_owned(),
                    ));
                }
                let encoded = read_secret_env(
                    reference.env.trim(),
                    &format!("wallet KEK {key_id}"),
                    MAX_ENCODED_KEK_BYTES,
                )?;
                if encoded.len() > MAX_ENCODED_KEK_BYTES {
                    return Err(Error::Config(format!("wallet KEK {key_id} is too large")));
                }
                let mut decoded = Zeroizing::new(
                    URL_SAFE_NO_PAD
                        .decode(encoded.as_bytes())
                        .map_err(|_| Error::Config(format!("wallet KEK {key_id} is not valid base64")))?,
                );
                let key: [u8; KEY_BYTES] = decoded
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Config(format!("wallet KEK {key_id} must contain 32 bytes")))?;
                decoded.zeroize();
                if keks.insert(key_id.clone(), Zeroizing::new(key)).is_some() {
                    return Err(Error::Config(format!("duplicate wallet KEK id {key_id}")));
                }
            }
            Ok(Self {
                storage,
                active_kek_id: config.active_key_id.clone(),
                keks,
            })
        }

        #[cfg(test)]
        pub(super) fn from_resolved_keys(
            storage: SqliteStorage,
            active_kek_id: &str,
            keys: impl IntoIterator<Item = (String, [u8; KEY_BYTES])>,
        ) -> Result<Self> {
            let resolved_keys = keys
                .into_iter()
                .map(|(key_id, key)| (key_id, Zeroizing::new(key)))
                .collect::<BTreeMap<_, _>>();
            if active_kek_id.trim().is_empty() || resolved_keys.is_empty() || !resolved_keys.contains_key(active_kek_id)
            {
                return Err(Error::Config("wallet KEK active version is unavailable".to_owned()));
            }
            Ok(Self {
                storage,
                active_kek_id: active_kek_id.to_owned(),
                keks: resolved_keys,
            })
        }

        /// Creates a local EOA wallet and its encrypted initial key in one SQL
        /// transaction.
        ///
        /// # Errors
        ///
        /// Fails when the identifier or private key is invalid, encryption fails,
        /// or the wallet/key aggregate cannot be inserted atomically.
        pub async fn create_local(&self, wallet_id: &str, private_key: &[u8], created_at: i64) -> Result<Wallet> {
            self.create_local_configured(
                wallet_id,
                private_key,
                &WalletProvisioningConfig {
                    enabled: true,
                    ..WalletProvisioningConfig::default()
                },
                created_at,
            )
            .await
        }

        /// Creates a local EOA wallet, encrypted initial key, lifecycle state, and
        /// wallet-level limits in one SQL transaction.
        ///
        /// # Errors
        ///
        /// Fails when key material or initial metadata is invalid, encryption
        /// fails, or the complete wallet aggregate cannot be committed atomically.
        pub async fn create_local_configured(
            &self,
            wallet_id: &str,
            private_key: &[u8],
            config: &WalletProvisioningConfig,
            created_at: i64,
        ) -> Result<Wallet> {
            validate_wallet_id(wallet_id)?;
            let wallet = wallet_from_private_key(wallet_id, private_key, created_at)?;
            let encrypted = self.encrypt_version(&wallet, private_key, 1, created_at)?;
            self.storage.create_local_configured(&wallet, &encrypted, config).await
        }

        /// Rotates a local wallet to a new immutable encrypted key version.
        ///
        /// # Errors
        ///
        /// Fails when the wallet has no active local key, the replacement key is
        /// invalid, encryption fails, or the compare-and-swap rotation conflicts.
        pub async fn rotate(&self, wallet_id: &str, private_key: &[u8], activated_at: i64) -> Result<Wallet> {
            let current = self
                .storage
                .get_payment_wallet(wallet_id)
                .await?
                .ok_or_else(|| Error::NotFound(format!("payment wallet {wallet_id}")))?;
            let current_version = current
                .active_key_version
                .ok_or_else(|| Error::InvalidTransition("wallet has no active key to rotate".to_owned()))?;
            let next_version = current_version
                .checked_add(1)
                .ok_or_else(|| Error::InvalidTransition("wallet key version overflow".to_owned()))?;
            let wallet = wallet_from_private_key(wallet_id, private_key, current.wallet.created_at)?;
            let encrypted = self.encrypt_version(&wallet, private_key, next_version, activated_at)?;
            self.storage.rotate(&wallet, &encrypted, activated_at).await
        }

        /// Produces the immutable SQL envelope for one EOA version.
        ///
        /// # Errors
        ///
        /// Fails when metadata is invalid, the private key does not match the
        /// wallet, the active KEK is unavailable, or authenticated encryption fails.
        pub fn encrypt_version(
            &self,
            wallet: &Wallet,
            private_key: &[u8],
            version: u32,
            activated_at: i64,
        ) -> Result<EncryptedWalletKeyVersion> {
            if version == 0 || activated_at < 0 {
                return Err(Error::InvalidKey("wallet key version metadata is invalid".to_owned()));
            }
            verify_private_key_matches_wallet(wallet, private_key)?;
            let aad = key_aad(wallet, version, &self.active_kek_id);
            let kek = self
                .keks
                .get(&self.active_kek_id)
                .ok_or_else(|| Error::Crypto("active wallet KEK is unavailable".to_owned()))?;
            let mut data_key = Zeroizing::new([0_u8; KEY_BYTES]);
            let wrap_nonce = new_uuid_nonce();
            let key_nonce = new_uuid_nonce();
            OsRng.fill_bytes(data_key.as_mut());
            let wrap_aad = wrapping_aad(&aad, &self.active_kek_id);
            let kek_cipher = Aes256GcmUuidNonce::new_from_slice(kek.as_ref())
                .map_err(|_| Error::Crypto("wallet KEK length is invalid".to_owned()))?;
            let wrapped_data_key = kek_cipher
                .encrypt(
                    UuidNonce::from_slice(&wrap_nonce),
                    Payload {
                        msg: data_key.as_ref(),
                        aad: &wrap_aad,
                    },
                )
                .map_err(|_| Error::Crypto("wallet data-key wrapping failed".to_owned()))?;
            let data_cipher = Aes256GcmUuidNonce::new_from_slice(data_key.as_ref())
                .map_err(|_| Error::Crypto("wallet data-key length is invalid".to_owned()))?;
            let key_ciphertext = data_cipher
                .encrypt(
                    UuidNonce::from_slice(&key_nonce),
                    Payload {
                        msg: private_key,
                        aad: &aad,
                    },
                )
                .map_err(|_| Error::Crypto("wallet private-key encryption failed".to_owned()))?;
            Ok(EncryptedWalletKeyVersion {
                wallet_id: wallet.id.clone(),
                version,
                algorithm: wallet.algorithm.clone(),
                address: wallet.address.clone(),
                public_key: wallet.public_key.clone(),
                encryption_scheme: WALLET_KEY_ENCRYPTION_SCHEME.to_owned(),
                kek_id: self.active_kek_id.clone(),
                wrapped_data_key,
                wrap_nonce: wrap_nonce.to_vec(),
                key_ciphertext,
                key_nonce: key_nonce.to_vec(),
                aad,
                status: WalletKeyStatus::Active,
                activated_at,
                retired_at: None,
                created_at: activated_at,
            })
        }

        fn decrypt_version(&self, key: &EncryptedWalletKeyVersion) -> Result<Zeroizing<Vec<u8>>> {
            if key.encryption_scheme != WALLET_KEY_ENCRYPTION_SCHEME || key.status != WalletKeyStatus::Active {
                return Err(Error::Crypto(
                    "wallet key envelope is not an active supported version".to_owned(),
                ));
            }
            let wallet = Wallet {
                id: key.wallet_id.clone(),
                algorithm: key.algorithm.clone(),
                address: key.address.clone(),
                public_key: key.public_key.clone(),
                created_at: 0,
            };
            let expected_aad = key_aad(&wallet, key.version, &key.kek_id);
            if !constant_time_eq(&expected_aad, &key.aad) {
                return Err(Error::Crypto(
                    "wallet key envelope metadata authentication failed".to_owned(),
                ));
            }
            let wrap_nonce = parse_uuid_nonce(&key.wrap_nonce)
                .map_err(|_| Error::Crypto("wallet key wrapping nonce is invalid".to_owned()))?;
            let key_nonce = parse_uuid_nonce(&key.key_nonce)
                .map_err(|_| Error::Crypto("wallet key nonce is invalid".to_owned()))?;
            let kek = self
                .keks
                .get(&key.kek_id)
                .ok_or_else(|| Error::Crypto("wallet KEK version is unavailable".to_owned()))?;
            let wrap_aad = wrapping_aad(&key.aad, &key.kek_id);
            let kek_cipher = Aes256GcmUuidNonce::new_from_slice(kek.as_ref())
                .map_err(|_| Error::Crypto("wallet KEK length is invalid".to_owned()))?;
            let mut data_key = Zeroizing::new(
                kek_cipher
                    .decrypt(
                        UuidNonce::from_slice(&wrap_nonce),
                        Payload {
                            msg: &key.wrapped_data_key,
                            aad: &wrap_aad,
                        },
                    )
                    .map_err(|_| Error::Crypto("wallet data-key authentication failed".to_owned()))?,
            );
            if data_key.len() != KEY_BYTES {
                return Err(Error::Crypto("wallet data key has an invalid length".to_owned()));
            }
            let data_cipher = Aes256GcmUuidNonce::new_from_slice(data_key.as_slice())
                .map_err(|_| Error::Crypto("wallet data-key length is invalid".to_owned()))?;
            let plaintext = data_cipher
                .decrypt(
                    UuidNonce::from_slice(&key_nonce),
                    Payload {
                        msg: &key.key_ciphertext,
                        aad: &key.aad,
                    },
                )
                .map(Zeroizing::new)
                .map_err(|_| Error::Crypto("wallet private-key authentication failed".to_owned()))?;
            data_key.zeroize();
            verify_private_key_matches_wallet(&wallet, &plaintext)?;
            Ok(plaintext)
        }
    }

    fn wallet_from_private_key(wallet_id: &str, private_key: &[u8], created_at: i64) -> Result<Wallet> {
        crypto::validate_private_key(private_key)?;
        Ok(Wallet {
            id: wallet_id.to_owned(),
            algorithm: EVM_EOA_ALGORITHM.to_owned(),
            address: crypto::address(private_key)?,
            public_key: crypto::public_key(private_key)?,
            created_at,
        })
    }

    fn verify_private_key_matches_wallet(wallet: &Wallet, private_key: &[u8]) -> Result<()> {
        crypto::validate_private_key(private_key)?;
        if wallet.algorithm != EVM_EOA_ALGORITHM
            || crypto::address(private_key)? != wallet.address
            || crypto::public_key(private_key)? != wallet.public_key
        {
            return Err(Error::InvalidKey(
                "private key does not match wallet public metadata".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_wallet_id(wallet_id: &str) -> Result<()> {
        if wallet_id.trim().is_empty() || wallet_id.len() > 256 || wallet_id.chars().any(char::is_control) {
            return Err(Error::InvalidRequest("wallet id is invalid".to_owned()));
        }
        Ok(())
    }

    fn key_aad(wallet: &Wallet, version: u32, kek_id: &str) -> Vec<u8> {
        let version = version.to_be_bytes();
        let fields = [
            wallet.id.as_bytes(),
            version.as_slice(),
            wallet.algorithm.as_bytes(),
            wallet.address.as_bytes(),
            wallet.public_key.as_bytes(),
            kek_id.as_bytes(),
        ];
        let mut aad = Vec::with_capacity(512);
        aad.extend_from_slice(AAD_DOMAIN);
        for field in fields {
            aad.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
            aad.extend_from_slice(field);
        }
        aad
    }

    fn wrapping_aad(aad: &[u8], kek_id: &str) -> Vec<u8> {
        let mut output = Vec::with_capacity(aad.len() + kek_id.len() + 16);
        output.extend_from_slice(b"agentvault.wrap.v1");
        output.extend_from_slice(aad);
        output.extend_from_slice(kek_id.as_bytes());
        output
    }

    fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
        if left.len() != right.len() {
            return false;
        }
        left.iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

mod ledger {
    use async_trait::async_trait;

    use crate::{
        model::PaymentLedgerEntry,
        storage::{IdempotentWrite, LedgerQuery, LedgerStorage, LedgerWrite, Page},
        Error, Result,
    };

    use super::{conflict, outbox::insert_outbox, records, storage_error, SqliteStorage};

    #[async_trait]
    impl LedgerStorage for SqliteStorage {
        async fn append(&self, entry: &PaymentLedgerEntry) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
            let entry = entry.clone();
            self.immediate(|connection| Box::pin(async move { insert_ledger(connection, &entry).await }))
                .await
        }

        async fn append_batch(&self, write: LedgerWrite<'_>) -> Result<Vec<IdempotentWrite<PaymentLedgerEntry>>> {
            let entries = write.entries.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    let mut results = Vec::with_capacity(entries.len());
                    for entry in &entries {
                        results.push(insert_ledger(connection, entry).await?);
                    }
                    for message in &outbox {
                        insert_outbox(connection, message).await?;
                    }
                    Ok(results)
                })
            })
            .await
        }

        async fn get(&self, ledger_id: &str) -> Result<Option<PaymentLedgerEntry>> {
            let row = sqlx::query("SELECT * FROM pay_ledger WHERE id = ?1")
                .bind(ledger_id)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("get ledger entry", error))?;
            row.as_ref().map(records::ledger).transpose()
        }

        async fn find_posting(&self, wallet_id: &str, posting_key: &str) -> Result<Option<PaymentLedgerEntry>> {
            let row = sqlx::query("SELECT * FROM pay_ledger WHERE wallet_id = ?1 AND posting_key = ?2")
                .bind(wallet_id)
                .bind(posting_key)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("find ledger posting", error))?;
            row.as_ref().map(records::ledger).transpose()
        }

        async fn list(&self, query: &LedgerQuery) -> Result<Page<PaymentLedgerEntry>> {
            let direction = query.direction.map(crate::model::PaymentDirection::as_str);
            let count_sql = "SELECT count(*) FROM pay_ledger WHERE \
                (?1 IS NULL OR scenario_id = ?1) AND (?2 IS NULL OR wallet_id = ?2) \
                AND (?3 IS NULL OR intent_id = ?3) AND (?4 IS NULL OR direction = ?4) \
                AND (?5 IS NULL OR network = ?5) AND (?6 IS NULL OR asset = ?6) \
                AND (?7 IS NULL OR effective_at >= ?7) AND (?8 IS NULL OR effective_at <= ?8)";
            let total: i64 = sqlx::query_scalar(count_sql)
                .bind(query.scenario_id.as_deref())
                .bind(query.wallet_id.as_deref())
                .bind(query.intent_id.as_deref())
                .bind(direction)
                .bind(query.network.as_deref())
                .bind(query.asset.as_deref())
                .bind(query.from_effective_at)
                .bind(query.to_effective_at)
                .fetch_one(self.pool())
                .await
                .map_err(|error| storage_error("count ledger entries", error))?;
            let (limit, offset) = super::codec::page_bounds(query.page)?;
            let select_sql = "SELECT * FROM pay_ledger WHERE \
                (?1 IS NULL OR scenario_id = ?1) AND (?2 IS NULL OR wallet_id = ?2) \
                AND (?3 IS NULL OR intent_id = ?3) AND (?4 IS NULL OR direction = ?4) \
                AND (?5 IS NULL OR network = ?5) AND (?6 IS NULL OR asset = ?6) \
                AND (?7 IS NULL OR effective_at >= ?7) AND (?8 IS NULL OR effective_at <= ?8) \
                ORDER BY effective_at DESC, id LIMIT ?9 OFFSET ?10";
            let rows = sqlx::query(select_sql)
                .bind(query.scenario_id.as_deref())
                .bind(query.wallet_id.as_deref())
                .bind(query.intent_id.as_deref())
                .bind(direction)
                .bind(query.network.as_deref())
                .bind(query.asset.as_deref())
                .bind(query.from_effective_at)
                .bind(query.to_effective_at)
                .bind(limit)
                .bind(offset)
                .fetch_all(self.pool())
                .await
                .map_err(|error| storage_error("list ledger entries", error))?;
            Ok(Page {
                items: rows.iter().map(records::ledger).collect::<Result<Vec<_>>>()?,
                total: u64::try_from(total).map_err(|_| Error::Storage("negative ledger entry count".to_owned()))?,
            })
        }
    }

    pub(super) async fn insert_ledger(
        connection: &mut sqlx::SqliteConnection,
        entry: &PaymentLedgerEntry,
    ) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
        entry
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid ledger entry {field}")))?;
        let existing = sqlx::query("SELECT * FROM pay_ledger WHERE wallet_id = ?1 AND posting_key = ?2")
            .bind(&entry.wallet_id)
            .bind(&entry.posting_key)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("find ledger posting replay", error))?;
        if let Some(row) = existing {
            let existing = records::ledger(&row)?;
            return if existing.same_fact_content_as(entry) {
                Ok(IdempotentWrite::Replay(existing))
            } else {
                Err(conflict(
                    "ledger posting key is already bound to different financial content",
                ))
            };
        }
        if let Some(original_id) = &entry.reversal_of {
            let row = sqlx::query("SELECT * FROM pay_ledger WHERE id = ?1")
                .bind(original_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| storage_error("load ledger reversal original", error))?
                .ok_or_else(|| conflict("ledger reversal original does not exist"))?;
            let original = records::ledger(&row)?;
            entry
                .validate_reversal_of(&original)
                .map_err(|_| conflict("ledger reversal does not exactly negate its original"))?;
        }
        sqlx::query(
            "INSERT INTO pay_ledger (id, intent_id, scenario_id, wallet_id, entry_type, direction, network, asset, \
             token_contract, asset_decimals, amount, posting_key, tx_hash, settlement_ref, effect_ref, transfer_ref, \
             reversal_of, effective_at, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        )
        .bind(&entry.id)
        .bind(&entry.intent_id)
        .bind(&entry.scenario_id)
        .bind(&entry.wallet_id)
        .bind(entry.entry_type.as_str())
        .bind(entry.direction.as_str())
        .bind(&entry.asset.network)
        .bind(&entry.asset.asset)
        .bind(&entry.asset.token_contract)
        .bind(i64::from(entry.asset.decimals))
        .bind(entry.amount.to_string())
        .bind(&entry.posting_key)
        .bind(&entry.tx_hash)
        .bind(&entry.settlement_ref)
        .bind(&entry.effect_ref)
        .bind(&entry.transfer_ref)
        .bind(&entry.reversal_of)
        .bind(entry.effective_at)
        .bind(entry.created_at)
        .execute(connection)
        .await
        .map_err(|error| conflict(format!("ledger append rejected: {error}")))?;
        Ok(IdempotentWrite::Inserted(entry.clone()))
    }
}

mod outbox {
    use async_trait::async_trait;

    use crate::{
        storage::{OutboxLeaseRequest, OutboxMessage, OutboxStorage},
        Error, Result,
    };

    use super::{codec::encode_json, conflict, records, storage_error, SqliteStorage};

    #[async_trait]
    impl OutboxStorage for SqliteStorage {
        async fn claim(&self, request: &OutboxLeaseRequest) -> Result<Vec<OutboxMessage>> {
            if request.owner.trim().is_empty() || request.lease_until <= request.now || request.limit == 0 {
                return Err(Error::InvalidRequest("invalid outbox lease request".to_owned()));
            }
            let request = request.clone();
            self.immediate(|connection| {
                Box::pin(async move {
                    let ids: Vec<String> = sqlx::query_scalar(
                        "SELECT id FROM pay_outbox WHERE status = 'pending' AND available_at <= ?1 \
                         AND (lease_expires_at IS NULL OR lease_expires_at <= ?1) \
                         AND ((?3 = 'approval_required' AND event_type = 'approval_required' \
                               AND destination GLOB 'client:?*.approvals') \
                              OR (?3 = 'payment_events' AND (destination GLOB 'client:?*.payments' \
                                  OR destination GLOB 'scenario:?*.payments'))) \
                         ORDER BY available_at, created_at, id LIMIT ?2",
                    )
                    .bind(request.now)
                    .bind(i64::from(request.limit))
                    .bind(request.class.as_str())
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| storage_error("select due outbox messages", error))?;
                    let mut claimed = Vec::with_capacity(ids.len());
                    for id in ids {
                        let result = sqlx::query(
                            "UPDATE pay_outbox SET lease_owner = ?1, lease_expires_at = ?2, \
                             attempt_count = attempt_count + 1 WHERE id = ?3 AND status = 'pending' \
                             AND available_at <= ?4 AND (lease_expires_at IS NULL OR lease_expires_at <= ?4) \
                             AND attempt_count < 4294967295",
                        )
                        .bind(&request.owner)
                        .bind(request.lease_until)
                        .bind(&id)
                        .bind(request.now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("claim outbox message", error))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("outbox claim lost its conditional lease update"));
                        }
                        let row = sqlx::query("SELECT * FROM pay_outbox WHERE id = ?1")
                            .bind(&id)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| storage_error("reload claimed outbox message", error))?;
                        claimed.push(records::outbox(&row)?);
                    }
                    Ok(claimed)
                })
            })
            .await
        }

        async fn mark_delivered(&self, message_id: &str, owner: &str, delivered_at: i64) -> Result<()> {
            transition_lease(
                self,
                "UPDATE pay_outbox SET status = 'delivered', delivered_at = ?1, lease_owner = NULL, \
                 lease_expires_at = NULL, last_error = NULL WHERE id = ?2 AND status = 'pending' AND lease_owner = ?3",
                message_id,
                owner,
                delivered_at,
                None,
                "mark outbox message delivered",
            )
            .await
        }

        async fn reschedule(&self, message_id: &str, owner: &str, available_at: i64, error: &str) -> Result<()> {
            transition_lease(
                self,
                "UPDATE pay_outbox SET available_at = ?1, lease_owner = NULL, lease_expires_at = NULL, \
                 last_error = ?4 WHERE id = ?2 AND status = 'pending' AND lease_owner = ?3",
                message_id,
                owner,
                available_at,
                Some(error),
                "reschedule outbox message",
            )
            .await
        }

        async fn mark_dead_letter(&self, message_id: &str, owner: &str, error: &str) -> Result<()> {
            transition_lease(
                self,
                "UPDATE pay_outbox SET status = 'dead_letter', lease_owner = NULL, lease_expires_at = NULL, \
                 last_error = ?4 WHERE id = ?2 AND status = 'pending' AND lease_owner = ?3",
                message_id,
                owner,
                0,
                Some(error),
                "mark outbox message dead-letter",
            )
            .await
        }
    }

    pub(super) async fn insert_outbox(connection: &mut sqlx::SqliteConnection, message: &OutboxMessage) -> Result<()> {
        validate_message(message)?;
        let existing = sqlx::query(
            "SELECT * FROM pay_outbox WHERE id = ?1 OR (destination = ?2 AND event_type = ?3 AND run_id = ?4)",
        )
        .bind(&message.id)
        .bind(&message.destination)
        .bind(&message.event_type)
        .bind(&message.run_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| storage_error("find outbox replay", error))?;
        if !existing.is_empty() {
            if existing.len() != 1 || !same_logical_message(&records::outbox(&existing[0])?, message) {
                return Err(conflict("outbox run identity has different content"));
            }
            return Ok(());
        }
        let payload = encode_json(&message.payload, "pay_outbox.payload")?;
        sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
             run_id, payload, status, attempt_count, available_at, lease_owner, lease_expires_at, \
             last_error, created_at, delivered_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .bind(&message.id)
        .bind(&message.aggregate_type)
        .bind(&message.aggregate_id)
        .bind(&message.event_type)
        .bind(&message.destination)
        .bind(&message.run_id)
        .bind(payload)
        .bind(message.status.as_str())
        .bind(i64::from(message.attempt_count))
        .bind(message.available_at)
        .bind(&message.lease_owner)
        .bind(message.lease_expires_at)
        .bind(&message.last_error)
        .bind(message.created_at)
        .bind(message.delivered_at)
        .execute(connection)
        .await
        .map_err(|error| conflict(format!("outbox insert rejected: {error}")))?;
        Ok(())
    }

    fn validate_message(message: &OutboxMessage) -> Result<()> {
        if message.id.trim().is_empty()
            || message.aggregate_type.trim().is_empty()
            || message.aggregate_id.trim().is_empty()
            || message.event_type.trim().is_empty()
            || message.destination.trim().is_empty()
            || message.run_id.trim().is_empty()
        {
            return Err(Error::InvalidRequest(
                "outbox message identity is incomplete".to_owned(),
            ));
        }
        Ok(())
    }

    fn same_logical_message(left: &OutboxMessage, right: &OutboxMessage) -> bool {
        left.id == right.id
            && left.aggregate_type == right.aggregate_type
            && left.aggregate_id == right.aggregate_id
            && left.event_type == right.event_type
            && left.destination == right.destination
            && left.run_id == right.run_id
            && left.payload == right.payload
            && left.created_at == right.created_at
    }

    async fn transition_lease(
        storage: &SqliteStorage,
        sql: &'static str,
        message_id: &str,
        owner: &str,
        timestamp: i64,
        error: Option<&str>,
        operation: &'static str,
    ) -> Result<()> {
        if message_id.trim().is_empty() || owner.trim().is_empty() {
            return Err(Error::InvalidRequest("outbox lease identity is incomplete".to_owned()));
        }
        let message_id = message_id.to_owned();
        let owner = owner.to_owned();
        let error = error.map(str::to_owned);
        storage
            .immediate(|connection| {
                Box::pin(async move {
                    let result = sqlx::query(sql)
                        .bind(timestamp)
                        .bind(&message_id)
                        .bind(&owner)
                        .bind(error)
                        .execute(&mut *connection)
                        .await
                        .map_err(|source| storage_error(operation, source))?;
                    if result.rows_affected() != 1 {
                        return Err(conflict("outbox message is not held by the lease owner"));
                    }
                    Ok(())
                })
            })
            .await
    }
}

mod payment {
    use async_trait::async_trait;
    use sqlx::{Row as _, SqliteConnection};

    use crate::{
        model::{
            payment_recipients_match, Amount, ApprovalRequest, BudgetUnit, PaymentChallenge, PaymentCredential,
            PaymentDirection, PaymentEvent, PaymentEventType, PaymentIntent, PaymentLedgerEntry, PaymentStatus,
            ReservationStatus, MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS,
        },
        storage::{
            AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
            ChallengeIssuanceGuard, ChallengeWrite, IdempotentWrite, OutboundEvidenceWrite, Page, PaymentProgressWrite,
            PaymentQuery, PaymentStorage, ReconciliationLeaseRequest, SettlementWrite, VerificationWrite,
        },
        Error, Result,
    };

    use super::{
        catalog::{load_payment_wallet_connection, load_policy_connection},
        conflict,
        event::insert_event,
        ledger::insert_ledger,
        outbox::insert_outbox,
        records, storage_error, SqliteStorage,
    };

    fn validate_challenge_received_event(events: &[PaymentEvent], intent: &PaymentIntent) -> Result<()> {
        let matching = events
            .iter()
            .filter(|event| event.event_type == PaymentEventType::ChallengeReceived)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(conflict(
                "outbound authorization must contain exactly one received challenge event",
            ));
        }
        let event = matching[0];
        let challenge: PaymentChallenge = event
            .payload
            .get("challenge")
            .cloned()
            .ok_or_else(|| conflict("received challenge event has no complete challenge"))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|_| conflict("received challenge event contains an invalid challenge"))
            })?;
        if event.intent_id != intent.id
            || event.attempt_no != 0
            || event.signature_ref.is_some()
            || event.tx_hash.is_some()
            || event.created_at != intent.created_at
            || event.payload.get("challenge_hash").and_then(serde_json::Value::as_str)
                != Some(challenge.challenge_hash.as_str())
            || !challenge_matches_intent(&challenge, intent, PaymentDirection::Outbound)
        {
            return Err(conflict(
                "received challenge event is not bound to the outbound payment intent",
            ));
        }
        Ok(())
    }

    fn validate_challenge_created_event(
        events: &[PaymentEvent],
        intent: &PaymentIntent,
        expected: &PaymentChallenge,
    ) -> Result<()> {
        let matching = events
            .iter()
            .filter(|event| event.event_type == PaymentEventType::ChallengeCreated)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(conflict(
                "inbound issuance must contain exactly one created challenge event",
            ));
        }
        let event = matching[0];
        let challenge: PaymentChallenge = event
            .payload
            .get("challenge")
            .cloned()
            .ok_or_else(|| conflict("created challenge event has no complete challenge"))
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|_| conflict("created challenge event contains an invalid challenge"))
            })?;
        if event.intent_id != intent.id
            || event.attempt_no != 0
            || event.signature_ref.is_some()
            || event.tx_hash.is_some()
            || event.created_at != intent.created_at
            || challenge != *expected
            || !challenge_matches_intent(&challenge, intent, PaymentDirection::Inbound)
        {
            return Err(conflict(
                "created challenge event is not bound to the inbound payment intent",
            ));
        }
        Ok(())
    }

    fn challenge_matches_intent(
        challenge: &PaymentChallenge,
        intent: &PaymentIntent,
        direction: PaymentDirection,
    ) -> bool {
        intent.direction == direction
            && challenge.scenario_id == intent.scenario_id
            && challenge.offer_id == intent.offer_id
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

    #[async_trait]
    impl PaymentStorage for SqliteStorage {
        async fn find_run(&self, scenario_id: &str, run_id: &str) -> Result<Option<AuthorizationRecord>> {
            let row = sqlx::query("SELECT * FROM pay_intent WHERE scenario_id = ?1 AND run_id = ?2")
                .bind(scenario_id)
                .bind(run_id)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("find payment run", error))?;
            let Some(row) = row else { return Ok(None) };
            let intent = records::intent(&row)?;
            let approval = load_approval_pool(self.pool(), &intent.id).await?;
            Ok(Some(AuthorizationRecord { intent, approval }))
        }

        async fn authorize(&self, write: AuthorizationWrite<'_>) -> Result<IdempotentWrite<AuthorizationRecord>> {
            let intent = write.intent.clone();
            let approval = write.approval.cloned();
            let action = write.action;
            let guard = write.guard.clone();
            let events = write.events.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    if let Some(existing) = load_run_connection(connection, &intent.scenario_id, &intent.run_id).await?
                    {
                        if existing.intent.request_hash != intent.request_hash {
                            return Err(conflict("payment run_id is bound to a different request"));
                        }
                        return Ok(IdempotentWrite::Replay(existing));
                    }
                    validate_authorization_guard(connection, &intent, &guard, action).await?;
                    validate_challenge_received_event(&events, &intent)?;
                    match action {
                        AuthorizationAction::RecordDenied => {
                            if approval.is_some()
                                || intent.status != PaymentStatus::Failed
                                || intent.reservation_status != ReservationStatus::Unreserved
                            {
                                return Err(conflict("denied authorization aggregate has invalid lifecycle"));
                            }
                        }
                        AuthorizationAction::RequestApproval => {
                            if approval.as_ref().is_none_or(|request| request.intent_id != intent.id)
                                || intent.status != PaymentStatus::PendingApproval
                                || intent.reservation_status != ReservationStatus::Unreserved
                            {
                                return Err(conflict("approval authorization aggregate has invalid lifecycle"));
                            }
                        }
                        AuthorizationAction::Reserve => {
                            if approval.is_some()
                                || intent.status != PaymentStatus::Authorized
                                || intent.reservation_status != ReservationStatus::Reserved
                                || intent.reserved_amount != intent.amount
                            {
                                return Err(conflict("reserved authorization aggregate has invalid lifecycle"));
                            }
                            enforce_budget(connection, &intent, guard.day_started_at, guard.evaluated_at, false)
                                .await?;
                        }
                    }
                    insert_intent(connection, &intent, None).await?;
                    if let Some(request) = &approval {
                        insert_approval_request(connection, request).await?;
                    }
                    append_side_effects(connection, &events, &outbox).await?;
                    Ok(IdempotentWrite::Inserted(AuthorizationRecord { intent, approval }))
                })
            })
            .await
        }

        async fn budget_snapshot(
            &self,
            scenario_id: &str,
            wallet_id: &str,
            direction: PaymentDirection,
            budget_unit: &BudgetUnit,
            day_started_at: i64,
            now: i64,
        ) -> Result<BudgetSnapshot> {
            if day_started_at < 0 || now < day_started_at {
                return Err(Error::InvalidRequest("invalid budget time window".to_owned()));
            }
            let mut connection = self
                .pool()
                .acquire()
                .await
                .map_err(|error| storage_error("acquire budget read connection", error))?;
            budget_snapshot_connection(
                &mut connection,
                scenario_id,
                wallet_id,
                direction,
                budget_unit,
                day_started_at,
                now,
            )
            .await
        }

        async fn record_challenge(&self, write: ChallengeWrite<'_>) -> Result<IdempotentWrite<PaymentIntent>> {
            let intent = write.intent.clone();
            let challenge = write.challenge.clone();
            let guard = write.guard.clone();
            let events = write.events.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    if let Some(existing) = load_intent_by_run(connection, &intent.scenario_id, &intent.run_id).await? {
                        if existing.request_hash == intent.request_hash {
                            return Ok(IdempotentWrite::Replay(existing));
                        }
                        return Err(conflict("payment run_id is bound to a different request"));
                    }
                    validate_challenge_guard(connection, &intent, &challenge, &guard).await?;
                    validate_challenge_created_event(&events, &intent, &challenge)?;
                    enforce_budget(connection, &intent, guard.day_started_at, guard.evaluated_at, false).await?;
                    insert_intent(connection, &intent, None).await?;
                    append_side_effects(connection, &events, &outbox).await?;
                    Ok(IdempotentWrite::Inserted(intent))
                })
            })
            .await
        }

        async fn get_for_challenge(&self, scenario_id: &str, challenge_hash: &str) -> Result<Option<PaymentIntent>> {
            let row = sqlx::query(
                "SELECT * FROM pay_intent WHERE scenario_id = ?1 AND challenge_hash = ?2 AND direction = 'inbound'",
            )
            .bind(scenario_id)
            .bind(challenge_hash)
            .fetch_optional(self.pool())
            .await
            .map_err(|error| storage_error("get inbound payment challenge", error))?;
            row.as_ref().map(records::intent).transpose()
        }

        async fn verify_incoming(&self, write: VerificationWrite<'_>) -> Result<PaymentIntent> {
            let intent = write.intent.clone();
            let credential = write.credential.clone();
            let expected_updated_at = write.expected_updated_at;
            let events = write.events.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    bind_credential_transition(connection, &intent, &credential, expected_updated_at).await?;
                    append_side_effects(connection, &events, &outbox).await?;
                    Ok(intent)
                })
            })
            .await
        }

        async fn get(&self, intent_id: &str) -> Result<Option<PaymentIntent>> {
            let row = sqlx::query("SELECT * FROM pay_intent WHERE id = ?1")
                .bind(intent_id)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("get payment intent", error))?;
            row.as_ref().map(records::intent).transpose()
        }

        async fn get_for_scenario(&self, scenario_id: &str, intent_id: &str) -> Result<Option<PaymentIntent>> {
            let row = sqlx::query("SELECT * FROM pay_intent WHERE id = ?1 AND scenario_id = ?2")
                .bind(intent_id)
                .bind(scenario_id)
                .fetch_optional(self.pool())
                .await
                .map_err(|error| storage_error("get scenario payment intent", error))?;
            row.as_ref().map(records::intent).transpose()
        }

        async fn commit_progress(&self, write: PaymentProgressWrite<'_>) -> Result<PaymentIntent> {
            let intent = write.intent.clone();
            let expected_updated_at = write.expected_updated_at;
            let events = write.events.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    let existing = load_intent_connection(connection, &intent.id)
                        .await?
                        .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
                    let mutation = validate_progress_transition(&existing, &intent, expected_updated_at, &events)?;
                    update_intent(connection, &intent, expected_updated_at, mutation).await?;
                    append_side_effects(connection, &events, &outbox).await?;
                    Ok(intent)
                })
            })
            .await
        }

        async fn record_outbound_evidence(
            &self,
            write: OutboundEvidenceWrite<'_>,
        ) -> Result<IdempotentWrite<PaymentIntent>> {
            let intent = write.intent.clone();
            let expected_updated_at = write.expected_updated_at;
            let evidence_hash = write.evidence_hash.to_owned();
            let event = write.event.clone();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    let existing = load_intent_connection(connection, &intent.id)
                        .await?
                        .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
                    if let Some(stored_event) = load_outbound_evidence_event(connection, &intent.id).await? {
                        if crate::storage::outbound_evidence_replay_matches(&stored_event, &intent.id, &evidence_hash) {
                            return Ok(IdempotentWrite::Replay(existing));
                        }
                        return Err(conflict("outbound merchant evidence has different content"));
                    }
                    crate::storage::validate_outbound_evidence_transition(
                        &existing,
                        &intent,
                        expected_updated_at,
                        &event,
                        &evidence_hash,
                    )?;
                    update_intent(connection, &intent, expected_updated_at, ReservationMutation::Unchanged).await?;
                    append_side_effects(connection, std::slice::from_ref(&event), &outbox).await?;
                    Ok(IdempotentWrite::Inserted(intent))
                })
            })
            .await
        }

        async fn list(&self, query: &PaymentQuery) -> Result<Page<PaymentIntent>> {
            let direction = query.direction.map(PaymentDirection::as_str);
            let status = query.status.map(PaymentStatus::as_str);
            let predicate = "(?1 IS NULL OR scenario_id = ?1) AND (?2 IS NULL OR wallet_id = ?2) \
                AND (?3 IS NULL OR run_id = ?3) AND (?4 IS NULL OR direction = ?4) \
                AND (?5 IS NULL OR status = ?5) AND (?6 IS NULL OR created_at >= ?6) \
                AND (?7 IS NULL OR created_at <= ?7)";
            let count_sql = format!("SELECT count(*) FROM pay_intent WHERE {predicate}");
            let total: i64 = sqlx::query_scalar(&count_sql)
                .bind(query.scenario_id.as_deref())
                .bind(query.wallet_id.as_deref())
                .bind(query.run_id.as_deref())
                .bind(direction)
                .bind(status)
                .bind(query.from_created_at)
                .bind(query.to_created_at)
                .fetch_one(self.pool())
                .await
                .map_err(|error| storage_error("count payment intents", error))?;
            let (limit, offset) = super::codec::page_bounds(query.page)?;
            let select_sql =
                format!("SELECT * FROM pay_intent WHERE {predicate} ORDER BY created_at DESC, id LIMIT ?8 OFFSET ?9");
            let rows = sqlx::query(&select_sql)
                .bind(query.scenario_id.as_deref())
                .bind(query.wallet_id.as_deref())
                .bind(query.run_id.as_deref())
                .bind(direction)
                .bind(status)
                .bind(query.from_created_at)
                .bind(query.to_created_at)
                .bind(limit)
                .bind(offset)
                .fetch_all(self.pool())
                .await
                .map_err(|error| storage_error("list payment intents", error))?;
            Ok(Page {
                items: rows.iter().map(records::intent).collect::<Result<Vec<_>>>()?,
                total: u64::try_from(total).map_err(|_| Error::Storage("negative payment intent count".to_owned()))?,
            })
        }

        async fn claim_reconciliation(&self, request: &ReconciliationLeaseRequest) -> Result<Vec<PaymentIntent>> {
            if request.owner.trim().is_empty() || request.lease_until <= request.now || request.limit == 0 {
                return Err(Error::InvalidRequest("invalid reconciliation lease request".to_owned()));
            }
            let request = request.clone();
            self.immediate(|connection| {
                Box::pin(async move {
                    let ids: Vec<String> = sqlx::query_scalar(
                        "SELECT id FROM pay_intent WHERE ( \
                           (status IN ('signed', 'submitted', 'pending_settlement', 'unknown') \
                            AND next_reconcile_at IS NOT NULL AND next_reconcile_at <= ?1) \
                           OR (status = 'created' AND expires_at IS NOT NULL AND expires_at <= ?1) \
                         ) \
                         AND (lease_expires_at IS NULL OR lease_expires_at <= ?1) \
                         ORDER BY next_reconcile_at, created_at, id LIMIT ?2",
                    )
                    .bind(request.now)
                    .bind(i64::from(request.limit))
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| storage_error("select due payment reconciliation", error))?;
                    let mut intents = Vec::with_capacity(ids.len());
                    for id in ids {
                        let result = sqlx::query(
                            "UPDATE pay_intent SET lease_owner = ?1, lease_expires_at = ?2, \
                             reconcile_attempts = reconcile_attempts + 1 WHERE id = ?3 \
                             AND ( \
                               (status IN ('signed', 'submitted', 'pending_settlement', 'unknown') \
                                AND next_reconcile_at IS NOT NULL AND next_reconcile_at <= ?4) \
                               OR (status = 'created' AND expires_at IS NOT NULL AND expires_at <= ?4) \
                             ) AND (lease_expires_at IS NULL OR lease_expires_at <= ?4) \
                             AND reconcile_attempts < 4294967295",
                        )
                        .bind(&request.owner)
                        .bind(request.lease_until)
                        .bind(&id)
                        .bind(request.now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("claim payment reconciliation", error))?;
                        if result.rows_affected() != 1 {
                            return Err(conflict("payment reconciliation claim lost its conditional update"));
                        }
                        intents.push(
                            load_intent_connection(connection, &id)
                                .await?
                                .ok_or_else(|| Error::Storage("claimed payment intent disappeared".to_owned()))?,
                        );
                    }
                    Ok(intents)
                })
            })
            .await
        }

        async fn renew_reconciliation_lease(&self, intent_id: &str, owner: &str, lease_until: i64) -> Result<()> {
            update_reconciliation_lease(self, intent_id, owner, Some(lease_until), None).await
        }

        async fn release_reconciliation_lease(&self, intent_id: &str, owner: &str, next_attempt_at: i64) -> Result<()> {
            update_reconciliation_lease(self, intent_id, owner, None, Some(next_attempt_at)).await
        }

        async fn commit_settlement(&self, write: SettlementWrite<'_>) -> Result<PaymentIntent> {
            let intent = write.intent.clone();
            let expected_updated_at = write.expected_updated_at;
            let events = write.events.to_vec();
            let entries = write.ledger_entries.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    let existing = load_intent_connection(connection, &intent.id)
                        .await?
                        .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
                    validate_settlement_transition(&existing, &intent, expected_updated_at, &entries)?;
                    for entry in &entries {
                        insert_ledger(connection, entry).await?;
                    }
                    update_intent(
                        connection,
                        &intent,
                        expected_updated_at,
                        ReservationMutation::Settlement,
                    )
                    .await?;
                    append_side_effects(connection, &events, &outbox).await?;
                    Ok(intent)
                })
            })
            .await
        }
    }

    #[derive(Clone, Copy)]
    pub(super) enum ReservationMutation {
        Unchanged,
        ReleaseOnly,
        Settlement,
        ApprovalReserve,
    }

    pub(super) async fn load_intent_connection(
        connection: &mut SqliteConnection,
        intent_id: &str,
    ) -> Result<Option<PaymentIntent>> {
        let row = sqlx::query("SELECT * FROM pay_intent WHERE id = ?1")
            .bind(intent_id)
            .fetch_optional(connection)
            .await
            .map_err(|error| storage_error("get payment intent", error))?;
        row.as_ref().map(records::intent).transpose()
    }

    async fn load_intent_by_run(
        connection: &mut SqliteConnection,
        scenario_id: &str,
        run_id: &str,
    ) -> Result<Option<PaymentIntent>> {
        let row = sqlx::query("SELECT * FROM pay_intent WHERE scenario_id = ?1 AND run_id = ?2")
            .bind(scenario_id)
            .bind(run_id)
            .fetch_optional(connection)
            .await
            .map_err(|error| storage_error("find payment intent run", error))?;
        row.as_ref().map(records::intent).transpose()
    }

    async fn load_run_connection(
        connection: &mut SqliteConnection,
        scenario_id: &str,
        run_id: &str,
    ) -> Result<Option<AuthorizationRecord>> {
        let Some(intent) = load_intent_by_run(connection, scenario_id, run_id).await? else {
            return Ok(None);
        };
        let row = sqlx::query("SELECT * FROM pay_approval WHERE intent_id = ?1 ORDER BY created_at DESC, id LIMIT 1")
            .bind(&intent.id)
            .fetch_optional(connection)
            .await
            .map_err(|error| storage_error("load payment approval replay", error))?;
        let approval = row.as_ref().map(records::approval).transpose()?;
        Ok(Some(AuthorizationRecord { intent, approval }))
    }

    async fn load_approval_pool(pool: &sqlx::SqlitePool, intent_id: &str) -> Result<Option<ApprovalRequest>> {
        let row = sqlx::query("SELECT * FROM pay_approval WHERE intent_id = ?1 ORDER BY created_at DESC, id LIMIT 1")
            .bind(intent_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| storage_error("load payment intent approval", error))?;
        row.as_ref().map(records::approval).transpose()
    }

    pub(super) async fn insert_intent(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        credential_hash: Option<&str>,
    ) -> Result<()> {
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        validate_intent_authority(connection, intent).await?;
        sqlx::query(
            "INSERT INTO pay_intent (id, scenario_id, run_id, wallet_id, policy_id, offer_id, direction, protocol, \
             network, asset, token_contract, asset_decimals, recipient, amount, settled_amount, resource, \
             challenge_hash, credential_hash, request_hash, status, reservation_status, \
             reserved_amount, tx_hash, settlement_ref, next_reconcile_at, reconcile_attempts, lease_owner, \
             lease_expires_at, created_at, updated_at, expires_at, settled_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
                     ?19, ?20, ?21, ?22, ?23, ?24, ?25, 0, NULL, NULL, ?26, ?27, ?28, ?29)",
        )
        .bind(&intent.id)
        .bind(&intent.scenario_id)
        .bind(&intent.run_id)
        .bind(&intent.wallet_id)
        .bind(&intent.policy_id)
        .bind(&intent.offer_id)
        .bind(intent.direction.as_str())
        .bind(&intent.protocol)
        .bind(&intent.asset.network)
        .bind(&intent.asset.asset)
        .bind(&intent.asset.token_contract)
        .bind(i64::from(intent.asset.decimals))
        .bind(&intent.recipient)
        .bind(intent.amount.to_string())
        .bind(intent.settled_amount.to_string())
        .bind(&intent.resource)
        .bind(&intent.challenge_hash)
        .bind(credential_hash)
        .bind(&intent.request_hash)
        .bind(intent.status.as_str())
        .bind(intent.reservation_status.as_str())
        .bind(intent.reserved_amount.to_string())
        .bind(&intent.tx_hash)
        .bind(&intent.settlement_ref)
        .bind(intent.next_reconcile_at)
        .bind(intent.created_at)
        .bind(intent.updated_at)
        .bind(intent.expires_at)
        .bind(intent.settled_at)
        .execute(connection)
        .await
        .map_err(|error| conflict(format!("payment intent insert rejected: {error}")))?;
        Ok(())
    }

    async fn validate_intent_authority(connection: &mut SqliteConnection, intent: &PaymentIntent) -> Result<()> {
        if intent.status == PaymentStatus::Failed {
            return Ok(());
        }
        let (policy_id, wallet_id) = intent
            .authority_ids()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let policy = load_policy_connection(connection, policy_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment policy {policy_id}")))?;
        let limit = policy
            .limits_for(intent.direction)
            .ok_or_else(|| conflict("policy does not authorize payment intent direction"))?;
        if policy.scenario_id != intent.scenario_id
            || !policy.wallet_ids.iter().any(|allowed| allowed == wallet_id)
            || !policy.budget_unit.matches(&intent.asset)
            || !policy.protocols.contains(&intent.protocol)
            || !policy.networks.contains(&intent.asset.network)
            || !policy.assets.contains(&intent.asset)
            || intent.amount > limit.max_per_payment
        {
            return Err(conflict(
                "payment intent exceeds or falls outside its directional policy scope",
            ));
        }
        Ok(())
    }

    pub(super) async fn insert_approval_request(
        connection: &mut SqliteConnection,
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
            .map(|material| super::codec::encode_json(material, "pay_approval.sealed_grant_token"))
            .transpose()?;
        sqlx::query(
            "INSERT INTO pay_approval (id, intent_id, run_id, requested_amount, approved_amount, requested_by, approved_by, \
             status, intent_hash, grant_jti, grant_claims_hash, sealed_grant_token, expires_at, consumed_at, \
             created_at, approved_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )
        .bind(&approval.id)
        .bind(&approval.intent_id)
        .bind(&approval.run_id)
        .bind(approval.requested_amount.to_string())
        .bind(approval.approved_amount.map(|amount| amount.to_string()))
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
        .map_err(|error| conflict(format!("payment approval insert rejected: {error}")))?;
        Ok(())
    }

    pub(super) async fn append_side_effects(
        connection: &mut SqliteConnection,
        events: &[crate::model::PaymentEvent],
        outbox: &[crate::storage::OutboxMessage],
    ) -> Result<()> {
        for event in events {
            insert_event(connection, event).await?;
        }
        for message in outbox {
            insert_outbox(connection, message).await?;
        }
        Ok(())
    }

    pub(super) async fn update_intent(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        expected_updated_at: i64,
        reservation_mutation: ReservationMutation,
    ) -> Result<()> {
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let existing = load_intent_connection(connection, &intent.id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
        if existing.updated_at != expected_updated_at
            || intent.updated_at <= expected_updated_at
            || !existing.same_immutable_identity_as(intent)
            || intent.settled_amount < existing.settled_amount
            || !allowed_status_transition(existing.status, intent.status)
        {
            return Err(conflict("payment intent identity, lifecycle, or revision changed"));
        }
        match reservation_mutation {
            ReservationMutation::Unchanged
                if intent.reservation_status != existing.reservation_status
                    || intent.reserved_amount != existing.reserved_amount =>
            {
                return Err(conflict("generic payment update cannot change a reservation"));
            }
            ReservationMutation::ReleaseOnly
                if existing.reservation_status != ReservationStatus::Reserved
                    || intent.reservation_status != ReservationStatus::Released
                    || intent.reserved_amount != existing.reserved_amount =>
            {
                return Err(conflict("reservation update must release one existing reservation"));
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
                return Err(conflict("settlement has an invalid reservation transition"));
            }
            ReservationMutation::ApprovalReserve
                if existing.reservation_status != ReservationStatus::Unreserved
                    || intent.reservation_status != ReservationStatus::Reserved
                    || intent.reserved_amount != intent.amount =>
            {
                return Err(conflict("approved payment must create exactly one full reservation"));
            }
            _ => {}
        }
        let result = sqlx::query(
            "UPDATE pay_intent SET settled_amount = ?1, status = ?2, reservation_status = ?3, reserved_amount = ?4, \
             tx_hash = ?5, settlement_ref = ?6, next_reconcile_at = ?7, updated_at = ?8, settled_at = ?9 \
             WHERE id = ?10 AND updated_at = ?11",
        )
        .bind(intent.settled_amount.to_string())
        .bind(intent.status.as_str())
        .bind(intent.reservation_status.as_str())
        .bind(intent.reserved_amount.to_string())
        .bind(&intent.tx_hash)
        .bind(&intent.settlement_ref)
        .bind(intent.next_reconcile_at)
        .bind(intent.updated_at)
        .bind(intent.settled_at)
        .bind(&intent.id)
        .bind(expected_updated_at)
        .execute(connection)
        .await
        .map_err(|error| conflict(format!("payment intent update rejected: {error}")))?;
        if result.rows_affected() != 1 {
            return Err(conflict("payment intent revision changed"));
        }
        Ok(())
    }

    async fn bind_credential_transition(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
        expected_updated_at: i64,
    ) -> Result<()> {
        if credential.credential_hash.trim().is_empty()
            || credential.protocol != intent.protocol
            || intent.challenge_hash.as_deref() != Some(credential.challenge_hash.as_str())
            || intent.direction != PaymentDirection::Inbound
        {
            return Err(conflict("payment credential is not bound to the inbound challenge"));
        }
        let existing_hash: Option<String> = sqlx::query_scalar("SELECT credential_hash FROM pay_intent WHERE id = ?1")
            .bind(&intent.id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("load payment credential binding", error))?
            .flatten();
        if existing_hash.is_some() {
            return Err(conflict("payment credential is already bound"));
        }
        update_intent(connection, intent, expected_updated_at, ReservationMutation::Unchanged).await?;
        let result =
            sqlx::query("UPDATE pay_intent SET credential_hash = ?1 WHERE id = ?2 AND credential_hash IS NULL")
                .bind(&credential.credential_hash)
                .bind(&intent.id)
                .execute(connection)
                .await
                .map_err(|error| conflict(format!("payment credential binding rejected: {error}")))?;
        if result.rows_affected() != 1 {
            return Err(conflict("payment credential binding lost its conditional update"));
        }
        Ok(())
    }

    fn allowed_status_transition(from: PaymentStatus, to: PaymentStatus) -> bool {
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

    async fn validate_authorization_guard(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        guard: &AuthorizationGuard,
        action: AuthorizationAction,
    ) -> Result<()> {
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let (_, wallet_id) = intent
            .authority_ids()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        if intent.direction != PaymentDirection::Outbound
            || intent.scenario_id != guard.scenario.id
            || intent.policy_id.as_deref() != Some(guard.policy.id.as_str())
            || guard.scenario.client_id != guard.client.id
            || !guard.policy.wallet_ids.iter().any(|candidate| candidate == wallet_id)
        {
            return Err(conflict("authorization guard is not bound to the payment intent"));
        }
        let client_row = sqlx::query("SELECT * FROM pay_client WHERE id = ?1 AND auth_principal = ?2")
            .bind(&guard.client.id)
            .bind(&guard.principal)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("reload authorization client", error))?
            .ok_or(Error::Forbidden)?;
        let client = records::client(&client_row)?;
        let scenario_row = sqlx::query("SELECT * FROM pay_scenario WHERE id = ?1 AND client_id = ?2")
            .bind(&guard.scenario.id)
            .bind(&guard.client.id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("reload authorization scenario", error))?
            .ok_or(Error::Forbidden)?;
        let scenario = records::scenario(&scenario_row)?;
        let policy = load_policy_connection(connection, &guard.policy.id)
            .await?
            .ok_or_else(|| conflict("authorization policy no longer exists"))?;
        let wallet = load_payment_wallet_connection(connection, wallet_id)
            .await?
            .ok_or_else(|| conflict("authorization wallet no longer exists"))?;
        if client != guard.client || scenario != guard.scenario || policy != guard.policy {
            return Err(conflict("authorization snapshot changed before commit"));
        }
        if action != AuthorizationAction::RecordDenied {
            if !client.authenticates(&guard.principal)
                || !scenario.enabled
                || !policy.enabled
                || !wallet.signing_ready_for_payment_network(&intent.asset.network)
                || wallet.updated_at != guard.expected_wallet_updated_at
                || wallet.active_key_version != guard.expected_wallet_key_version
                || wallet.budget_unit.as_ref() != Some(&policy.budget_unit)
                || !policy.allows_wallet(wallet_id)
                || !policy.allows_direction(intent.direction)
                || !policy.allows_protocol(&intent.protocol)
                || !policy.allows_asset(&intent.asset)
                || !policy.allows_payment_recipient(&intent.asset.network, &intent.recipient)
                || !policy.allows_origin(&guard.origin)
                || intent
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= guard.evaluated_at)
            {
                return Err(Error::PolicyDenied(
                    "payment authorization changed or falls outside current policy".to_owned(),
                ));
            }
            let limits = policy
                .limits_for(intent.direction)
                .ok_or_else(|| Error::PolicyDenied("payment direction is disabled".to_owned()))?;
            if intent.amount > limits.max_per_payment {
                return Err(Error::PolicyDenied(
                    "payment exceeds the per-payment policy limit".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn validate_current_payment_scope(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        origin: Option<&str>,
    ) -> Result<()> {
        intent
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let (policy_id, wallet_id) = intent
            .authority_ids()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let scenario_row = sqlx::query(
            "SELECT s.*, c.enabled AS client_enabled FROM pay_scenario s \
             JOIN pay_client c ON c.id = s.client_id WHERE s.id = ?1",
        )
        .bind(&intent.scenario_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| storage_error("reload payment scenario lifecycle", error))?
        .ok_or_else(|| conflict("payment scenario no longer exists"))?;
        let scenario = records::scenario(&scenario_row)?;
        let client_enabled: i64 = scenario_row
            .try_get("client_enabled")
            .map_err(|error| storage_error("decode payment client lifecycle", error))?;
        let policy = load_policy_connection(connection, policy_id)
            .await?
            .ok_or_else(|| conflict("payment policy no longer exists"))?;
        let wallet = load_payment_wallet_connection(connection, wallet_id)
            .await?
            .ok_or_else(|| conflict("payment wallet no longer exists"))?;
        if !scenario.enabled
            || client_enabled != 1
            || !policy.enabled
            || policy.scenario_id != intent.scenario_id
            || !policy.allows_wallet(wallet_id)
            || !policy.allows_direction(intent.direction)
            || !policy.allows_protocol(&intent.protocol)
            || !policy.allows_asset(&intent.asset)
            || wallet.budget_unit.as_ref() != Some(&policy.budget_unit)
            || !wallet.enabled
            || (intent.direction == PaymentDirection::Outbound
                && !wallet.signing_ready_for_payment_network(&intent.asset.network))
            || (intent.direction == PaymentDirection::Outbound
                && !policy.allows_payment_recipient(&intent.asset.network, &intent.recipient))
            || origin.is_some_and(|origin| !policy.allows_origin(origin))
        {
            return Err(Error::PolicyDenied("payment scope is no longer authorized".to_owned()));
        }
        Ok(())
    }

    async fn validate_challenge_guard(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        challenge: &PaymentChallenge,
        guard: &ChallengeIssuanceGuard,
    ) -> Result<()> {
        if intent.direction != PaymentDirection::Inbound
            || intent.status != PaymentStatus::Created
            || intent.reservation_status != ReservationStatus::Reserved
            || intent.reserved_amount != intent.amount
            || intent.offer_id.as_deref() != Some(guard.offer.id.as_str())
            || intent.policy_id.as_deref() != Some(guard.policy.id.as_str())
            || intent.wallet_id.as_deref() != Some(guard.wallet.wallet.id.as_str())
            || intent.challenge_hash.as_deref() != Some(challenge.challenge_hash.as_str())
            || challenge.scenario_id != intent.scenario_id
            || challenge.offer_id != intent.offer_id
            || challenge.resource != intent.resource
            || challenge.protocol != intent.protocol
            || challenge.asset != intent.asset
            || challenge.amount != intent.amount
            || challenge.recipient != intent.recipient
            || challenge.expires_at != intent.expires_at.unwrap_or_default()
            || challenge.expires_at <= guard.evaluated_at
        {
            return Err(conflict("inbound challenge aggregate is not internally bound"));
        }
        let client_row = sqlx::query("SELECT * FROM pay_client WHERE id = ?1 AND auth_principal = ?2")
            .bind(&guard.client.id)
            .bind(&guard.principal)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("reload challenge client", error))?
            .ok_or(Error::Forbidden)?;
        let client = records::client(&client_row)?;
        let scenario_row = sqlx::query("SELECT * FROM pay_scenario WHERE id = ?1 AND client_id = ?2")
            .bind(&guard.scenario.id)
            .bind(&guard.client.id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("reload challenge scenario", error))?
            .ok_or(Error::Forbidden)?;
        let scenario = records::scenario(&scenario_row)?;
        let offer_row = sqlx::query("SELECT * FROM pay_offer WHERE id = ?1")
            .bind(&guard.offer.id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("reload challenge offer", error))?
            .ok_or_else(|| conflict("payment offer no longer exists"))?;
        let offer = records::offer(&offer_row)?;
        let policy = load_policy_connection(connection, &guard.policy.id)
            .await?
            .ok_or_else(|| conflict("payment policy no longer exists"))?;
        let wallet = load_payment_wallet_connection(connection, &guard.wallet.wallet.id)
            .await?
            .ok_or_else(|| conflict("payment wallet no longer exists"))?;
        let canonical_wallet = wallet
            .clone()
            .canonicalized_for_payment_network(&intent.asset.network)
            .map_err(|_| Error::PolicyDenied("offer recipient wallet is invalid for its network".to_owned()))?;
        if client != guard.client
            || scenario != guard.scenario
            || offer != guard.offer
            || policy != guard.policy
            || wallet != guard.wallet
            || !client.authenticates(&guard.principal)
            || !scenario.enabled
            || !offer.enabled
            || !policy.enabled
            || !wallet.enabled
            || policy.scenario_id != scenario.id
            || offer.scenario_id != scenario.id
            || offer.policy_id != policy.id
            || offer.wallet_id != wallet.wallet.id
            || !policy.allows_wallet(&wallet.wallet.id)
            || !policy.allows_direction(PaymentDirection::Inbound)
            || !policy.allows_protocol(&intent.protocol)
            || !policy.allows_asset(&intent.asset)
            || !policy.allows_origin(&guard.origin)
            || wallet.budget_unit.as_ref() != Some(&policy.budget_unit)
            || canonical_wallet.wallet.address != intent.recipient
        {
            return Err(Error::PolicyDenied(
                "inbound challenge changed or falls outside current policy".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn enforce_budget(
        connection: &mut SqliteConnection,
        intent: &PaymentIntent,
        day_started_at: i64,
        now: i64,
        approved_overage: bool,
    ) -> Result<()> {
        let (policy_id, wallet_id) = intent
            .authority_ids()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
        let policy = load_policy_connection(connection, policy_id)
            .await?
            .ok_or_else(|| conflict("payment policy no longer exists"))?;
        let limits = policy
            .limits_for(intent.direction)
            .ok_or_else(|| Error::PolicyDenied("payment direction is disabled".to_owned()))?;
        if intent.amount > limits.max_per_payment {
            return Err(Error::PolicyDenied(
                "payment exceeds the per-payment policy limit".to_owned(),
            ));
        }
        let snapshot = budget_snapshot_connection(
            connection,
            &intent.scenario_id,
            wallet_id,
            intent.direction,
            &intent.asset.budget_unit(),
            day_started_at,
            now,
        )
        .await?;
        let policy_limit = if approved_overage {
            limits
                .hard_daily_limit()
                .ok_or_else(|| Error::PolicyDenied("approved policy limit overflows".to_owned()))?
        } else {
            limits.max_per_day
        };
        let scenario_after = snapshot
            .scenario_spent_and_reserved
            .checked_add(intent.amount)
            .ok_or_else(|| Error::PolicyDenied("scenario budget arithmetic overflow".to_owned()))?;
        if scenario_after > policy_limit {
            return Err(Error::PolicyDenied(
                "scenario daily payment limit is exhausted".to_owned(),
            ));
        }
        if let Some(max_per_payment) = snapshot.wallet_max_per_payment {
            if intent.amount > max_per_payment {
                return Err(Error::PolicyDenied("wallet per-payment limit is exceeded".to_owned()));
            }
        } else if snapshot.wallet_shared_across_scenarios {
            return Err(Error::PolicyDenied(
                "shared wallet is missing its aggregate per-payment limit".to_owned(),
            ));
        }
        if let Some(max_per_day) = snapshot.wallet_max_per_day {
            let wallet_after = snapshot
                .wallet_spent_and_reserved
                .checked_add(intent.amount)
                .ok_or_else(|| Error::PolicyDenied("wallet budget arithmetic overflow".to_owned()))?;
            if wallet_after > max_per_day {
                return Err(Error::PolicyDenied(
                    "wallet daily payment limit is exhausted".to_owned(),
                ));
            }
        } else if snapshot.wallet_shared_across_scenarios {
            return Err(Error::PolicyDenied(
                "shared wallet is missing its aggregate daily limit".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn budget_snapshot_connection(
        connection: &mut SqliteConnection,
        scenario_id: &str,
        wallet_id: &str,
        direction: PaymentDirection,
        budget_unit: &BudgetUnit,
        day_started_at: i64,
        now: i64,
    ) -> Result<BudgetSnapshot> {
        budget_unit
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid budget unit {field}")))?;
        if day_started_at < 0 || now < day_started_at {
            return Err(Error::InvalidRequest("invalid budget time window".to_owned()));
        }
        let wallet = load_payment_wallet_connection(connection, wallet_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment wallet {wallet_id}")))?;
        if wallet.budget_unit.as_ref() != Some(budget_unit) {
            return Err(conflict("wallet budget unit does not match requested aggregation"));
        }
        let shared_count: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT p.scenario_id) FROM pay_policy_wallet m \
             JOIN pay_policy p ON p.id = m.policy_id WHERE m.wallet_id = ?1 AND p.enabled = 1",
        )
        .bind(wallet_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| storage_error("count wallet scenario sharing", error))?;

        let latest_effective_at = now.saturating_add(MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS);
        assert_no_ledger_beyond_future_skew(
            connection,
            scenario_id,
            wallet_id,
            direction,
            budget_unit,
            latest_effective_at,
        )
        .await?;
        assert_consumed_intents_have_ledger(connection, scenario_id, wallet_id, direction, budget_unit).await?;

        let scenario_reserved = load_amounts(
            connection,
            "SELECT reserved_amount FROM pay_intent WHERE scenario_id = ?1 AND direction = ?2 \
             AND reservation_status = 'reserved' AND asset = ?3 AND asset_decimals = ?4",
            scenario_id,
            direction,
            budget_unit,
        )
        .await?;
        let wallet_reserved = load_amounts(
            connection,
            "SELECT reserved_amount FROM pay_intent WHERE wallet_id = ?1 AND direction = ?2 \
             AND reservation_status = 'reserved' AND asset = ?3 AND asset_decimals = ?4",
            wallet_id,
            direction,
            budget_unit,
        )
        .await?;
        let scenario_confirmed = load_ledger_amounts(
            connection,
            "SELECT amount FROM pay_ledger WHERE scenario_id = ?1 AND direction = ?2 \
             AND asset = ?3 AND asset_decimals = ?4 AND effective_at >= ?5 AND effective_at <= ?6",
            scenario_id,
            direction,
            budget_unit,
            day_started_at,
            latest_effective_at,
        )
        .await?;
        let wallet_confirmed = load_ledger_amounts(
            connection,
            "SELECT amount FROM pay_ledger WHERE wallet_id = ?1 AND direction = ?2 \
             AND asset = ?3 AND asset_decimals = ?4 AND effective_at >= ?5 AND effective_at <= ?6",
            wallet_id,
            direction,
            budget_unit,
            day_started_at,
            latest_effective_at,
        )
        .await?;

        Ok(BudgetSnapshot {
            budget_unit: budget_unit.clone(),
            wallet_enabled: wallet.enabled,
            wallet_algorithm: wallet.wallet.algorithm,
            wallet_address: wallet.wallet.address,
            wallet_updated_at: wallet.updated_at,
            wallet_active_key_version: wallet.active_key_version,
            wallet_shared_across_scenarios: shared_count > 1,
            scenario_spent_and_reserved: checked_usage(scenario_reserved, scenario_confirmed, "scenario")?,
            wallet_spent_and_reserved: checked_usage(wallet_reserved, wallet_confirmed, "wallet")?,
            wallet_max_per_payment: wallet.max_per_payment,
            wallet_max_per_day: wallet.max_per_day,
        })
    }

    async fn load_amounts(
        connection: &mut SqliteConnection,
        sql: &str,
        scope_id: &str,
        direction: PaymentDirection,
        unit: &BudgetUnit,
    ) -> Result<Amount> {
        let values: Vec<String> = sqlx::query_scalar(sql)
            .bind(scope_id)
            .bind(direction.as_str())
            .bind(&unit.asset)
            .bind(i64::from(unit.decimals))
            .fetch_all(connection)
            .await
            .map_err(|error| storage_error("load reserved payment amounts", error))?;
        sum_amounts(values)
    }

    async fn load_ledger_amounts(
        connection: &mut SqliteConnection,
        sql: &str,
        scope_id: &str,
        direction: PaymentDirection,
        unit: &BudgetUnit,
        day_started_at: i64,
        latest_effective_at: i64,
    ) -> Result<Amount> {
        let values: Vec<String> = sqlx::query_scalar(sql)
            .bind(scope_id)
            .bind(direction.as_str())
            .bind(&unit.asset)
            .bind(i64::from(unit.decimals))
            .bind(day_started_at)
            .bind(latest_effective_at)
            .fetch_all(connection)
            .await
            .map_err(|error| storage_error("load confirmed ledger amounts", error))?;
        sum_amounts(values)
    }

    fn sum_amounts(values: Vec<String>) -> Result<Amount> {
        values.into_iter().try_fold(Amount::ZERO, |sum, value| {
            let amount = value
                .parse::<Amount>()
                .map_err(|_| Error::Storage("SQLite contains an invalid monetary amount".to_owned()))?;
            sum.checked_add(amount)
                .ok_or_else(|| Error::Storage("authoritative payment aggregate overflowed u128".to_owned()))
        })
    }

    fn checked_usage(reserved: Amount, confirmed: Amount, scope: &str) -> Result<Amount> {
        reserved
            .checked_add(confirmed)
            .ok_or_else(|| Error::Storage(format!("{scope} payment usage overflowed u128")))
    }

    async fn assert_no_ledger_beyond_future_skew(
        connection: &mut SqliteConnection,
        scenario_id: &str,
        wallet_id: &str,
        direction: PaymentDirection,
        unit: &BudgetUnit,
        latest_effective_at: i64,
    ) -> Result<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pay_ledger WHERE (scenario_id = ?1 OR wallet_id = ?2) AND direction = ?3 \
             AND asset = ?4 AND asset_decimals = ?5 AND effective_at > ?6",
        )
        .bind(scenario_id)
        .bind(wallet_id)
        .bind(direction.as_str())
        .bind(&unit.asset)
        .bind(i64::from(unit.decimals))
        .bind(latest_effective_at)
        .fetch_one(connection)
        .await
        .map_err(|error| storage_error("verify ledger effective times", error))?;
        if count != 0 {
            return Err(Error::Storage(
                "ledger effective time exceeds the bounded future-skew window".to_owned(),
            ));
        }
        Ok(())
    }

    async fn assert_consumed_intents_have_ledger(
        connection: &mut SqliteConnection,
        scenario_id: &str,
        wallet_id: &str,
        direction: PaymentDirection,
        unit: &BudgetUnit,
    ) -> Result<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pay_intent i WHERE (i.scenario_id = ?1 OR i.wallet_id = ?2) \
             AND i.direction = ?3 AND i.asset = ?4 AND i.asset_decimals = ?5 \
             AND i.reservation_status = 'consumed' AND NOT EXISTS ( \
                 SELECT 1 FROM pay_ledger l WHERE l.intent_id = i.id AND l.scenario_id = i.scenario_id \
                 AND l.wallet_id = i.wallet_id AND l.direction = i.direction \
                 AND l.asset = i.asset AND l.asset_decimals = i.asset_decimals)",
        )
        .bind(scenario_id)
        .bind(wallet_id)
        .bind(direction.as_str())
        .bind(&unit.asset)
        .bind(i64::from(unit.decimals))
        .fetch_one(connection)
        .await
        .map_err(|error| storage_error("verify consumed payment ledger facts", error))?;
        if count != 0 {
            return Err(Error::Storage(
                "a consumed payment reservation has no matching durable ledger fact".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_settlement_transition(
        existing: &PaymentIntent,
        intent: &PaymentIntent,
        expected_updated_at: i64,
        entries: &[PaymentLedgerEntry],
    ) -> Result<()> {
        if existing.updated_at != expected_updated_at
            || !existing.same_immutable_identity_as(intent)
            || !allowed_status_transition(existing.status, intent.status)
        {
            return Err(conflict("settlement payment identity or revision changed"));
        }
        for entry in entries {
            if entry.intent_id.as_deref() != Some(intent.id.as_str())
                || entry.scenario_id.as_deref() != Some(intent.scenario_id.as_str())
                || Some(entry.wallet_id.as_str()) != intent.wallet_id.as_deref()
            {
                return Err(conflict("settlement ledger entry is not bound to its payment intent"));
            }
        }
        if existing.reservation_status == ReservationStatus::Reserved {
            if intent.reservation_status != ReservationStatus::Consumed {
                return Err(conflict("settlement must consume the existing reservation"));
            }
            let matching_fact = entries.iter().any(|entry| {
                entry.direction == intent.direction
                    && entry.asset.budget_unit() == intent.asset.budget_unit()
                    && entry.intent_id.as_deref() == Some(intent.id.as_str())
            });
            if !matching_fact {
                return Err(conflict(
                    "reservation consumption requires a matching confirmed ledger fact",
                ));
            }
        } else if intent.reservation_status == ReservationStatus::Consumed
            && existing.reservation_status != ReservationStatus::Consumed
        {
            return Err(conflict("an unreserved payment cannot become consumed"));
        }
        if intent.status == PaymentStatus::Settled
            && (intent.settled_at.is_none() || intent.settled_amount.is_zero() || entries.is_empty())
        {
            return Err(conflict("settled payment is missing confirmed settlement facts"));
        }
        Ok(())
    }

    fn validate_progress_transition(
        existing: &PaymentIntent,
        intent: &PaymentIntent,
        expected_updated_at: i64,
        events: &[PaymentEvent],
    ) -> Result<ReservationMutation> {
        if existing.updated_at != expected_updated_at
            || !existing.same_immutable_identity_as(intent)
            || !allowed_status_transition(existing.status, intent.status)
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
            return Err(conflict("non-financial payment progress is invalid"));
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
        Err(conflict(
            "non-financial payment progress cannot consume or replace a reservation",
        ))
    }

    async fn load_outbound_evidence_event(
        connection: &mut SqliteConnection,
        intent_id: &str,
    ) -> Result<Option<PaymentEvent>> {
        let row =
            sqlx::query("SELECT * FROM pay_event WHERE intent_id = ?1 AND event_type = 'submitted' AND attempt_no = 0")
                .bind(intent_id)
                .fetch_optional(connection)
                .await
                .map_err(|error| storage_error("load outbound merchant evidence", error))?;
        row.as_ref().map(records::event).transpose()
    }

    async fn update_reconciliation_lease(
        storage: &SqliteStorage,
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
            .immediate(|connection| {
                Box::pin(async move {
                    let result = if let Some(lease_until) = lease_until {
                        sqlx::query(
                            "UPDATE pay_intent SET lease_expires_at = ?1 WHERE id = ?2 AND lease_owner = ?3 \
                             AND status IN ('created', 'signed', 'submitted', 'pending_settlement', 'unknown')",
                        )
                        .bind(lease_until)
                        .bind(&intent_id)
                        .bind(&owner)
                        .execute(&mut *connection)
                        .await
                    } else {
                        sqlx::query(
                            "UPDATE pay_intent SET lease_owner = NULL, lease_expires_at = NULL, next_reconcile_at = ?1 \
                             WHERE id = ?2 AND lease_owner = ?3 \
                             AND status IN ('created', 'signed', 'submitted', 'pending_settlement', 'unknown')",
                        )
                        .bind(next_attempt_at)
                        .bind(&intent_id)
                        .bind(&owner)
                        .execute(&mut *connection)
                        .await
                    }
                    .map_err(|error| storage_error("update payment reconciliation lease", error))?;
                    if result.rows_affected() != 1 {
                        return Err(conflict("payment reconciliation lease is not held by this owner"));
                    }
                    Ok(())
                })
            })
            .await
    }
}

mod records {
    use sqlx::{sqlite::SqliteRow, Row as _};

    use crate::{
        model::{
            ApprovalRequest, ApprovalStatus, AssetIdentity, BudgetUnit, LedgerEntryType, PaymentClient,
            PaymentDirection, PaymentEvent, PaymentEventType, PaymentIntent, PaymentLedgerEntry, PaymentOffer,
            PaymentPolicy, PaymentPolicyLimits, PaymentScenario, PaymentScenarioApprover, PaymentStatus,
            ReservationStatus,
        },
        storage::{
            EncryptedWalletKeyVersion, OutboxMessage, OutboxStatus, PaymentWallet, WalletCustody, WalletKeyStatus,
        },
        utils::secure_material::SealedSensitiveMaterial,
        Error, Result,
    };

    use super::codec::{decode_amount, decode_bool, decode_json, decode_optional_amount, decode_u32, decode_u8};

    pub(super) fn client(row: &SqliteRow) -> Result<PaymentClient> {
        Ok(PaymentClient {
            id: field(row, "id")?,
            name: field(row, "name")?,
            auth_principal: field(row, "auth_principal")?,
            enabled: decode_bool(field(row, "enabled")?, "pay_client.enabled")?,
            created_at: field(row, "created_at")?,
            updated_at: field(row, "updated_at")?,
        })
    }

    pub(super) fn scenario(row: &SqliteRow) -> Result<PaymentScenario> {
        Ok(PaymentScenario {
            id: field(row, "id")?,
            client_id: field(row, "client_id")?,
            name: field(row, "name")?,
            enabled: decode_bool(field(row, "enabled")?, "pay_scenario.enabled")?,
            created_at: field(row, "created_at")?,
            updated_at: field(row, "updated_at")?,
        })
    }

    pub(super) fn approver(row: &SqliteRow) -> Result<PaymentScenarioApprover> {
        Ok(PaymentScenarioApprover {
            scenario_id: field(row, "scenario_id")?,
            auth_principal: field(row, "auth_principal")?,
            enabled: decode_bool(field(row, "enabled")?, "pay_scenario_approver.enabled")?,
            created_at: field(row, "created_at")?,
            updated_at: field(row, "updated_at")?,
        })
    }

    pub(super) fn policy(row: &SqliteRow, wallet_ids: Vec<String>) -> Result<PaymentPolicy> {
        let inbound = limits(row, "inbound")?;
        let outbound = limits(row, "outbound")?;
        Ok(PaymentPolicy {
            id: field(row, "id")?,
            scenario_id: field(row, "scenario_id")?,
            wallet_ids,
            protocols: decode_json(&field::<String>(row, "protocols")?, "pay_policy.protocols")?,
            networks: decode_json(&field::<String>(row, "networks")?, "pay_policy.networks")?,
            assets: decode_json(&field::<String>(row, "assets")?, "pay_policy.assets")?,
            budget_unit: BudgetUnit {
                asset: field(row, "budget_asset")?,
                decimals: decode_u8(field(row, "budget_decimals")?, "pay_policy.budget_decimals")?,
            },
            allowed_recipients: decode_json(
                &field::<String>(row, "allowed_recipients")?,
                "pay_policy.allowed_recipients",
            )?,
            allowed_origins: decode_json(&field::<String>(row, "allowed_origins")?, "pay_policy.allowed_origins")?,
            inbound_limits: inbound,
            outbound_limits: outbound,
            enabled: decode_bool(field(row, "enabled")?, "pay_policy.enabled")?,
            created_at: field(row, "created_at")?,
            updated_at: field(row, "updated_at")?,
        })
    }

    fn limits(row: &SqliteRow, prefix: &str) -> Result<Option<PaymentPolicyLimits>> {
        let max_per_payment = field::<Option<String>>(row, &format!("{prefix}_max_per_payment"))?;
        let Some(max_per_payment) = max_per_payment else {
            return Ok(None);
        };
        Ok(Some(PaymentPolicyLimits {
            max_per_payment: decode_amount(&max_per_payment, &format!("pay_policy.{prefix}_max_per_payment"))?,
            max_per_day: decode_amount(
                &field::<String>(row, &format!("{prefix}_max_per_day"))?,
                &format!("pay_policy.{prefix}_max_per_day"),
            )?,
            approval_threshold: decode_optional_amount(
                field(row, &format!("{prefix}_approval_threshold"))?,
                &format!("pay_policy.{prefix}_approval_threshold"),
            )?,
            approval_enabled: decode_bool(
                field(row, &format!("{prefix}_approval_enabled"))?,
                &format!("pay_policy.{prefix}_approval_enabled"),
            )?,
            approval_overage: decode_optional_amount(
                field(row, &format!("{prefix}_approval_overage"))?,
                &format!("pay_policy.{prefix}_approval_overage"),
            )?,
        }))
    }

    pub(super) fn offer(row: &SqliteRow) -> Result<PaymentOffer> {
        Ok(PaymentOffer {
            id: field(row, "id")?,
            scenario_id: field(row, "scenario_id")?,
            policy_id: field(row, "policy_id")?,
            resource: field(row, "resource")?,
            protocols: decode_json(&field::<String>(row, "protocols")?, "pay_offer.protocols")?,
            asset: asset(row)?,
            amount: decode_amount(&field::<String>(row, "amount")?, "pay_offer.amount")?,
            wallet_id: field(row, "wallet_id")?,
            enabled: decode_bool(field(row, "enabled")?, "pay_offer.enabled")?,
            created_at: field(row, "created_at")?,
            updated_at: field(row, "updated_at")?,
        })
    }

    pub(super) fn intent(row: &SqliteRow) -> Result<PaymentIntent> {
        Ok(PaymentIntent {
            id: field(row, "id")?,
            scenario_id: field(row, "scenario_id")?,
            run_id: field(row, "run_id")?,
            wallet_id: field(row, "wallet_id")?,
            policy_id: field(row, "policy_id")?,
            offer_id: field(row, "offer_id")?,
            direction: direction(&field::<String>(row, "direction")?)?,
            protocol: field(row, "protocol")?,
            asset: asset(row)?,
            recipient: field(row, "recipient")?,
            amount: decode_amount(&field::<String>(row, "amount")?, "pay_intent.amount")?,
            settled_amount: decode_amount(&field::<String>(row, "settled_amount")?, "pay_intent.settled_amount")?,
            resource: field(row, "resource")?,
            challenge_hash: field(row, "challenge_hash")?,
            request_hash: field(row, "request_hash")?,
            status: payment_status(&field::<String>(row, "status")?)?,
            reservation_status: reservation_status(&field::<String>(row, "reservation_status")?)?,
            reserved_amount: decode_amount(&field::<String>(row, "reserved_amount")?, "pay_intent.reserved_amount")?,
            tx_hash: field(row, "tx_hash")?,
            settlement_ref: field(row, "settlement_ref")?,
            next_reconcile_at: field(row, "next_reconcile_at")?,
            created_at: field(row, "created_at")?,
            updated_at: field(row, "updated_at")?,
            expires_at: field(row, "expires_at")?,
            settled_at: field(row, "settled_at")?,
        })
    }

    pub(super) fn event(row: &SqliteRow) -> Result<PaymentEvent> {
        Ok(PaymentEvent {
            id: field(row, "id")?,
            intent_id: field(row, "intent_id")?,
            event_type: event_type(&field::<String>(row, "event_type")?)?,
            attempt_no: decode_u32(field(row, "attempt_no")?, "pay_event.attempt_no")?,
            signature_ref: field(row, "signature_ref")?,
            tx_hash: field(row, "tx_hash")?,
            payload: decode_json(&field::<String>(row, "payload")?, "pay_event.payload")?,
            created_at: field(row, "created_at")?,
        })
    }

    pub(super) fn approval(row: &SqliteRow) -> Result<ApprovalRequest> {
        Ok(ApprovalRequest {
            id: field(row, "id")?,
            intent_id: field(row, "intent_id")?,
            run_id: field(row, "run_id")?,
            requested_amount: decode_amount(
                &field::<String>(row, "requested_amount")?,
                "pay_approval.requested_amount",
            )?,
            approved_amount: decode_optional_amount(field(row, "approved_amount")?, "pay_approval.approved_amount")?,
            requested_by: field(row, "requested_by")?,
            approved_by: field(row, "approved_by")?,
            status: approval_status(&field::<String>(row, "status")?)?,
            intent_hash: field(row, "intent_hash")?,
            grant_jti: field(row, "grant_jti")?,
            grant_claims_hash: field(row, "grant_claims_hash")?,
            sealed_grant_token: field::<Option<String>>(row, "sealed_grant_token")?
                .map(|value| decode_json::<SealedSensitiveMaterial>(&value, "pay_approval.sealed_grant_token"))
                .transpose()?,
            expires_at: field(row, "expires_at")?,
            consumed_at: field(row, "consumed_at")?,
            created_at: field(row, "created_at")?,
            approved_at: field(row, "approved_at")?,
        })
    }

    pub(super) fn ledger(row: &SqliteRow) -> Result<PaymentLedgerEntry> {
        Ok(PaymentLedgerEntry {
            id: field(row, "id")?,
            intent_id: field(row, "intent_id")?,
            scenario_id: field(row, "scenario_id")?,
            wallet_id: field(row, "wallet_id")?,
            entry_type: ledger_type(&field::<String>(row, "entry_type")?)?,
            direction: direction(&field::<String>(row, "direction")?)?,
            asset: asset(row)?,
            amount: decode_amount(&field::<String>(row, "amount")?, "pay_ledger.amount")?,
            posting_key: field(row, "posting_key")?,
            tx_hash: field(row, "tx_hash")?,
            settlement_ref: field(row, "settlement_ref")?,
            effect_ref: field(row, "effect_ref")?,
            transfer_ref: field(row, "transfer_ref")?,
            reversal_of: field(row, "reversal_of")?,
            effective_at: field(row, "effective_at")?,
            created_at: field(row, "created_at")?,
        })
    }

    pub(super) fn outbox(row: &SqliteRow) -> Result<OutboxMessage> {
        Ok(OutboxMessage {
            id: field(row, "id")?,
            aggregate_type: field(row, "aggregate_type")?,
            aggregate_id: field(row, "aggregate_id")?,
            event_type: field(row, "event_type")?,
            destination: field(row, "destination")?,
            run_id: field(row, "run_id")?,
            payload: decode_json(&field::<String>(row, "payload")?, "pay_outbox.payload")?,
            status: outbox_status(&field::<String>(row, "status")?)?,
            attempt_count: decode_u32(field(row, "attempt_count")?, "pay_outbox.attempt_count")?,
            available_at: field(row, "available_at")?,
            lease_owner: field(row, "lease_owner")?,
            lease_expires_at: field(row, "lease_expires_at")?,
            last_error: field(row, "last_error")?,
            created_at: field(row, "created_at")?,
            delivered_at: field(row, "delivered_at")?,
        })
    }

    pub(super) fn payment_wallet(row: &SqliteRow) -> Result<PaymentWallet> {
        let public_key: Option<String> = field(row, "public_key")?;
        let budget_asset: Option<String> = field(row, "budget_asset")?;
        let budget_decimals: Option<i64> = field(row, "budget_decimals")?;
        let budget_unit = match (budget_asset, budget_decimals) {
            (None, None) => None,
            (Some(asset), Some(decimals)) => Some(BudgetUnit {
                asset,
                decimals: decode_u8(decimals, "pay_wallet.budget_decimals")?,
            }),
            _ => {
                return Err(Error::Storage(
                    "SQLite wallet contains an incomplete budget unit".to_owned(),
                ))
            }
        };
        Ok(PaymentWallet {
            wallet: crate::model::wallet::Wallet {
                id: field(row, "id")?,
                algorithm: field(row, "algorithm")?,
                address: field(row, "address")?,
                public_key: public_key.unwrap_or_default(),
                created_at: field(row, "created_at")?,
            },
            custody: custody(&field::<String>(row, "custody_type")?)?,
            enabled: decode_bool(field(row, "enabled")?, "pay_wallet.enabled")?,
            active_key_version: field::<Option<i64>>(row, "active_key_version")?
                .map(|value| decode_u32(value, "wallet_key_version.version"))
                .transpose()?,
            budget_unit,
            max_per_payment: decode_optional_amount(field(row, "max_per_payment")?, "pay_wallet.max_per_payment")?,
            max_per_day: decode_optional_amount(field(row, "max_per_day")?, "pay_wallet.max_per_day")?,
            updated_at: field(row, "updated_at")?,
        })
    }

    pub(super) fn encrypted_key(row: &SqliteRow) -> Result<EncryptedWalletKeyVersion> {
        Ok(EncryptedWalletKeyVersion {
            wallet_id: field(row, "wallet_id")?,
            version: decode_u32(field(row, "version")?, "wallet_key_version.version")?,
            algorithm: field(row, "algorithm")?,
            address: field(row, "address")?,
            public_key: field(row, "public_key")?,
            encryption_scheme: field(row, "encryption_scheme")?,
            kek_id: field(row, "kek_id")?,
            wrapped_data_key: field(row, "wrapped_data_key")?,
            wrap_nonce: field(row, "wrap_nonce")?,
            key_ciphertext: field(row, "key_ciphertext")?,
            key_nonce: field(row, "key_nonce")?,
            aad: field(row, "aad")?,
            status: key_status(&field::<String>(row, "status")?)?,
            activated_at: field(row, "activated_at")?,
            retired_at: field(row, "retired_at")?,
            created_at: field(row, "created_at")?,
        })
    }

    fn asset(row: &SqliteRow) -> Result<AssetIdentity> {
        Ok(AssetIdentity {
            network: field(row, "network")?,
            asset: field(row, "asset")?,
            token_contract: field(row, "token_contract")?,
            decimals: decode_u8(field(row, "asset_decimals")?, "asset_decimals")?,
        })
    }

    fn field<T>(row: &SqliteRow, field_name: &str) -> Result<T>
    where
        for<'row> T: sqlx::Decode<'row, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
    {
        row.try_get(field_name)
            .map_err(|error| Error::Storage(format!("decode SQLite column {field_name}: {error}")))
    }

    fn invalid_enum(field_name: &str, value: &str) -> Error {
        Error::Storage(format!("SQLite {field_name} contains unsupported value {value}"))
    }

    fn direction(value: &str) -> Result<PaymentDirection> {
        match value {
            "inbound" => Ok(PaymentDirection::Inbound),
            "outbound" => Ok(PaymentDirection::Outbound),
            _ => Err(invalid_enum("direction", value)),
        }
    }

    fn payment_status(value: &str) -> Result<PaymentStatus> {
        match value {
            "created" => Ok(PaymentStatus::Created),
            "pending_approval" => Ok(PaymentStatus::PendingApproval),
            "authorized" => Ok(PaymentStatus::Authorized),
            "signed" => Ok(PaymentStatus::Signed),
            "submitted" => Ok(PaymentStatus::Submitted),
            "pending_settlement" => Ok(PaymentStatus::PendingSettlement),
            "settled" => Ok(PaymentStatus::Settled),
            "failed" => Ok(PaymentStatus::Failed),
            "expired" => Ok(PaymentStatus::Expired),
            "unknown" => Ok(PaymentStatus::Unknown),
            _ => Err(invalid_enum("payment status", value)),
        }
    }

    fn reservation_status(value: &str) -> Result<ReservationStatus> {
        match value {
            "unreserved" => Ok(ReservationStatus::Unreserved),
            "reserved" => Ok(ReservationStatus::Reserved),
            "consumed" => Ok(ReservationStatus::Consumed),
            "released" => Ok(ReservationStatus::Released),
            _ => Err(invalid_enum("reservation status", value)),
        }
    }

    fn approval_status(value: &str) -> Result<ApprovalStatus> {
        match value {
            "pending" => Ok(ApprovalStatus::Pending),
            "approved" => Ok(ApprovalStatus::Approved),
            "rejected" => Ok(ApprovalStatus::Rejected),
            "expired" => Ok(ApprovalStatus::Expired),
            "consumed" => Ok(ApprovalStatus::Consumed),
            _ => Err(invalid_enum("approval status", value)),
        }
    }

    fn event_type(value: &str) -> Result<PaymentEventType> {
        match value {
            "challenge_created" => Ok(PaymentEventType::ChallengeCreated),
            "challenge_received" => Ok(PaymentEventType::ChallengeReceived),
            "sign_requested" => Ok(PaymentEventType::SignRequested),
            "signed" => Ok(PaymentEventType::Signed),
            "approval_requested" => Ok(PaymentEventType::ApprovalRequested),
            "approval_granted" => Ok(PaymentEventType::ApprovalGranted),
            "approval_rejected" => Ok(PaymentEventType::ApprovalRejected),
            "approval_consumed" => Ok(PaymentEventType::ApprovalConsumed),
            "credential_verified" => Ok(PaymentEventType::CredentialVerified),
            "submitted" => Ok(PaymentEventType::Submitted),
            "settlement_requested" => Ok(PaymentEventType::SettlementRequested),
            "settled" => Ok(PaymentEventType::Settled),
            "retried" => Ok(PaymentEventType::Retried),
            "failed" => Ok(PaymentEventType::Failed),
            "reconciled" => Ok(PaymentEventType::Reconciled),
            _ => Err(invalid_enum("payment event type", value)),
        }
    }

    fn ledger_type(value: &str) -> Result<LedgerEntryType> {
        match value {
            "payment" => Ok(LedgerEntryType::Payment),
            "fee" => Ok(LedgerEntryType::Fee),
            "refund" => Ok(LedgerEntryType::Refund),
            "deposit" => Ok(LedgerEntryType::Deposit),
            "transfer" => Ok(LedgerEntryType::Transfer),
            "reversal" => Ok(LedgerEntryType::Reversal),
            _ => Err(invalid_enum("ledger entry type", value)),
        }
    }

    fn outbox_status(value: &str) -> Result<OutboxStatus> {
        match value {
            "pending" => Ok(OutboxStatus::Pending),
            "delivered" => Ok(OutboxStatus::Delivered),
            "dead_letter" => Ok(OutboxStatus::DeadLetter),
            _ => Err(invalid_enum("outbox status", value)),
        }
    }

    fn custody(value: &str) -> Result<WalletCustody> {
        match value {
            "local" => Ok(WalletCustody::Local),
            "external" => Ok(WalletCustody::External),
            _ => Err(invalid_enum("wallet custody", value)),
        }
    }

    fn key_status(value: &str) -> Result<WalletKeyStatus> {
        match value {
            "active" => Ok(WalletKeyStatus::Active),
            "retired" => Ok(WalletKeyStatus::Retired),
            "compromised" => Ok(WalletKeyStatus::Compromised),
            _ => Err(invalid_enum("wallet key status", value)),
        }
    }
}

mod signing {
    use async_trait::async_trait;
    use sqlx::Row as _;

    use crate::{
        model::{PaymentChallenge, PaymentDirection, PaymentEventType, PaymentStatus, ReservationStatus},
        storage::{
            IdempotentWrite, OutboundSigningAuthorization, OutboundSigningIdentity, OutboundSigningRead,
            PersistedSignedPayment, SigningGuard, SigningStorage, SigningWrite, WalletCustody, WalletKeyStatus,
        },
        utils::secure_material::SealedSensitiveMaterial,
        Error, Result,
    };

    use super::{
        catalog::{load_payment_wallet_connection, load_policy_connection},
        conflict,
        payment::{append_side_effects, update_intent, ReservationMutation},
        records, storage_error, SqliteStorage,
    };

    #[async_trait]
    impl SigningStorage for SqliteStorage {
        async fn load_for_principal(
            &self,
            expected_principal: Option<&str>,
            intent_id: &str,
        ) -> Result<Option<OutboundSigningRead>> {
            if expected_principal.is_some_and(str::is_empty) {
                return Err(Error::Unauthenticated);
            }
            let mut connection = self
                .pool()
                .acquire()
                .await
                .map_err(|error| storage_error("acquire outbound signing read connection", error))?;
            let Some(identity) = load_identity_for_access(&mut connection, expected_principal, intent_id).await? else {
                return Ok(None);
            };
            match identity.intent.status {
                PaymentStatus::Signed
                | PaymentStatus::Submitted
                | PaymentStatus::PendingSettlement
                | PaymentStatus::Unknown
                | PaymentStatus::Settled => load_signed(&mut connection, identity).await.map(Some),
                PaymentStatus::Authorized => load_authorized(&mut connection, identity).await.map(Some),
                _ => Ok(None),
            }
        }

        async fn commit_signed(&self, write: SigningWrite<'_>) -> Result<IdempotentWrite<PersistedSignedPayment>> {
            let intent = write.intent.clone();
            let challenge = write.challenge.clone();
            let credential_hash = write.credential_hash.to_owned();
            let sealed_credential = write.sealed_credential.clone();
            let guard = write.guard.clone();
            let events = write.events.to_vec();
            let outbox = write.outbox.to_vec();
            self.immediate(|connection| {
                Box::pin(async move {
                    if let Some(identity) = load_identity(connection, &guard.principal, &intent.id).await? {
                        if matches!(
                            identity.intent.status,
                            PaymentStatus::Signed
                                | PaymentStatus::Submitted
                                | PaymentStatus::PendingSettlement
                                | PaymentStatus::Unknown
                                | PaymentStatus::Settled
                        ) {
                            let OutboundSigningRead::Signed(existing) = load_signed(connection, identity).await? else {
                                return Err(Error::Storage("signed payment replay has no credential".to_owned()));
                            };
                            if existing.identity.challenge == challenge
                                && existing.identity.intent.same_immutable_identity_as(&intent)
                            {
                                return Ok(IdempotentWrite::Replay(*existing));
                            }
                            return Err(conflict("signed payment replay differs from the committed credential"));
                        }
                    }

                    let authorization = load_and_validate_guard(connection, &guard, &intent, &challenge).await?;
                    validate_signing_events(&events, &intent.id, &credential_hash, &sealed_credential)?;
                    update_intent(
                        connection,
                        &intent,
                        guard.expected_intent_updated_at,
                        ReservationMutation::Unchanged,
                    )
                    .await?;
                    let result = sqlx::query(
                        "UPDATE pay_intent SET credential_hash = ?1 WHERE id = ?2 AND credential_hash IS NULL",
                    )
                    .bind(&credential_hash)
                    .bind(&intent.id)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| conflict(format!("signed credential binding rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(conflict("payment credential was already bound"));
                    }
                    append_side_effects(connection, &events, &outbox).await?;
                    Ok(IdempotentWrite::Inserted(PersistedSignedPayment {
                        identity: OutboundSigningIdentity {
                            client: authorization.identity.client,
                            scenario: authorization.identity.scenario,
                            intent,
                            challenge,
                        },
                        credential_hash,
                        sealed_credential,
                    }))
                })
            })
            .await
        }
    }

    async fn load_identity_for_access(
        connection: &mut sqlx::SqliteConnection,
        expected_principal: Option<&str>,
        intent_id: &str,
    ) -> Result<Option<OutboundSigningIdentity>> {
        let principal: Option<String> = sqlx::query_scalar(
            "SELECT c.auth_principal FROM pay_intent i \
             JOIN pay_scenario s ON s.id = i.scenario_id \
             JOIN pay_client c ON c.id = s.client_id \
             WHERE i.id = ?1 AND (?2 IS NULL OR c.auth_principal = ?2) \
               AND c.enabled = 1 AND s.enabled = 1",
        )
        .bind(intent_id)
        .bind(expected_principal)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| storage_error("load accessible signing intent", error))?;
        let Some(principal) = principal else {
            return Ok(None);
        };
        load_identity(connection, &principal, intent_id).await
    }

    async fn load_identity(
        connection: &mut sqlx::SqliteConnection,
        principal: &str,
        intent_id: &str,
    ) -> Result<Option<OutboundSigningIdentity>> {
        let row = sqlx::query(
            "SELECT i.*, s.id AS joined_scenario_id, s.client_id AS joined_client_id, \
             s.name AS scenario_name, s.enabled AS scenario_enabled, s.created_at AS scenario_created_at, \
             s.updated_at AS scenario_updated_at, c.id AS joined_client_row_id, c.name AS client_name, \
             c.auth_principal, c.enabled AS client_enabled, c.created_at AS client_created_at, \
             c.updated_at AS client_updated_at FROM pay_intent i \
             JOIN pay_scenario s ON s.id = i.scenario_id JOIN pay_client c ON c.id = s.client_id \
             WHERE i.id = ?1 AND c.auth_principal = ?2 AND c.enabled = 1 AND s.enabled = 1",
        )
        .bind(intent_id)
        .bind(principal)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| storage_error("load principal-owned signing intent", error))?;
        let Some(row) = row else { return Ok(None) };
        let intent = records::intent(&row)?;
        if intent.direction != PaymentDirection::Outbound {
            return Ok(None);
        }
        let client = crate::model::PaymentClient {
            id: row.try_get("joined_client_row_id").map_err(decode_join)?,
            name: row.try_get("client_name").map_err(decode_join)?,
            auth_principal: row.try_get("auth_principal").map_err(decode_join)?,
            enabled: decode_join_bool(row.try_get("client_enabled").map_err(decode_join)?)?,
            created_at: row.try_get("client_created_at").map_err(decode_join)?,
            updated_at: row.try_get("client_updated_at").map_err(decode_join)?,
        };
        let scenario = crate::model::PaymentScenario {
            id: row.try_get("joined_scenario_id").map_err(decode_join)?,
            client_id: row.try_get("joined_client_id").map_err(decode_join)?,
            name: row.try_get("scenario_name").map_err(decode_join)?,
            enabled: decode_join_bool(row.try_get("scenario_enabled").map_err(decode_join)?)?,
            created_at: row.try_get("scenario_created_at").map_err(decode_join)?,
            updated_at: row.try_get("scenario_updated_at").map_err(decode_join)?,
        };
        let challenge = load_challenge(connection, &intent.id).await?;
        Ok(Some(OutboundSigningIdentity {
            client,
            scenario,
            intent,
            challenge,
        }))
    }

    fn decode_join(error: sqlx::Error) -> Error {
        storage_error("decode outbound signing join", error)
    }

    fn decode_join_bool(value: i64) -> Result<bool> {
        match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Storage(
                "outbound signing join has an invalid boolean".to_owned(),
            )),
        }
    }

    async fn load_challenge(connection: &mut sqlx::SqliteConnection, intent_id: &str) -> Result<PaymentChallenge> {
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM pay_event WHERE intent_id = ?1 AND event_type = 'challenge_received' AND attempt_no = 0",
        )
        .bind(intent_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| storage_error("load durable outbound challenge", error))?
        .ok_or_else(|| Error::Storage("authorized outbound intent has no durable challenge".to_owned()))?;
        let payload: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|error| Error::Storage(format!("decode outbound challenge event: {error}")))?;
        serde_json::from_value(
            payload
                .get("challenge")
                .cloned()
                .ok_or_else(|| Error::Storage("challenge event payload is incomplete".to_owned()))?,
        )
        .map_err(|error| Error::Storage(format!("decode outbound payment challenge: {error}")))
    }

    async fn load_signed(
        connection: &mut sqlx::SqliteConnection,
        identity: OutboundSigningIdentity,
    ) -> Result<OutboundSigningRead> {
        let row = sqlx::query(
            "SELECT e.signature_ref, e.payload, i.credential_hash AS intent_credential_hash \
             FROM pay_event e JOIN pay_intent i ON i.id = e.intent_id \
             WHERE e.intent_id = ?1 AND e.event_type = 'signed' AND e.attempt_no = 0",
        )
        .bind(&identity.intent.id)
        .fetch_optional(connection)
        .await
        .map_err(|error| storage_error("load durable signed credential", error))?
        .ok_or_else(|| Error::Storage("signed payment has no durable signed event".to_owned()))?;
        let credential_hash: String = row.try_get("signature_ref").map_err(decode_join)?;
        let intent_credential_hash: Option<String> = row.try_get("intent_credential_hash").map_err(decode_join)?;
        let payload: String = row.try_get("payload").map_err(decode_join)?;
        let payload: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|error| Error::Storage(format!("decode signed event payload: {error}")))?;
        let sealed_credential: SealedSensitiveMaterial = serde_json::from_value(
            payload
                .get("sealed_credential")
                .cloned()
                .ok_or_else(|| Error::Storage("signed event has no sealed credential".to_owned()))?,
        )
        .map_err(|error| Error::Storage(format!("decode sealed signed credential: {error}")))?;
        if intent_credential_hash.as_deref() != Some(credential_hash.as_str())
            || payload.get("credential_hash").and_then(serde_json::Value::as_str) != Some(credential_hash.as_str())
        {
            return Err(Error::Storage(
                "signed event credential fingerprint is inconsistent".to_owned(),
            ));
        }
        Ok(OutboundSigningRead::Signed(Box::new(PersistedSignedPayment {
            identity,
            credential_hash,
            sealed_credential,
        })))
    }

    async fn load_authorized(
        connection: &mut sqlx::SqliteConnection,
        identity: OutboundSigningIdentity,
    ) -> Result<OutboundSigningRead> {
        let policy_id = identity
            .intent
            .policy_id
            .as_deref()
            .ok_or_else(|| Error::Storage("authorized signing intent has no policy".to_owned()))?;
        let wallet_id = identity
            .intent
            .wallet_id
            .as_deref()
            .ok_or_else(|| Error::Storage("authorized signing intent has no wallet".to_owned()))?;
        let policy = load_policy_connection(connection, policy_id)
            .await?
            .ok_or_else(|| Error::Storage("authorized signing policy does not exist".to_owned()))?;
        let wallet = load_payment_wallet_connection(connection, wallet_id)
            .await?
            .ok_or_else(|| Error::Storage("authorized signing wallet does not exist".to_owned()))?;
        let key_row = sqlx::query("SELECT * FROM wallet_key_version WHERE wallet_id = ?1 AND status = 'active'")
            .bind(wallet_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| storage_error("load authorized signing key", error))?
            .ok_or_else(|| Error::Storage("authorized signing wallet has no active key".to_owned()))?;
        let active_key = records::encrypted_key(&key_row)?;
        validate_authorization_state(&identity, &policy, &wallet, &active_key, None)?;
        Ok(OutboundSigningRead::Authorized(Box::new(
            OutboundSigningAuthorization {
                identity,
                policy,
                wallet,
                active_key,
            },
        )))
    }

    async fn load_and_validate_guard(
        connection: &mut sqlx::SqliteConnection,
        guard: &SigningGuard,
        intent: &crate::model::PaymentIntent,
        challenge: &PaymentChallenge,
    ) -> Result<OutboundSigningAuthorization> {
        let identity = load_identity(connection, &guard.principal, &intent.id).await?;
        let identity = identity.ok_or(Error::Forbidden)?;
        let OutboundSigningRead::Authorized(authorization) = load_authorized(connection, identity).await? else {
            return Err(conflict("payment is no longer authorized for signing"));
        };
        let authorization = *authorization;
        if authorization.identity.client.id != guard.client_id
            || authorization.identity.client.updated_at != guard.expected_client_updated_at
            || authorization.identity.scenario.id != guard.scenario_id
            || authorization.identity.scenario.updated_at != guard.expected_scenario_updated_at
            || authorization.policy.id != guard.policy_id
            || authorization.policy.updated_at != guard.expected_policy_updated_at
            || authorization.wallet.wallet.id != guard.wallet_id
            || authorization.wallet.updated_at != guard.expected_wallet_updated_at
            || authorization.active_key.version != guard.expected_key_version
            || authorization.identity.intent.updated_at != guard.expected_intent_updated_at
            || authorization.identity.challenge.challenge_hash != guard.challenge_hash
            || authorization.identity.challenge != *challenge
            || !authorization.identity.intent.same_immutable_identity_as(intent)
            || intent.status != PaymentStatus::Signed
            || intent.updated_at <= guard.expected_intent_updated_at
        {
            return Err(conflict("outbound signing authority changed before commit"));
        }
        validate_authorization_state(
            &authorization.identity,
            &authorization.policy,
            &authorization.wallet,
            &authorization.active_key,
            Some(guard.evaluated_at),
        )?;
        Ok(authorization)
    }

    fn validate_authorization_state(
        identity: &OutboundSigningIdentity,
        policy: &crate::model::PaymentPolicy,
        wallet: &crate::storage::PaymentWallet,
        key: &crate::storage::EncryptedWalletKeyVersion,
        evaluated_at: Option<i64>,
    ) -> Result<()> {
        let intent = &identity.intent;
        let challenge = &identity.challenge;
        let origin = canonical_origin(&intent.resource)?;
        let Some(limits) = policy.limits_for(PaymentDirection::Outbound) else {
            return Err(Error::PolicyDenied(
                "outbound payment signing authority is no longer valid".to_owned(),
            ));
        };
        if intent.status != PaymentStatus::Authorized
            || intent.direction != PaymentDirection::Outbound
            || intent.reservation_status != ReservationStatus::Reserved
            || intent.reserved_amount != intent.amount
            || evaluated_at.is_some_and(|now| challenge.expires_at <= now)
            || intent.expires_at != Some(challenge.expires_at)
            || intent.challenge_hash.as_deref() != Some(challenge.challenge_hash.as_str())
            || challenge.scenario_id != intent.scenario_id
            || challenge.offer_id.is_some()
            || challenge.resource != intent.resource
            || challenge.protocol != intent.protocol
            || challenge.asset != intent.asset
            || challenge.amount != intent.amount
            || !crate::model::payment_recipients_match(&intent.asset.network, &challenge.recipient, &intent.recipient)
            || challenge.nonce.trim().is_empty()
            || identity.scenario.id != intent.scenario_id
            || identity.scenario.client_id != identity.client.id
            || intent.policy_id.as_deref() != Some(policy.id.as_str())
            || intent.wallet_id.as_deref() != Some(wallet.wallet.id.as_str())
            || !policy.enabled
            || policy.scenario_id != identity.scenario.id
            || !policy.allows_wallet(&wallet.wallet.id)
            || !policy.allows_direction(PaymentDirection::Outbound)
            || !policy.allows_protocol(&intent.protocol)
            || !policy.allows_asset(&intent.asset)
            || !policy.allows_payment_recipient(&intent.asset.network, &intent.recipient)
            || !policy.allows_origin(&origin)
            || !policy.budget_unit.matches(&intent.asset)
            || intent.amount > limits.max_per_payment
            || limits.hard_daily_limit().is_none_or(|maximum| intent.amount > maximum)
            || !wallet.signing_ready_for_payment_network(&intent.asset.network)
            || wallet.custody != WalletCustody::Local
            || wallet.budget_unit.as_ref() != Some(&intent.asset.budget_unit())
            || wallet.max_per_payment.is_some_and(|maximum| intent.amount > maximum)
            || wallet.max_per_day.is_some_and(|maximum| intent.amount > maximum)
            || wallet.active_key_version != Some(key.version)
            || key.status != WalletKeyStatus::Active
            || key.wallet_id != wallet.wallet.id
            || key.algorithm != wallet.wallet.algorithm
            || key.address != wallet.wallet.address
            || key.public_key != wallet.wallet.public_key
            || key.version == 0
            || evaluated_at.is_some_and(|now| key.activated_at > now)
            || key.encryption_scheme.trim().is_empty()
            || key.kek_id.trim().is_empty()
            || key.wrapped_data_key.is_empty()
            || key.wrap_nonce.is_empty()
            || key.key_ciphertext.is_empty()
            || key.key_nonce.is_empty()
            || key.aad.is_empty()
            || key.retired_at.is_some()
        {
            return Err(Error::PolicyDenied(
                "outbound payment signing authority is no longer valid".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_signing_events(
        events: &[crate::model::PaymentEvent],
        intent_id: &str,
        credential_hash: &str,
        sealed_credential: &SealedSensitiveMaterial,
    ) -> Result<()> {
        let requested = events
            .iter()
            .filter(|event| event.event_type == PaymentEventType::SignRequested)
            .collect::<Vec<_>>();
        let signed = events
            .iter()
            .filter(|event| event.event_type == PaymentEventType::Signed)
            .collect::<Vec<_>>();
        if events.len() != 2
            || requested.len() != 1
            || signed.len() != 1
            || events
                .iter()
                .any(|event| event.intent_id != intent_id || event.attempt_no != 0)
            || signed[0].signature_ref.as_deref() != Some(credential_hash)
            || signed[0]
                .payload
                .get("credential_hash")
                .and_then(serde_json::Value::as_str)
                != Some(credential_hash)
            || signed[0]
                .payload
                .get("sealed_credential")
                .cloned()
                .and_then(|value| serde_json::from_value::<SealedSensitiveMaterial>(value).ok())
                .as_ref()
                != Some(sealed_credential)
        {
            return Err(conflict("outbound signing events are incomplete or inconsistent"));
        }
        Ok(())
    }

    fn canonical_origin(resource: &str) -> Result<String> {
        let parsed = reqwest::Url::parse(resource)
            .map_err(|_| Error::PolicyDenied("payment resource has no canonical HTTP origin".to_owned()))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(Error::PolicyDenied(
                "payment resource has no canonical HTTP origin".to_owned(),
            ));
        }
        Ok(parsed.origin().ascii_serialization())
    }
}

// All correctness-sensitive writes use `SqliteStorage::immediate`. SQLite
// foreign-key enforcement is connection-local, so the pool both enables and
// verifies it for every connection before that connection can serve work.

use std::{future::Future, path::Path, pin::Pin, str::FromStr, sync::Arc};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Executor as _, SqliteConnection, SqlitePool,
};

use crate::{config::SqliteStorageConfig, Error, Result};

pub use key_repository::SqliteWalletKeyRepository;

#[cfg(test)]
#[path = "../../tests/storage/sqlite_test.rs"]
mod tests;

const SCHEMA_VERSION: i64 = 1;
const CORE_TABLES: &[&str] = &[
    "pay_schema_version",
    "pay_wallet",
    "wallet_key_version",
    "pay_client",
    "pay_scenario",
    "pay_scenario_approver",
    "pay_policy",
    "pay_policy_wallet",
    "pay_offer",
    "pay_intent",
    "pay_event",
    "pay_approval",
    "pay_ledger",
    "pay_outbox",
];
const INITIAL_SCHEMA_DDL: &str = include_str!("../../migrations/sqlite/001_init.ddl.sql");
const INITIAL_BOOTSTRAP_DML: &str = include_str!("../../migrations/sqlite/001_init.dml.sql");

pub(crate) type ImmediateFuture<'connection, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'connection>>;

/// Cloneable `SQLite` implementation shared by every payment storage port.
#[derive(Clone, Debug)]
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    /// Constructs a lazy durable `SQLite` provider without opening a
    /// connection. [`crate::storage::IStorage::init`] owns connection and
    /// migration work.
    ///
    /// # Errors
    ///
    /// Fails closed for a non-absolute path, invalid connection settings,
    /// unsupported schema version, missing schema objects, or a disabled
    /// foreign-key pragma.
    pub(crate) fn from_config(config: &SqliteStorageConfig) -> Result<Self> {
        let path = Path::new(&config.database_file);
        if !path.is_absolute() || config.max_connections == 0 || config.busy_timeout.is_zero() {
            return Err(Error::Config(
                "SQLite payment storage requires an absolute path, a positive pool size, and a positive busy timeout"
                    .to_owned(),
            ));
        }

        let options = SqliteConnectOptions::from_str("sqlite://")
            .map_err(|error| Error::Config(format!("invalid SQLite connection options: {error}")))?
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(config.busy_timeout);

        let pool = SqlitePoolOptions::new()
            .max_connections(config.max_connections)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(&mut *connection)
                        .await?;
                    let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                        .fetch_one(&mut *connection)
                        .await?;
                    if enabled != 1 {
                        return Err(sqlx::Error::Protocol(
                            "SQLite foreign-key enforcement is disabled".to_owned(),
                        ));
                    }
                    Ok(())
                })
            })
            .connect_lazy_with(options);

        Ok(Self { pool })
    }

    #[cfg(test)]
    pub(crate) async fn connect(config: &SqliteStorageConfig) -> Result<Self> {
        use crate::storage::IStorage as _;

        let storage = Self::from_config(config)?;
        storage.init().await?;
        Ok(storage)
    }

    /// Returns a pool reference for adapter modules. It is intentionally not
    /// public API: callers should depend on the storage ports.
    pub(crate) const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    async fn migrate(&self) -> Result<()> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|error| storage_error("acquire SQLite migration connection", error))?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await
            .map_err(|error| storage_error("begin SQLite migration", error))?;

        let result = async {
            let version: i64 = sqlx::query_scalar("PRAGMA user_version")
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| storage_error("read SQLite schema version", error))?;
            match version {
                0 => {
                    let body = migration_body()?;
                    sqlx::raw_sql(body)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("apply SQLite payment schema", error))?;
                    sqlx::raw_sql(INITIAL_BOOTSTRAP_DML)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("apply SQLite payment bootstrap data", error))?;
                    sqlx::query("PRAGMA user_version = 1")
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| storage_error("record SQLite schema version", error))?;
                }
                SCHEMA_VERSION => {}
                unsupported => {
                    return Err(Error::Storage(format!(
                        "unsupported SQLite payment schema version {unsupported}"
                    )));
                }
            }
            verify_schema(&mut connection).await
        }
        .await;

        finish_transaction(&mut connection, result.is_ok()).await?;
        result
    }

    /// Runs one authoritative write aggregate under `SQLite`'s eager writer
    /// lock. The closure must not commit or roll back the connection itself.
    pub(crate) async fn immediate<T, Operation>(&self, operation: Operation) -> Result<T>
    where
        T: Send,
        Operation: for<'connection> FnOnce(&'connection mut SqliteConnection) -> ImmediateFuture<'connection, T> + Send,
    {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|error| storage_error("acquire SQLite write connection", error))?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await
            .map_err(|error| storage_error("begin SQLite write transaction", error))?;
        let result = operation(&mut connection).await;
        finish_transaction(&mut connection, result.is_ok()).await?;
        result
    }
}

impl crate::storage::IStorage for SqliteStorage {
    fn provider(&self) -> crate::config::StorageProvider {
        crate::config::StorageProvider::Sqlite
    }

    fn init(&self) -> crate::storage::StorageLifecycleFuture {
        let storage = self.clone();
        Box::pin(async move { storage.migrate().await })
    }

    fn ping(&self) -> crate::storage::StorageHealthFuture {
        let pool = self.pool.clone();
        Box::pin(async move {
            sqlx::query("SELECT 1")
                .execute(&pool)
                .await
                .map_err(|error| storage_error("ping SQLite payment storage", error))?;
            Ok(())
        })
    }

    fn close(&self) -> crate::storage::StorageCloseFuture {
        let pool = self.pool.clone();
        Box::pin(async move {
            pool.close().await;
            Ok(())
        })
    }

    fn ports(self: Arc<Self>) -> crate::storage::StoragePorts {
        crate::storage::StoragePorts {
            clients: self.clone(),
            scenarios: self.clone(),
            approvers: self.clone(),
            policies: self.clone(),
            offers: self.clone(),
            wallets: self.clone(),
            wallet_balances: self.clone(),
            payments: self.clone(),
            approvals: self.clone(),
            events: self.clone(),
            ledger: self.clone(),
            outbox: self.clone(),
            signing: self,
        }
    }

    fn wallet_key_manager(
        self: Arc<Self>,
        config: &crate::config::WalletKeyRingConfig,
    ) -> Result<Arc<dyn crate::storage::WalletKeyManager>> {
        Ok(Arc::new(SqliteWalletKeyRepository::from_config(
            (*self).clone(),
            config,
        )?))
    }

    fn wallet_payment_signer(
        self: Arc<Self>,
        config: &crate::config::WalletKeyRingConfig,
    ) -> Result<Arc<dyn crate::storage::WalletPaymentSigner>> {
        Ok(Arc::new(SqliteWalletKeyRepository::from_config(
            (*self).clone(),
            config,
        )?))
    }
}

fn migration_body() -> Result<&'static str> {
    let migration = INITIAL_SCHEMA_DDL.trim();
    let migration = migration
        .strip_prefix("PRAGMA foreign_keys = ON;")
        .ok_or_else(|| Error::Storage("SQLite migration is missing its foreign-key preamble".to_owned()))?
        .trim_start();
    let migration = migration
        .strip_prefix("BEGIN IMMEDIATE;")
        .ok_or_else(|| Error::Storage("SQLite migration is missing BEGIN IMMEDIATE".to_owned()))?
        .trim_start();
    migration
        .strip_suffix("COMMIT;")
        .map(str::trim_end)
        .ok_or_else(|| Error::Storage("SQLite migration is missing COMMIT".to_owned()))
}

async fn verify_schema(connection: &mut SqliteConnection) -> Result<()> {
    for table in CORE_TABLES {
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(table)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| storage_error("verify SQLite payment schema", error))?;
        if present != 1 {
            return Err(Error::Storage(format!(
                "SQLite payment schema is missing required table {table}"
            )));
        }
    }
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM pay_schema_version ORDER BY version")
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| storage_error("read SQLite payment schema version", error))?;
    if versions != [SCHEMA_VERSION] {
        return Err(Error::Storage(format!(
            "unsupported SQLite payment schema versions {versions:?}"
        )));
    }
    Ok(())
}

async fn finish_transaction(connection: &mut SqliteConnection, commit: bool) -> Result<()> {
    let statement = if commit { "COMMIT" } else { "ROLLBACK" };
    connection
        .execute(statement)
        .await
        .map_err(|error| storage_error("finish SQLite transaction", error))?;
    Ok(())
}

pub(crate) fn storage_error(operation: &str, error: impl std::fmt::Display) -> Error {
    Error::Storage(format!("{operation}: {error}"))
}

pub(crate) fn conflict(message: impl Into<String>) -> Error {
    Error::PaymentConflict(message.into())
}
