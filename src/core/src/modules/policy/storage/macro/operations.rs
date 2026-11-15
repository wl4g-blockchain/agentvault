macro_rules! policy_operations {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        #[async_trait]
        impl IPolicyRepository for $repository {
            async fn insert(&self, policy: &PaymentPolicy) -> Result<PaymentPolicy> {
                PolicyStorageRules::validate_policy(policy)?;
                let policy = policy.clone();
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            Self::insert_policy_aggregate(connection, &policy).await?;
                            Ok(policy)
                        })
                    })
                    .await
            }

            async fn get(&self, policy_id: &str) -> Result<Option<PaymentPolicy>> {
                Self::load_policy_pool(self.base.pool(), policy_id).await
            }

            async fn update(&self, policy: &PaymentPolicy, expected_updated_at: i64) -> Result<PaymentPolicy> {
                PolicyStorageRules::validate_policy(policy)?;
                let policy = policy.clone();
                self.base
                    .transaction(|connection| {
                        Box::pin(async move {
                            let existing = Self::load_policy_for_update(connection, &policy.id)
                                .await?
                                .ok_or_else(|| Error::NotFound(format!("payment policy {}", policy.id)))?;
                            if existing.scenario_id != policy.scenario_id
                                || existing.wallet_ids != policy.wallet_ids
                                || existing.budget_unit != policy.budget_unit
                                || existing.created_at != policy.created_at
                                || existing.updated_at != expected_updated_at
                                || policy.updated_at <= expected_updated_at
                            {
                                return Err(crate::Error::conflict(
                                    "payment policy identity or revision changed",
                                ));
                            }
                            Self::lock_wallets(connection, &policy.wallet_ids).await?;
                            Self::validate_wallet_bindings(connection, &policy).await?;
                            let encoded = EncodedPolicy::new(&policy)?;
                            let result = sqlx::query(
                                "UPDATE pay_policy SET protocols = $1, networks = $2, assets = $3, \
                     allowed_recipients = $4, allowed_origins = $5, \
                     inbound_max_per_payment = $6, inbound_max_per_day = $7, \
                     inbound_approval_threshold = $8, inbound_approval_enabled = $9, \
                     inbound_approval_overage = $10, outbound_max_per_payment = $11, \
                     outbound_max_per_day = $12, outbound_approval_threshold = $13, \
                     outbound_approval_enabled = $14, outbound_approval_overage = $15, \
                     enabled = $16, updated_at = $17 WHERE id = $18 AND updated_at = $19",
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
                            .bind(policy.enabled)
                            .bind(policy.updated_at)
                            .bind(&policy.id)
                            .bind(expected_updated_at)
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| {
                                crate::Error::conflict(format!("payment policy update rejected: {error}"))
                            })?;
                            if result.rows_affected() != 1 {
                                return Err(crate::Error::conflict("payment policy revision changed"));
                            }
                            Ok(policy)
                        })
                    })
                    .await
            }

            async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>> {
                Self::list_policies(
                    self,
                    "p.scenario_id = $1",
                    scenario_id,
                    "SELECT count(*) FROM pay_policy p WHERE p.scenario_id = $1",
                    page,
                )
                .await
            }

            async fn list_for_wallet(&self, wallet_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>> {
                Self::list_policies(
                    self,
                    "EXISTS (SELECT 1 FROM pay_policy_wallet m WHERE m.policy_id = p.id AND m.wallet_id = $1)",
                    wallet_id,
                    "SELECT count(*) FROM pay_policy p WHERE EXISTS \
             (SELECT 1 FROM pay_policy_wallet m WHERE m.policy_id = p.id AND m.wallet_id = $1)",
                    page,
                )
                .await
            }

            async fn has_enabled_wallet_binding(&self, scenario_id: &str, wallet_id: &str) -> Result<bool> {
                sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pay_policy p JOIN pay_policy_wallet m ON m.policy_id = p.id \
             WHERE p.scenario_id = $1 AND m.wallet_id = $2 AND p.enabled)",
                )
                .bind(scenario_id)
                .bind(wallet_id)
                .fetch_one(self.base.pool())
                .await
                .map_err(|error| BaseRepository::storage_error("check scenario wallet policy binding", error))
            }
        }
    };
}
pub(crate) use policy_operations;
