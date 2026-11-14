//! Payment budget shared across SQL providers.

macro_rules! payment_budget {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        impl $repository {
            pub(crate) async fn enforce_budget(
                connection: &mut Connection,
                intent: &PaymentIntent,
                day_started_at: i64,
                now: i64,
                approved_overage: bool,
            ) -> Result<()> {
                let (policy_id, wallet_id) = intent
                    .authority_ids()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
                Self::lock_scenario_budget(connection, &intent.scenario_id).await?;
                Self::lock_policy_wallet(connection, policy_id, wallet_id).await?;
                let policy = PolicyRepository::load_policy_connection(connection, policy_id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("payment policy no longer exists"))?;
                let limits = policy
                    .limits_for(intent.direction)
                    .ok_or_else(|| Error::PolicyDenied("payment direction is disabled".to_owned()))?;
                if intent.amount > limits.max_per_payment {
                    return Err(Error::PolicyDenied(
                        "payment exceeds the per-payment policy limit".to_owned(),
                    ));
                }
                let snapshot = Self::budget_snapshot_connection(
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
                        return Err(Error::PolicyDenied(
                            "wallet per-payment limit is exceeded".to_owned(),
                        ));
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

            async fn lock_scenario_budget(connection: &mut Connection, scenario_id: &str) -> Result<()> {
                let present: Option<String> =
                    sqlx::query_scalar(concat!("SELECT id FROM pay_scenario WHERE id = $1", $update))
                        .bind(scenario_id)
                        .fetch_optional(connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("lock payment scenario budget", error))?;
                if present.is_none() {
                    return Err(crate::Error::conflict("payment scenario no longer exists"));
                }
                Ok(())
            }

            pub(crate) async fn budget_snapshot_connection(
                connection: &mut Connection,
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
                let wallet = WalletRepository::load_payment_wallet_connection(connection, wallet_id)
                    .await?
                    .ok_or_else(|| Error::NotFound(format!("payment wallet {wallet_id}")))?;
                if wallet.budget_unit.as_ref() != Some(budget_unit) {
                    return Err(crate::Error::conflict(
                        "wallet budget unit does not match requested aggregation",
                    ));
                }
                let shared_count: i64 = sqlx::query_scalar(
                    "SELECT count(DISTINCT p.scenario_id) FROM pay_policy_wallet m \
    JOIN pay_policy p ON p.id = m.policy_id WHERE m.wallet_id = $1 AND p.enabled = TRUE",
                )
                .bind(wallet_id)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| BaseRepository::storage_error("count wallet scenario sharing", error))?;

                let latest_effective_at = now.saturating_add(MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS);
                Self::assert_no_ledger_beyond_future_skew(
                    connection,
                    scenario_id,
                    wallet_id,
                    direction,
                    budget_unit,
                    latest_effective_at,
                )
                .await?;
                Self::assert_consumed_intents_have_ledger(connection, scenario_id, wallet_id, direction, budget_unit)
                    .await?;

                let scenario_reserved = Self::load_amounts(
                    connection,
                    "SELECT CAST(reserved_amount AS TEXT) FROM pay_intent WHERE scenario_id = $1 AND direction = $2 \
     AND reservation_status = 'reserved' AND asset = $3 AND asset_decimals = $4",
                    scenario_id,
                    direction,
                    budget_unit,
                )
                .await?;
                let wallet_reserved = Self::load_amounts(
                    connection,
                    "SELECT CAST(reserved_amount AS TEXT) FROM pay_intent WHERE wallet_id = $1 AND direction = $2 \
     AND reservation_status = 'reserved' AND asset = $3 AND asset_decimals = $4",
                    wallet_id,
                    direction,
                    budget_unit,
                )
                .await?;
                let scenario_confirmed = Self::load_ledger_amounts(
                    connection,
                    "SELECT CAST(amount AS TEXT) FROM pay_ledger WHERE scenario_id = $1 AND direction = $2 \
     AND asset = $3 AND asset_decimals = $4 AND effective_at >= $5 AND effective_at <= $6",
                    scenario_id,
                    direction,
                    budget_unit,
                    day_started_at,
                    latest_effective_at,
                )
                .await?;
                let wallet_confirmed = Self::load_ledger_amounts(
                    connection,
                    "SELECT CAST(amount AS TEXT) FROM pay_ledger WHERE wallet_id = $1 AND direction = $2 \
     AND asset = $3 AND asset_decimals = $4 AND effective_at >= $5 AND effective_at <= $6",
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
                    scenario_spent_and_reserved: PaymentStorageRules::checked_usage(
                        scenario_reserved,
                        scenario_confirmed,
                        "scenario",
                    )?,
                    wallet_spent_and_reserved: PaymentStorageRules::checked_usage(
                        wallet_reserved,
                        wallet_confirmed,
                        "wallet",
                    )?,
                    wallet_max_per_payment: wallet.max_per_payment,
                    wallet_max_per_day: wallet.max_per_day,
                })
            }

            async fn load_amounts(
                connection: &mut Connection,
                sql: &str,
                scope_id: &str,
                direction: PaymentDirection,
                unit: &BudgetUnit,
            ) -> Result<Amount> {
                let values: Vec<String> = sqlx::query_scalar(sql)
                    .bind(scope_id)
                    .bind(direction.as_str())
                    .bind(&unit.asset)
                    .bind(i16::from(unit.decimals))
                    .fetch_all(connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("load reserved payment amounts", error))?;
                Self::sum_amounts(values)
            }

            async fn load_ledger_amounts(
                connection: &mut Connection,
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
                    .bind(i16::from(unit.decimals))
                    .bind(day_started_at)
                    .bind(latest_effective_at)
                    .fetch_all(connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("load confirmed ledger amounts", error))?;
                Self::sum_amounts(values)
            }

            fn sum_amounts(values: Vec<String>) -> Result<Amount> {
                values.into_iter().try_fold(Amount::ZERO, |sum, value| {
                    let amount = value
                        .parse::<Amount>()
                        .map_err(|_| Error::Storage("PostgreSQL contains an invalid monetary amount".to_owned()))?;
                    sum.checked_add(amount)
                        .ok_or_else(|| Error::Storage("authoritative payment aggregate overflowed u128".to_owned()))
                })
            }

            async fn assert_no_ledger_beyond_future_skew(
                connection: &mut Connection,
                scenario_id: &str,
                wallet_id: &str,
                direction: PaymentDirection,
                unit: &BudgetUnit,
                latest_effective_at: i64,
            ) -> Result<()> {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pay_ledger WHERE (scenario_id = $1 OR wallet_id = $2) AND direction = $3 \
     AND asset = $4 AND asset_decimals = $5 AND effective_at > $6",
                )
                .bind(scenario_id)
                .bind(wallet_id)
                .bind(direction.as_str())
                .bind(&unit.asset)
                .bind(i16::from(unit.decimals))
                .bind(latest_effective_at)
                .fetch_one(connection)
                .await
                .map_err(|error| BaseRepository::storage_error("verify ledger effective times", error))?;
                if count != 0 {
                    return Err(Error::Storage(
                        "ledger effective time exceeds the bounded future-skew window".to_owned(),
                    ));
                }
                Ok(())
            }

            async fn assert_consumed_intents_have_ledger(
                connection: &mut Connection,
                scenario_id: &str,
                wallet_id: &str,
                direction: PaymentDirection,
                unit: &BudgetUnit,
            ) -> Result<()> {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pay_intent i WHERE (i.scenario_id = $1 OR i.wallet_id = $2) \
     AND i.direction = $3 AND i.asset = $4 AND i.asset_decimals = $5 \
     AND i.reservation_status = 'consumed' AND NOT EXISTS ( \
         SELECT 1 FROM pay_ledger l WHERE l.intent_id = i.id AND l.scenario_id = i.scenario_id \
         AND l.wallet_id = i.wallet_id AND l.direction = i.direction \
         AND l.asset = i.asset AND l.asset_decimals = i.asset_decimals)",
                )
                .bind(scenario_id)
                .bind(wallet_id)
                .bind(direction.as_str())
                .bind(&unit.asset)
                .bind(i16::from(unit.decimals))
                .fetch_one(connection)
                .await
                .map_err(|error| BaseRepository::storage_error("verify consumed payment ledger facts", error))?;
                if count != 0 {
                    return Err(Error::Storage(
                        "a consumed payment reservation has no matching durable ledger fact".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    };
}
pub(crate) use payment_budget;

#[cfg(test)]
#[path = "../../../tests/storage/payment/budget_test.rs"]
mod tests;
