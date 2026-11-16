macro_rules! wallet_operations {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
#[async_trait]
impl IWalletRepository for $repository {
    async fn create_local(
        &self,
        wallet: &Wallet,
        key: &EncryptedWalletKeyVersion,
    ) -> Result<Wallet> {
        self.create_local_configured(
            wallet,
            key,
            &WalletCreationConfig {
                enabled: true,
                ..WalletCreationConfig::default()
            },
        )
        .await
    }

    async fn create_local_configured(
        &self,
        wallet: &Wallet,
        key: &EncryptedWalletKeyVersion,
        config: &WalletCreationConfig,
    ) -> Result<Wallet> {
        let wallet = WalletStorageRules::canonical_wallet(wallet)?;
        WalletStorageRules::validate_key_for_wallet(&wallet, key, true)?;
        WalletStorageRules::validate_creation_config(config)?;
        if key.version != 1 {
            return Err(Error::InvalidKey(
                "initial wallet key version must be 1".to_owned(),
            ));
        }
        let key = key.clone();
        let config = config.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    Self::insert_wallet(connection, &wallet, WalletCustody::Local, &config).await?;
                    Self::insert_key(connection, &key).await?;
                    Ok(wallet)
                })
            })
            .await
    }

    async fn create_external(&self, wallet: &Wallet) -> Result<Wallet> {
        self.create_external_configured(
            wallet,
            &WalletCreationConfig {
                enabled: true,
                ..WalletCreationConfig::default()
            },
        )
        .await
    }

    async fn create_external_configured(
        &self,
        wallet: &Wallet,
        config: &WalletCreationConfig,
    ) -> Result<Wallet> {
        let wallet = WalletStorageRules::canonical_wallet(wallet)?;
        WalletStorageRules::validate_creation_config(config)?;
        let config = config.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    Self::insert_wallet(connection, &wallet, WalletCustody::External, &config)
                        .await?;
                    Ok(wallet)
                })
            })
            .await
    }

    async fn get(&self, wallet_id: &str) -> Result<Option<Wallet>> {
        Ok(self
            .get_payment_wallet(wallet_id)
            .await?
            .map(|payment| payment.wallet))
    }

    async fn get_payment_wallet(&self, wallet_id: &str) -> Result<Option<PaymentWallet>> {
        let query = format!("{PAYMENT_WALLET_SELECT} WHERE w.id = $1");
        let row = sqlx::query(&query)
            .bind(wallet_id)
            .fetch_optional(self.base.pool())
            .await
            .map_err(|error| BaseRepository::storage_error("get payment wallet", error))?;
        Self::decode_payment_wallet(row.as_ref())
    }

    async fn list(&self, page: PageRequest) -> Result<Page<Wallet>> {
        let (limit, offset) = BaseRepository::page_bounds(page)?;
        let total: i64 = dynamic_query!(
            count,
            crate::model::Wallet,
            crate::storage::EntityQuery::new()
        )?
        .build_query_scalar::<i64>()
        .fetch_one(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("count payment wallets", error))?;
        let query =
            format!("{PAYMENT_WALLET_SELECT} ORDER BY w.created_at, w.id LIMIT $1 OFFSET $2");
        let rows = sqlx::query(&query)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.base.pool())
            .await
            .map_err(|error| BaseRepository::storage_error("list payment wallets", error))?;
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            items.push(Self::canonical_payment_wallet(Self::decode_row(row)?)?.wallet);
        }
        Ok(Page {
            items,
            total: u64::try_from(total)
                .map_err(|_| Error::Storage("negative payment wallet count".to_owned()))?,
        })
    }

    async fn load_active_key(&self, wallet_id: &str) -> Result<Option<EncryptedWalletKeyVersion>> {
        let row = sqlx::query(
            "SELECT * FROM wallet_key_version WHERE wallet_id = $1 AND status = 'active'",
        )
        .bind(wallet_id)
        .fetch_optional(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("load active wallet key", error))?;
        row.as_ref().map(Self::decode_key_row).transpose()
    }

    async fn list_key_versions(&self, wallet_id: &str) -> Result<Vec<EncryptedWalletKeyVersion>> {
        let rows =
            sqlx::query("SELECT * FROM wallet_key_version WHERE wallet_id = $1 ORDER BY version")
                .bind(wallet_id)
                .fetch_all(self.base.pool())
                .await
                .map_err(|error| {
                    BaseRepository::storage_error("list wallet key versions", error)
                })?;
        rows.iter().map(Self::decode_key_row).collect()
    }

    async fn rotate(
        &self,
        wallet: &Wallet,
        key: &EncryptedWalletKeyVersion,
        retired_at: i64,
    ) -> Result<Wallet> {
        let wallet = WalletStorageRules::canonical_wallet(wallet)?;
        WalletStorageRules::validate_key_for_wallet(&wallet, key, true)?;
        let key = key.clone();
        self.base.transaction(|connection| {
            Box::pin(async move {
                let row = sqlx::query(
                    concat!("SELECT custody_type, created_at, updated_at FROM pay_wallet WHERE id = $1", $update),
                )
                .bind(&wallet.id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| BaseRepository::storage_error("lock wallet rotation state", error))?
                .ok_or_else(|| Error::NotFound(format!("payment wallet {}", wallet.id)))?;
                let custody: String = row
                    .try_get("custody_type")
                    .map_err(|error| BaseRepository::storage_error("decode wallet custody", error))?;
                let created_at: i64 = row
                    .try_get("created_at")
                    .map_err(|error| BaseRepository::storage_error("decode wallet creation time", error))?;
                let updated_at: i64 = row
                    .try_get("updated_at")
                    .map_err(|error| BaseRepository::storage_error("decode wallet update time", error))?;
                let active_version: Option<i64> = sqlx::query_scalar(
                    concat!("SELECT version FROM wallet_key_version WHERE wallet_id = $1 AND status = 'active'", $update),
                )
                .bind(&wallet.id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| BaseRepository::storage_error("lock active wallet key", error))?;
                let active_version = active_version
                    .and_then(|version| u32::try_from(version).ok())
                    .ok_or_else(|| crate::Error::conflict("local wallet has no active key to rotate"))?;
                if custody != "local"
                    || created_at != wallet.created_at
                    || key.version
                        != active_version
                            .checked_add(1)
                            .ok_or_else(|| crate::Error::conflict("wallet key version overflow"))?
                    || retired_at < key.activated_at
                    || key.activated_at < updated_at
                {
                    return Err(crate::Error::conflict("wallet key rotation identity or version is invalid"));
                }

                let retired = sqlx::query(
                    "UPDATE wallet_key_version SET status = 'retired', retired_at = $1 \
                     WHERE wallet_id = $2 AND version = $3 AND status = 'active'",
                )
                .bind(retired_at)
                .bind(&wallet.id)
                .bind(i64::from(active_version))
                .execute(&mut *connection)
                .await
                .map_err(|error| crate::Error::conflict(format!("retire active wallet key rejected: {error}")))?;
                if retired.rows_affected() != 1 {
                    return Err(crate::Error::conflict("active wallet key changed during rotation"));
                }
                let updated = sqlx::query(
                    "UPDATE pay_wallet SET algorithm = $1, address = $2, public_key = $3, updated_at = $4 WHERE id = $5",
                )
                .bind(&wallet.algorithm)
                .bind(&wallet.address)
                .bind(&wallet.public_key)
                .bind(key.activated_at)
                .bind(&wallet.id)
                .execute(&mut *connection)
                .await
                .map_err(|error| crate::Error::conflict(format!("update wallet key metadata rejected: {error}")))?;
                if updated.rows_affected() != 1 {
                    return Err(crate::Error::conflict("payment wallet changed during key rotation"));
                }
                Self::insert_key(connection, &key).await?;
                Ok(wallet)
            })
        })
        .await
    }

    async fn mark_key_compromised(
        &self,
        wallet_id: &str,
        version: u32,
        retired_at: i64,
    ) -> Result<()> {
        let wallet_id = wallet_id.to_owned();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "UPDATE wallet_key_version SET status = 'compromised', \
                     retired_at = CASE WHEN status = 'active' THEN $1 ELSE retired_at END \
                     WHERE wallet_id = $2 AND version = $3 \
                     AND (status = 'active' OR (status IN ('retired', 'compromised') AND retired_at = $1))",
                    )
                    .bind(retired_at)
                    .bind(&wallet_id)
                    .bind(i64::from(version))
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| {
                        crate::Error::conflict(format!("mark wallet key compromised rejected: {error}"))
                    })?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict(
                            "wallet key was not found or retirement time conflicts",
                        ));
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
        Self::validate_limits(budget_unit, max_per_payment, max_per_day)?;
        let wallet_id = wallet_id.to_owned();
        let budget_unit = budget_unit.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let policy_ids = Self::lock_bound_policies(connection, &wallet_id).await?;
                    let row = sqlx::query(
                        concat!("SELECT budget_asset, budget_decimals, updated_at FROM pay_wallet WHERE id = $1", $update),
                    )
                    .bind(&wallet_id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("lock payment wallet limits", error))?
                    .ok_or_else(|| Error::NotFound(format!("payment wallet {wallet_id}")))?;
                    let current_asset: Option<String> = row
                        .try_get("budget_asset")
                        .map_err(|error| BaseRepository::storage_error("decode wallet budget asset", error))?;
                    let current_decimals: Option<i16> = row.try_get("budget_decimals").map_err(|error| {
                        BaseRepository::storage_error("decode wallet budget decimals", error)
                    })?;
                    let current_updated_at: i64 = row
                        .try_get("updated_at")
                        .map_err(|error| BaseRepository::storage_error("decode wallet update time", error))?;
                    let revalidated = Self::bound_policy_ids(connection, &wallet_id).await?;
                    if revalidated != policy_ids
                        || current_updated_at > updated_at
                        || (!policy_ids.is_empty()
                            && (current_asset.as_deref() != Some(budget_unit.asset.as_str())
                                || current_decimals != Some(i16::from(budget_unit.decimals))))
                    {
                        return Err(crate::Error::conflict(
                            "wallet policy bindings, revision, or budget unit changed",
                        ));
                    }
                    let enabled_scenarios: i64 = sqlx::query_scalar(
                        "SELECT count(DISTINCT policy.scenario_id) FROM pay_policy_wallet membership \
                     JOIN pay_policy policy ON policy.id = membership.policy_id \
                     WHERE membership.wallet_id = $1 AND policy.enabled",
                    )
                    .bind(&wallet_id)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("count shared wallet scenarios", error))?;
                    if enabled_scenarios > 1 && (max_per_payment.is_none() || max_per_day.is_none()) {
                        return Err(crate::Error::conflict(
                            "wallet shared across scenarios requires aggregate hard limits",
                        ));
                    }
                    let max_per_payment = AmountCodec::encode_optional_amount(max_per_payment)?;
                    let max_per_day = AmountCodec::encode_optional_amount(max_per_day)?;
                    let result = sqlx::query(
                        "UPDATE pay_wallet SET budget_asset = $1, budget_decimals = $2, max_per_payment = $3, \
                     max_per_day = $4, updated_at = $5 WHERE id = $6 AND updated_at = $7",
                    )
                    .bind(&budget_unit.asset)
                    .bind(i16::from(budget_unit.decimals))
                    .bind(max_per_payment)
                    .bind(max_per_day)
                    .bind(updated_at)
                    .bind(&wallet_id)
                    .bind(current_updated_at)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| crate::Error::conflict(format!("wallet limit update rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict("payment wallet revision changed"));
                    }
                    Ok(())
                })
            })
            .await
    }

    async fn set_enabled(&self, wallet_id: &str, enabled: bool, updated_at: i64) -> Result<()> {
        let result =
            sqlx::query("UPDATE pay_wallet SET enabled = $1, updated_at = $2 WHERE id = $3 AND updated_at <= $2")
                .bind(enabled)
                .bind(updated_at)
                .bind(wallet_id)
                .execute(self.base.pool())
                .await
                .map_err(|error| BaseRepository::storage_error("update payment wallet lifecycle", error))?;
        if result.rows_affected() != 1 {
            return Err(crate::Error::conflict(
                "wallet was not found or update timestamp moved backwards",
            ));
        }
        Ok(())
    }
}
};
}
pub(crate) use wallet_operations;
