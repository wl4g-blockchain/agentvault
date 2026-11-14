//! Payment transaction implementation shared across providers.
macro_rules! payment_repository {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        use crate::model::Amount;
        use crate::model::ApprovalRequest;
        use crate::model::BudgetUnit;
        use crate::model::PaymentChallenge;
        use crate::model::PaymentCredential;
        use crate::model::PaymentDirection;
        use crate::model::PaymentEvent;
        use crate::model::PaymentIntent;
        use crate::model::PaymentStatus;
        use crate::model::ReservationStatus;
        use crate::model::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;
        use crate::storage::codec::AmountEncoding as _;
        use crate::storage::payment::PaymentStorageRules;
        use crate::storage::payment::ReservationMutation;
        use crate::storage::AuthorizationAction;
        use crate::storage::AuthorizationGuard;
        use crate::storage::AuthorizationRecord;
        use crate::storage::AuthorizationWrite;
        use crate::storage::BudgetSnapshot;
        use crate::storage::ChallengeIssuanceGuard;
        use crate::storage::ChallengeWrite;
        use crate::storage::IIntentRepository;
        use crate::storage::IdempotentWrite;
        use crate::storage::OutboundEvidenceWrite;
        use crate::storage::Page;
        use crate::storage::PaymentProgressWrite;
        use crate::storage::PaymentQuery;
        use crate::storage::ReconciliationClaim;
        use crate::storage::ReconciliationLeaseRequest;
        use crate::storage::SettlementWrite;
        use crate::storage::VerificationWrite;
        use crate::Error;
        use crate::Result;
        use async_trait::async_trait;
        use sqlx::Row as _;

        #[async_trait]
        impl IIntentRepository for $repository {
            async fn find_run(&self, scenario_id: &str, run_id: &str) -> Result<Option<AuthorizationRecord>> {
                let row = dynamic_query!(
                    select,
                    crate::model::PaymentIntent,
                    crate::storage::EntityQuery::new()
                        .eq("scenario_id", scenario_id)
                        .eq("run_id", run_id)
                )?
                .build()
                .fetch_optional(self.base.pool())
                .await
                .map_err(|error| BaseRepository::storage_error("find payment run", error))?;
                let Some(row) = row else { return Ok(None) };
                let intent = Self::decode_row(&row)?;
                let approval = Self::load_approval_pool(self.base.pool(), &intent.id).await?;
                Ok(Some(AuthorizationRecord { intent, approval }))
            }

            async fn authorize(&self, write: AuthorizationWrite<'_>) -> Result<IdempotentWrite<AuthorizationRecord>> {
                let intent = write.intent.clone();
                let approval = write.approval.cloned();
                let action = write.action;
                let guard = write.guard.clone();
                let events = write.events.to_vec();
                let outbox = write.outbox.to_vec();
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            Self::lock_run_id(connection, &intent.scenario_id, &intent.run_id).await?;
                            if let Some(existing) =
                                Self::load_run_connection(connection, &intent.scenario_id, &intent.run_id).await?
                            {
                                if existing.intent.request_hash != intent.request_hash {
                                    return Err(crate::Error::conflict(
                                        "payment run_id is bound to a different request",
                                    ));
                                }
                                return Ok(IdempotentWrite::Replay(existing));
                            }
                            Self::validate_authorization_guard(connection, &intent, &guard, action).await?;
                            PaymentStorageRules::validate_challenge_received_event(&events, &intent)?;
                            match action {
                                AuthorizationAction::RecordDenied => {
                                    if approval.is_some()
                                        || intent.status != PaymentStatus::Failed
                                        || intent.reservation_status != ReservationStatus::Unreserved
                                    {
                                        return Err(crate::Error::conflict(
                                            "denied authorization aggregate has invalid lifecycle",
                                        ));
                                    }
                                }
                                AuthorizationAction::RequestApproval => {
                                    if approval
                                        .as_ref()
                                        .is_none_or(|request| request.intent_id != intent.id)
                                        || intent.status != PaymentStatus::PendingApproval
                                        || intent.reservation_status != ReservationStatus::Unreserved
                                    {
                                        return Err(crate::Error::conflict(
                                            "approval authorization aggregate has invalid lifecycle",
                                        ));
                                    }
                                }
                                AuthorizationAction::Reserve => {
                                    if approval.is_some()
                                        || intent.status != PaymentStatus::Authorized
                                        || intent.reservation_status != ReservationStatus::Reserved
                                        || intent.reserved_amount != intent.amount
                                    {
                                        return Err(crate::Error::conflict(
                                            "reserved authorization aggregate has invalid lifecycle",
                                        ));
                                    }
                                    Self::enforce_budget(
                                        connection,
                                        &intent,
                                        guard.day_started_at,
                                        guard.evaluated_at,
                                        false,
                                    )
                                    .await?;
                                }
                            }
                            Self::insert_intent(connection, &intent, None).await?;
                            if let Some(request) = &approval {
                                Self::insert_approval_request(connection, request).await?;
                            }
                            Self::append_side_effects(connection, &events, &outbox).await?;
                            Ok(IdempotentWrite::Inserted(AuthorizationRecord {
                                intent,
                                approval,
                            }))
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
                    .base
                    .pool()
                    .acquire()
                    .await
                    .map_err(|error| BaseRepository::storage_error("acquire budget read connection", error))?;
                Self::budget_snapshot_connection(
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
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            Self::lock_run_id(connection, &intent.scenario_id, &intent.run_id).await?;
                            if let Some(existing) =
                                Self::load_intent_by_run(connection, &intent.scenario_id, &intent.run_id).await?
                            {
                                if existing.request_hash == intent.request_hash {
                                    return Ok(IdempotentWrite::Replay(existing));
                                }
                                return Err(crate::Error::conflict(
                                    "payment run_id is bound to a different request",
                                ));
                            }
                            Self::validate_challenge_guard(connection, &intent, &challenge, &guard).await?;
                            PaymentStorageRules::validate_challenge_created_event(&events, &intent, &challenge)?;
                            Self::enforce_budget(
                                connection,
                                &intent,
                                guard.day_started_at,
                                guard.evaluated_at,
                                false,
                            )
                            .await?;
                            Self::insert_intent(connection, &intent, None).await?;
                            Self::append_side_effects(connection, &events, &outbox).await?;
                            Ok(IdempotentWrite::Inserted(intent))
                        })
                    })
                    .await
            }

            async fn get_for_challenge(
                &self,
                scenario_id: &str,
                challenge_hash: &str,
            ) -> Result<Option<PaymentIntent>> {
                let row = dynamic_query!(
                    select,
                    crate::model::PaymentIntent,
                    crate::storage::EntityQuery::new()
                        .eq("scenario_id", scenario_id)
                        .eq("challenge_hash", challenge_hash)
                        .eq("direction", "inbound")
                )?
                .build()
                .fetch_optional(self.base.pool())
                .await
                .map_err(|error| BaseRepository::storage_error("get inbound payment challenge", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn verify_incoming(&self, write: VerificationWrite<'_>) -> Result<PaymentIntent> {
                let intent = write.intent.clone();
                let credential = write.credential.clone();
                let expected_updated_at = write.expected_updated_at;
                let events = write.events.to_vec();
                let outbox = write.outbox.to_vec();
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            // Credential binding races with challenge expiry, which can
                            // release the same authoritative reservation. Both paths
                            // must acquire scenario, wallet, then intent locks in the
                            // same order or PostgreSQL can deadlock between the intent
                            // revision update and the conditional credential binding.
                            Self::lock_intent_financial_scope(connection, &intent).await?;
                            Self::bind_credential_transition(connection, &intent, &credential, expected_updated_at)
                                .await?;
                            Self::append_side_effects(connection, &events, &outbox).await?;
                            Ok(intent)
                        })
                    })
                    .await
            }

            async fn get(&self, intent_id: &str) -> Result<Option<PaymentIntent>> {
                let row = dynamic_query!(
                    select,
                    crate::model::PaymentIntent,
                    crate::storage::EntityQuery::new().eq("id", intent_id)
                )?
                .build()
                .fetch_optional(self.base.pool())
                .await
                .map_err(|error| BaseRepository::storage_error("get payment intent", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn get_for_scenario(&self, scenario_id: &str, intent_id: &str) -> Result<Option<PaymentIntent>> {
                let row = dynamic_query!(
                    select,
                    crate::model::PaymentIntent,
                    crate::storage::EntityQuery::new()
                        .eq("id", intent_id)
                        .eq("scenario_id", scenario_id)
                )?
                .build()
                .fetch_optional(self.base.pool())
                .await
                .map_err(|error| BaseRepository::storage_error("get scenario payment intent", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn commit_progress(&self, write: PaymentProgressWrite<'_>) -> Result<PaymentIntent> {
                let intent = write.intent.clone();
                let expected_updated_at = write.expected_updated_at;
                let events = write.events.to_vec();
                let outbox = write.outbox.to_vec();
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            Self::lock_intent_financial_scope(connection, &intent).await?;
                            let existing = Self::load_intent_connection(connection, &intent.id)
                                .await?
                                .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
                            let mutation = PaymentStorageRules::validate_progress_transition(
                                &existing,
                                &intent,
                                expected_updated_at,
                                &events,
                            )?;
                            Self::update_intent(connection, &intent, expected_updated_at, mutation).await?;
                            Self::append_side_effects(connection, &events, &outbox).await?;
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
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            Self::lock_intent_financial_scope(connection, &intent).await?;
                            let existing = Self::load_intent_connection(connection, &intent.id)
                                .await?
                                .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
                            if let Some(stored_event) =
                                Self::load_outbound_evidence_event(connection, &intent.id).await?
                            {
                                if crate::storage::OutboundEvidenceWrite::outbound_evidence_replay_matches(
                                    &stored_event,
                                    &intent.id,
                                    &evidence_hash,
                                ) {
                                    return Ok(IdempotentWrite::Replay(existing));
                                }
                                return Err(crate::Error::conflict(
                                    "outbound merchant evidence has different content",
                                ));
                            }
                            crate::storage::OutboundEvidenceWrite::validate_outbound_evidence_transition(
                                &existing,
                                &intent,
                                expected_updated_at,
                                &event,
                                &evidence_hash,
                            )?;
                            Self::update_intent(
                                connection,
                                &intent,
                                expected_updated_at,
                                ReservationMutation::Unchanged,
                            )
                            .await?;
                            Self::append_side_effects(connection, std::slice::from_ref(&event), &outbox).await?;
                            Ok(IdempotentWrite::Inserted(intent))
                        })
                    })
                    .await
            }

            async fn list(&self, query: &PaymentQuery) -> Result<Page<PaymentIntent>> {
                self.base
                    .entity_page(
                        query.entity_query().order("created_at", true).order("id", false),
                        query.page,
                    )
                    .await
            }

            async fn claim_reconciliation(
                &self,
                request: &ReconciliationLeaseRequest,
            ) -> Result<Vec<ReconciliationClaim>> {
                self.claim_reconciliation_batch(request).await
            }

            async fn renew_reconciliation_lease(&self, intent_id: &str, owner: &str, lease_until: i64) -> Result<()> {
                Self::update_reconciliation_lease(self, intent_id, owner, Some(lease_until), None).await
            }

            async fn release_reconciliation_lease(
                &self,
                intent_id: &str,
                owner: &str,
                next_attempt_at: i64,
            ) -> Result<()> {
                Self::update_reconciliation_lease(self, intent_id, owner, None, Some(next_attempt_at)).await
            }

            async fn commit_settlement(&self, write: SettlementWrite<'_>) -> Result<PaymentIntent> {
                let intent = write.intent.clone();
                let expected_updated_at = write.expected_updated_at;
                let events = write.events.to_vec();
                let entries = write.ledger_entries.to_vec();
                let outbox = write.outbox.to_vec();
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            Self::lock_intent_financial_scope(connection, &intent).await?;
                            let existing = Self::load_intent_connection(connection, &intent.id)
                                .await?
                                .ok_or_else(|| Error::NotFound(format!("payment intent {}", intent.id)))?;
                            PaymentStorageRules::validate_settlement_transition(
                                &existing,
                                &intent,
                                expected_updated_at,
                                &entries,
                            )?;
                            for entry in &entries {
                                LedgerRepository::insert_ledger(connection, entry).await?;
                            }
                            Self::update_intent(
                                connection,
                                &intent,
                                expected_updated_at,
                                ReservationMutation::Settlement,
                            )
                            .await?;
                            Self::append_side_effects(connection, &events, &outbox).await?;
                            Ok(intent)
                        })
                    })
                    .await
            }
        }

        impl $repository {
            async fn lock_intent_financial_scope(connection: &mut Connection, intent: &PaymentIntent) -> Result<()> {
                let wallet_id = intent
                    .wallet_id
                    .clone()
                    .ok_or_else(|| crate::Error::conflict("payment intent has no wallet authority"))?;
                LedgerRepository::lock_financial_scopes(
                    connection,
                    std::slice::from_ref(&intent.scenario_id),
                    std::slice::from_ref(&wallet_id),
                )
                .await
            }

            pub(crate) async fn load_intent_connection(
                connection: &mut Connection,
                intent_id: &str,
            ) -> Result<Option<PaymentIntent>> {
                let row = sqlx::query(concat!("SELECT * FROM pay_intent WHERE id = $1", $update))
                    .bind(intent_id)
                    .fetch_optional(connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("get payment intent", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn load_intent_by_run(
                connection: &mut Connection,
                scenario_id: &str,
                run_id: &str,
            ) -> Result<Option<PaymentIntent>> {
                let row = sqlx::query(concat!(
                    "SELECT * FROM pay_intent WHERE scenario_id = $1 AND run_id = $2",
                    $update
                ))
                .bind(scenario_id)
                .bind(run_id)
                .fetch_optional(connection)
                .await
                .map_err(|error| BaseRepository::storage_error("find payment intent run", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn load_run_connection(
                connection: &mut Connection,
                scenario_id: &str,
                run_id: &str,
            ) -> Result<Option<AuthorizationRecord>> {
                let Some(intent) = Self::load_intent_by_run(connection, scenario_id, run_id).await? else {
                    return Ok(None);
                };
                let row =
                    sqlx::query("SELECT * FROM pay_approval WHERE intent_id = $1 ORDER BY created_at DESC, id LIMIT 1")
                        .bind(&intent.id)
                        .fetch_optional(connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("load payment approval replay", error))?;
                let approval = row.as_ref().map(ApprovalRepository::decode_row).transpose()?;
                Ok(Some(AuthorizationRecord { intent, approval }))
            }

            async fn load_approval_pool(pool: &Pool, intent_id: &str) -> Result<Option<ApprovalRequest>> {
                let row =
                    sqlx::query("SELECT * FROM pay_approval WHERE intent_id = $1 ORDER BY created_at DESC, id LIMIT 1")
                        .bind(intent_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|error| BaseRepository::storage_error("load payment intent approval", error))?;
                row.as_ref().map(ApprovalRepository::decode_row).transpose()
            }

            pub(crate) async fn append_side_effects(
                connection: &mut Connection,
                events: &[crate::model::PaymentEvent],
                outbox: &[crate::storage::OutboxMessage],
            ) -> Result<()> {
                for event in events {
                    EventRepository::insert_event(connection, event).await?;
                }
                for message in outbox {
                    OutboxRepository::insert_outbox(connection, message).await?;
                }
                Ok(())
            }

            pub(crate) fn decode_row(row: &DbRow) -> Result<PaymentIntent> {
                sqlx::FromRow::from_row(row)
                    .map_err(|error| BaseRepository::storage_error("decode payment entity", error))
            }
        }
        crate::storage::payment::payment_authority!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
        crate::storage::policy::r#macro::payment_budget!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        crate::storage::payment::r#macro::payment_reconciliation!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
        crate::storage::payment::r#macro::payment_mutations!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
    };
}
pub(crate) use payment_repository;

#[cfg(any(test, feature = "test-support"))]
#[path = "../../../tests/storage/payment/transactions_test.rs"]
mod transactions_tests;

// Intent aggregate write primitives; callable only within the authoritative storage transactions.
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
mod mutations_tests;

// Leased payment reconciliation storage, sharing the same intent transaction authority.
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
                     AND (lease_expires_at IS NULL OR lease_expires_at <= $1) AND ($3 IS NULL OR direction = $3) \
                     ORDER BY COALESCE(next_reconcile_at, expires_at), created_at, id \
                     LIMIT $2",
                        $skip
                    ))
                    .bind(request.now)
                    .bind(i64::from(request.limit))
                    .bind(request.direction.map(|direction|match direction{PaymentDirection::Inbound=>"inbound",PaymentDirection::Outbound=>"outbound"}))
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
mod reconciliation_tests;
