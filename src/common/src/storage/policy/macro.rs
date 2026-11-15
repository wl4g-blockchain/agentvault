//! Shared policy SQL transactions; concrete providers supply only driver types and locks.

macro_rules! policy_repository {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
use crate::{model::BudgetUnit,model::PaymentPolicy,model::PaymentPolicyLimits,storage::policy::PolicyStorageRules,storage::Page,storage::PageRequest,storage::IPolicyRepository,storage::codec::AmountEncoding as _,Error,Result};
use async_trait::async_trait;
use sqlx::Row as _;

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

    async fn update(
        &self,
        policy: &PaymentPolicy,
        expected_updated_at: i64,
    ) -> Result<PaymentPolicy> {
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

    async fn list_for_scenario(
        &self,
        scenario_id: &str,
        page: PageRequest,
    ) -> Result<Page<PaymentPolicy>> {
        Self::list_policies(
            self,
            "p.scenario_id = $1",
            scenario_id,
            "SELECT count(*) FROM pay_policy p WHERE p.scenario_id = $1",
            page,
        )
        .await
    }

    async fn list_for_wallet(
        &self,
        wallet_id: &str,
        page: PageRequest,
    ) -> Result<Page<PaymentPolicy>> {
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
struct EncodedLimits {
    max_per_payment: Option<AmountValue>,
    max_per_day: Option<AmountValue>,
    approval_threshold: Option<AmountValue>,
    approval_enabled: bool,
    approval_overage: Option<AmountValue>,
}
impl EncodedLimits {
    fn new(limits: Option<&PaymentPolicyLimits>) -> Result<Self> {
        limits.map_or_else(
            || {
                Ok(Self {
                    max_per_payment: None,
                    max_per_day: None,
                    approval_threshold: None,
                    approval_enabled: true,
                    approval_overage: None,
                })
            },
            |limits| {
                Ok(Self {
                    max_per_payment: Some(AmountCodec::encode_amount(limits.max_per_payment)?),
                    max_per_day: Some(AmountCodec::encode_amount(limits.max_per_day)?),
                    approval_threshold: AmountCodec::encode_optional_amount(
                        limits.approval_threshold,
                    )?,
                    approval_enabled: limits.approval_enabled,
                    approval_overage: AmountCodec::encode_optional_amount(limits.approval_overage)?,
                })
            },
        )
    }
}
struct EncodedPolicy {
    protocols: JsonValue,
    networks: JsonValue,
    assets: JsonValue,
    allowed_recipients: JsonValue,
    allowed_origins: JsonValue,
    inbound: EncodedLimits,
    outbound: EncodedLimits,
}
impl EncodedPolicy {
    fn new(policy: &PaymentPolicy) -> Result<Self> {
        Ok(Self {
            protocols: BaseRepository::encode_json(&policy.protocols, "pay_policy.protocols")?,
            networks: BaseRepository::encode_json(&policy.networks, "pay_policy.networks")?,
            assets: BaseRepository::encode_json(&policy.assets, "pay_policy.assets")?,
            allowed_recipients: BaseRepository::encode_json(
                &policy.allowed_recipients,
                "pay_policy.allowed_recipients",
            )?,
            allowed_origins: BaseRepository::encode_json(
                &policy.allowed_origins,
                "pay_policy.allowed_origins",
            )?,
            inbound: EncodedLimits::new(policy.inbound_limits.as_ref())?,
            outbound: EncodedLimits::new(policy.outbound_limits.as_ref())?,
        })
    }
}
impl $repository {
    pub(crate) fn validate_policy(policy: &PaymentPolicy) -> Result<()> {
        PolicyStorageRules::validate_policy(policy)
    }

    async fn insert_policy(connection: &mut Connection, policy: &PaymentPolicy) -> Result<()> {
        let encoded = EncodedPolicy::new(policy)?;
        sqlx::query(
            "INSERT INTO pay_policy (id, scenario_id, protocols, networks, assets, budget_asset, budget_decimals, \
     allowed_recipients, allowed_origins, inbound_max_per_payment, inbound_max_per_day, \
     inbound_approval_threshold, inbound_approval_enabled, inbound_approval_overage, \
     outbound_max_per_payment, outbound_max_per_day, outbound_approval_threshold, \
     outbound_approval_enabled, outbound_approval_overage, enabled, created_at, updated_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22)",
        )
        .bind(&policy.id)
        .bind(&policy.scenario_id)
        .bind(encoded.protocols)
        .bind(encoded.networks)
        .bind(encoded.assets)
        .bind(&policy.budget_unit.asset)
        .bind(i16::from(policy.budget_unit.decimals))
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
        .bind(policy.created_at)
        .bind(policy.updated_at)
        .execute(connection)
        .await
        .map_err(|error| crate::Error::conflict(format!("payment policy insert rejected: {error}")))?;
        Ok(())
    }

    pub(crate) async fn insert_policy_aggregate(
        connection: &mut Connection,
        policy: &PaymentPolicy,
    ) -> Result<()> {
        // Establish the policy before locking wallets. Every payment path
        // uses the same policy -> wallet order, preventing deadlocks.
        Self::insert_policy(connection, policy).await?;
        Self::lock_wallets(connection, &policy.wallet_ids).await?;
        Self::validate_wallet_bindings(connection, policy).await?;
        for wallet_id in &policy.wallet_ids {
            sqlx::query("INSERT INTO pay_policy_wallet (policy_id, wallet_id, created_at) VALUES ($1, $2, $3)")
                .bind(&policy.id)
                .bind(wallet_id)
                .bind(policy.created_at)
                .execute(&mut *connection)
                .await
                .map_err(|error| crate::Error::conflict(format!("payment policy wallet binding rejected: {error}")))?;
        }
        Ok(())
    }

    /// Validates every cross-policy wallet invariant while the wallet rows
    /// are locked in canonical order. This deliberately replaces the old
    /// DDL triggers so the full policy mutation is one observable,
    /// testable `AgentVault` transaction in both database dialects.
    async fn validate_wallet_bindings(
        connection: &mut Connection,
        policy: &PaymentPolicy,
    ) -> Result<()> {
        for wallet_id in &policy.wallet_ids {
            let row = sqlx::query(
                "SELECT budget_asset, budget_decimals, max_per_payment IS NOT NULL AS has_payment_limit, \
         max_per_day IS NOT NULL AS has_day_limit FROM pay_wallet WHERE id = $1",
            )
            .bind(wallet_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| BaseRepository::storage_error("load locked payment policy wallet", error))?;
            let budget_asset: Option<String> = row.try_get("budget_asset").map_err(|error| {
                BaseRepository::storage_error("decode wallet budget asset", error)
            })?;
            let budget_decimals: Option<i16> = row.try_get("budget_decimals").map_err(|error| {
                BaseRepository::storage_error("decode wallet budget decimals", error)
            })?;
            let has_hard_limits = row
                .try_get::<bool, _>("has_payment_limit")
                .map_err(|error| {
                    BaseRepository::storage_error("decode wallet payment limit", error)
                })?
                && row.try_get::<bool, _>("has_day_limit").map_err(|error| {
                    BaseRepository::storage_error("decode wallet daily limit", error)
                })?;

            match (budget_asset.as_deref(), budget_decimals) {
                (None, None) => {
                    Self::initialize_wallet_budget_unit(connection, wallet_id, policy).await?;
                }
                (Some(asset), Some(decimals))
                    if asset == policy.budget_unit.asset.as_str()
                        && decimals == i16::from(policy.budget_unit.decimals) => {}
                _ => {
                    return Err(crate::Error::conflict(
                        "payment policy budget unit does not match its wallet",
                    ))
                }
            }

            if !policy.enabled {
                continue;
            }
            let duplicate: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pay_policy_wallet membership \
         JOIN pay_policy other ON other.id = membership.policy_id \
         WHERE membership.wallet_id = $1 AND other.enabled \
         AND other.scenario_id = $2 AND other.id <> $3)",
            )
            .bind(wallet_id)
            .bind(&policy.scenario_id)
            .bind(&policy.id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| {
                BaseRepository::storage_error("validate unique scenario wallet policy", error)
            })?;
            if duplicate {
                return Err(crate::Error::conflict(
                    "scenario wallet already has an enabled payment policy",
                ));
            }

            let shared: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pay_policy_wallet membership \
         JOIN pay_policy other ON other.id = membership.policy_id \
         WHERE membership.wallet_id = $1 AND other.enabled \
         AND other.scenario_id <> $2 AND other.id <> $3)",
            )
            .bind(wallet_id)
            .bind(&policy.scenario_id)
            .bind(&policy.id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| {
                BaseRepository::storage_error("validate shared payment wallet limits", error)
            })?;
            if shared && !has_hard_limits {
                return Err(crate::Error::conflict(
                    "wallet shared across scenarios requires aggregate hard limits",
                ));
            }
        }
        Ok(())
    }

    async fn initialize_wallet_budget_unit(
        connection: &mut Connection,
        wallet_id: &str,
        policy: &PaymentPolicy,
    ) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE pay_wallet SET budget_asset = $1, budget_decimals = $2, \
                 updated_at = CASE WHEN updated_at + 1 > $3 THEN updated_at + 1 ELSE $3 END \
                 WHERE id = $4 AND budget_asset IS NULL AND budget_decimals IS NULL",
        )
        .bind(&policy.budget_unit.asset)
        .bind(i16::from(policy.budget_unit.decimals))
        .bind(policy.created_at)
        .bind(wallet_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| {
            crate::Error::conflict(format!("initialize wallet budget unit rejected: {error}"))
        })?;
        if changed.rows_affected() != 1 {
            return Err(crate::Error::conflict(
                "wallet budget unit changed while binding policy",
            ));
        }
        Ok(())
    }

    async fn load_policy_pool(pool: &Pool, policy_id: &str) -> Result<Option<PaymentPolicy>> {
        let row = dynamic_query!(
            select,
            crate::model::PaymentPolicy,
            crate::storage::EntityQuery::new().eq("id", policy_id)
        )?
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|error| BaseRepository::storage_error("get payment policy", error))?;
        let Some(row) = row else { return Ok(None) };
        let wallet_ids = Self::load_wallet_ids_pool(pool, policy_id).await?;
        Self::decode_row(&row, wallet_ids).map(Some)
    }

    pub(crate) async fn load_policy_connection(
        connection: &mut Connection,
        policy_id: &str,
    ) -> Result<Option<PaymentPolicy>> {
        let row = dynamic_query!(
            select,
            crate::model::PaymentPolicy,
            crate::storage::EntityQuery::new().eq("id", policy_id)
        )?
        .build()
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| BaseRepository::storage_error("get payment policy", error))?;
        let Some(row) = row else { return Ok(None) };
        let wallet_ids = Self::load_wallet_ids_connection(connection, policy_id).await?;
        Self::decode_row(&row, wallet_ids).map(Some)
    }

    async fn load_policy_for_update(
        connection: &mut Connection,
        policy_id: &str,
    ) -> Result<Option<PaymentPolicy>> {
        let row = sqlx::query(concat!(
            "SELECT * FROM pay_policy WHERE id = $1",
            $update
        ))
        .bind(policy_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| BaseRepository::storage_error("lock payment policy", error))?;
        let Some(row) = row else { return Ok(None) };
        let wallet_ids = Self::load_wallet_ids_connection(connection, policy_id).await?;
        Self::decode_row(&row, wallet_ids).map(Some)
    }

    async fn load_wallet_ids_pool(pool: &Pool, policy_id: &str) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT wallet_id FROM pay_policy_wallet WHERE policy_id = $1 ORDER BY wallet_id",
        )
        .bind(policy_id)
        .fetch_all(pool)
        .await
        .map_err(|error| BaseRepository::storage_error("load payment policy wallets", error))
    }

    async fn load_wallet_ids_connection(
        connection: &mut Connection,
        policy_id: &str,
    ) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT wallet_id FROM pay_policy_wallet WHERE policy_id = $1 ORDER BY wallet_id",
        )
        .bind(policy_id)
        .fetch_all(connection)
        .await
        .map_err(|error| BaseRepository::storage_error("load payment policy wallets", error))
    }

    async fn list_policies(
        storage: &$repository,
        predicate: &str,
        value: &str,
        count_sql: &str,
        page: PageRequest,
    ) -> Result<Page<PaymentPolicy>> {
        let (limit, offset) = BaseRepository::page_bounds(page)?;
        let total: i64 = sqlx::query_scalar(count_sql)
            .bind(value)
            .fetch_one(storage.base.pool())
            .await
            .map_err(|error| BaseRepository::storage_error("count payment policies", error))?;
        let query =
            format!("SELECT p.* FROM pay_policy p WHERE {predicate} ORDER BY p.created_at, p.id LIMIT $2 OFFSET $3");
        let rows = sqlx::query(&query)
            .bind(value)
            .bind(limit)
            .bind(offset)
            .fetch_all(storage.base.pool())
            .await
            .map_err(|error| BaseRepository::storage_error("list payment policies", error))?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let policy_id: String = row.try_get("id").map_err(|error| {
                BaseRepository::storage_error("decode payment policy id", error)
            })?;
            let wallet_ids = Self::load_wallet_ids_pool(storage.base.pool(), &policy_id).await?;
            items.push(Self::decode_row(&row, wallet_ids)?);
        }
        Ok(Page {
            items,
            total: u64::try_from(total)
                .map_err(|_| Error::Storage("negative payment policy count".to_owned()))?,
        })
    }

    pub(crate) fn decode_row(row: &DbRow, wallet_ids: Vec<String>) -> Result<PaymentPolicy> {
        Ok(PaymentPolicy {
            id: BaseRepository::field(row, "id")?,
            scenario_id: BaseRepository::field(row, "scenario_id")?,
            wallet_ids,
            protocols: BaseRepository::decode_json(
                BaseRepository::field::<JsonValue>(row, "protocols")?,
                "pay_policy.protocols",
            )?,
            networks: BaseRepository::decode_json(
                BaseRepository::field::<JsonValue>(row, "networks")?,
                "pay_policy.networks",
            )?,
            assets: BaseRepository::decode_json(
                BaseRepository::field::<JsonValue>(row, "assets")?,
                "pay_policy.assets",
            )?,
            budget_unit: BudgetUnit {
                asset: BaseRepository::field(row, "budget_asset")?,
                decimals: BaseRepository::decode_u8(
                    BaseRepository::field::<i16>(row, "budget_decimals")?,
                    "pay_policy.budget_decimals",
                )?,
            },
            allowed_recipients: BaseRepository::decode_json(
                BaseRepository::field::<JsonValue>(row, "allowed_recipients")?,
                "pay_policy.allowed_recipients",
            )?,
            allowed_origins: BaseRepository::decode_json(
                BaseRepository::field::<JsonValue>(row, "allowed_origins")?,
                "pay_policy.allowed_origins",
            )?,
            inbound_limits: Self::decode_limits(row, "inbound")?,
            outbound_limits: Self::decode_limits(row, "outbound")?,
            enabled: BaseRepository::field(row, "enabled")?,
            created_at: BaseRepository::field(row, "created_at")?,
            updated_at: BaseRepository::field(row, "updated_at")?,
        })
    }

    fn decode_limits(row: &DbRow, prefix: &str) -> Result<Option<PaymentPolicyLimits>> {
        let max_field = format!("{prefix}_max_per_payment");
        let Some(max_per_payment) = BaseRepository::field::<Option<AmountValue>>(row, &max_field)?
        else {
            return Ok(None);
        };
        let day_field = format!("{prefix}_max_per_day");
        let threshold_field = format!("{prefix}_approval_threshold");
        let enabled_field = format!("{prefix}_approval_enabled");
        let overage_field = format!("{prefix}_approval_overage");
        Ok(Some(PaymentPolicyLimits {
            max_per_payment: AmountCodec::decode_amount(
                &max_per_payment,
                &format!("pay_policy.{max_field}"),
            )?,
            max_per_day: AmountCodec::decode_amount(
                &BaseRepository::field::<AmountValue>(row, &day_field)?,
                &format!("pay_policy.{day_field}"),
            )?,
            approval_threshold: AmountCodec::decode_optional_amount(
                BaseRepository::field::<Option<AmountValue>>(row, &threshold_field)?.as_ref(),
                &format!("pay_policy.{threshold_field}"),
            )?,
            approval_enabled: BaseRepository::field(row, &enabled_field)?,
            approval_overage: AmountCodec::decode_optional_amount(
                BaseRepository::field::<Option<AmountValue>>(row, &overage_field)?.as_ref(),
                &format!("pay_policy.{overage_field}"),
            )?,
        }))
    }
}



};
}
pub(crate) use policy_repository;

#[cfg(test)]
#[path = "../../../tests/storage/policy/transactions_test.rs"]
mod tests;

// Payment budget shared across SQL providers.

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
mod budget_tests;
