macro_rules! payment_operations {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
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
    };
}
pub(crate) use payment_operations;
