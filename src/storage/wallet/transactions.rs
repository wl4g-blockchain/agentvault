//! Shared wallet SQL transactions; concrete providers supply only driver types and locks.

macro_rules! wallet_repository {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
use crate::model::wallet::Wallet;
use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::BudgetUnit;
use crate::storage::wallet::WalletStorageRules;
use crate::storage::EncryptedWalletKeyVersion;
use crate::storage::Page;
use crate::storage::PageRequest;
use crate::storage::PaymentWallet;
use crate::storage::WalletBalanceRecord;
use crate::storage::IWalletBalanceRepository;
use crate::storage::WalletCreationConfig;
use crate::storage::WalletCustody;
use crate::storage::WalletKeyStatus;
use crate::storage::IWalletRepository;
use crate::utils::storage::AmountEncoding as _;
use crate::utils::storage::PaymentStorageCodec;
use crate::Error;
use crate::Result;
use async_trait::async_trait;
use sqlx::Row as _;

const PAYMENT_WALLET_SELECT: &str =
    "SELECT w.*, active.version AS active_key_version FROM pay_wallet w \
     LEFT JOIN wallet_key_version active ON active.wallet_id = w.id AND active.status = 'active'";
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
#[async_trait]
impl IWalletBalanceRepository for $repository {
    async fn upsert_balance(&self, balance: &WalletBalanceRecord) -> Result<WalletBalanceRecord> {
        balance
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid wallet balance {field}")))?;
        let asset_available = AmountCodec::encode_amount(balance.asset_available)?;
        let native_available = AmountCodec::encode_amount(balance.native_available)?;
        let result = sqlx::query(
            "INSERT INTO pay_wallet_balance \
             (wallet_id, network, asset, token_contract, asset_decimals, asset_available, \
              native_available, observed_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT(wallet_id, network, asset, token_contract, asset_decimals) DO UPDATE SET \
               asset_available = EXCLUDED.asset_available, native_available = EXCLUDED.native_available, \
               observed_at = EXCLUDED.observed_at, updated_at = EXCLUDED.updated_at \
             WHERE EXCLUDED.observed_at >= pay_wallet_balance.observed_at",
        )
        .bind(&balance.wallet_id)
        .bind(&balance.asset.network)
        .bind(&balance.asset.asset)
        .bind(balance.asset.token_contract.as_deref().unwrap_or(""))
        .bind(i16::from(balance.asset.decimals))
        .bind(asset_available)
        .bind(native_available)
        .bind(balance.observed_at)
        .bind(balance.updated_at)
        .execute(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("upsert wallet balance", error))?;
        if result.rows_affected() != 1 {
            return Err(crate::Error::conflict(
                "wallet balance observation moved backwards",
            ));
        }
        Ok(balance.clone())
    }

    async fn get_balance(
        &self,
        wallet_id: &str,
        asset: &AssetIdentity,
    ) -> Result<Option<WalletBalanceRecord>> {
        asset.validate().map_err(|field| {
            Error::InvalidRequest(format!("invalid wallet balance asset {field}"))
        })?;
        let row = sqlx::query(
            "SELECT asset_available, native_available, observed_at, updated_at \
             FROM pay_wallet_balance WHERE wallet_id = $1 AND network = $2 AND asset = $3 \
             AND token_contract = $4 AND asset_decimals = $5",
        )
        .bind(wallet_id)
        .bind(&asset.network)
        .bind(&asset.asset)
        .bind(asset.token_contract.as_deref().unwrap_or(""))
        .bind(i16::from(asset.decimals))
        .fetch_optional(self.base.pool())
        .await
        .map_err(|error| BaseRepository::storage_error("get wallet balance", error))?;
        row.map(|row| {
            let asset_available: AmountValue = row.try_get("asset_available").map_err(|error| {
                BaseRepository::storage_error("decode wallet asset balance", error)
            })?;
            let native_available: AmountValue =
                row.try_get("native_available").map_err(|error| {
                    BaseRepository::storage_error("decode wallet native balance", error)
                })?;
            Ok(WalletBalanceRecord {
                wallet_id: wallet_id.to_owned(),
                asset: asset.clone(),
                asset_available: AmountCodec::decode_amount(
                    &asset_available,
                    "pay_wallet_balance.asset_available",
                )?,
                native_available: AmountCodec::decode_amount(
                    &native_available,
                    "pay_wallet_balance.native_available",
                )?,
                observed_at: row.try_get("observed_at").map_err(|error| {
                    BaseRepository::storage_error("decode wallet balance observed_at", error)
                })?,
                updated_at: row.try_get("updated_at").map_err(|error| {
                    BaseRepository::storage_error("decode wallet balance updated_at", error)
                })?,
            })
        })
        .transpose()
    }
}
impl $repository {
    pub(crate) async fn load_payment_wallet_connection(
        connection: &mut Connection,
        wallet_id: &str,
    ) -> Result<Option<PaymentWallet>> {
        let query = format!("{PAYMENT_WALLET_SELECT} WHERE w.id = $1");
        let row = sqlx::query(&query)
            .bind(wallet_id)
            .fetch_optional(connection)
            .await
            .map_err(|error| BaseRepository::storage_error("get payment wallet", error))?;
        Self::decode_payment_wallet(row.as_ref())
    }

    fn decode_payment_wallet(row: Option<&DbRow>) -> Result<Option<PaymentWallet>> {
        row.map(Self::decode_row)
            .transpose()?
            .map(Self::canonical_payment_wallet)
            .transpose()
    }

    fn canonical_payment_wallet(wallet: PaymentWallet) -> Result<PaymentWallet> {
        wallet
            .canonicalized()
            .map_err(|field| Error::Storage(format!("stored payment wallet has invalid {field}")))
    }

    fn validate_limits(
        budget_unit: &BudgetUnit,
        max_per_payment: Option<Amount>,
        max_per_day: Option<Amount>,
    ) -> Result<()> {
        budget_unit.validate().map_err(|field| {
            Error::InvalidRequest(format!("invalid wallet budget unit {field}"))
        })?;
        if max_per_payment.is_some_and(Amount::is_zero)
            || max_per_day.is_some_and(Amount::is_zero)
            || max_per_payment
                .zip(max_per_day)
                .is_some_and(|(payment, day)| payment > day)
        {
            return Err(Error::InvalidRequest(
                "invalid wallet spending limits".to_owned(),
            ));
        }
        Ok(())
    }

    async fn insert_wallet(
        connection: &mut Connection,
        wallet: &Wallet,
        custody: WalletCustody,
        config: &WalletCreationConfig,
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
        let max_per_payment = AmountCodec::encode_optional_amount(config.max_per_payment)?;
        let max_per_day = AmountCodec::encode_optional_amount(config.max_per_day)?;
        sqlx::query(
            "INSERT INTO pay_wallet \
     (id, custody_type, algorithm, address, public_key, enabled, budget_asset, budget_decimals, \
      max_per_payment, max_per_day, created_at, updated_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
        )
        .bind(&wallet.id)
        .bind(custody)
        .bind(&wallet.algorithm)
        .bind(&wallet.address)
        .bind(public_key)
        .bind(config.enabled)
        .bind(config.budget_unit.as_ref().map(|unit| unit.asset.as_str()))
        .bind(
            config
                .budget_unit
                .as_ref()
                .map(|unit| i16::from(unit.decimals)),
        )
        .bind(max_per_payment)
        .bind(max_per_day)
        .bind(wallet.created_at)
        .execute(connection)
        .await
        .map_err(|error| {
            crate::Error::conflict(format!("payment wallet insert rejected: {error}"))
        })?;
        Ok(())
    }

    pub(crate) async fn insert_key(
        connection: &mut Connection,
        key: &EncryptedWalletKeyVersion,
    ) -> Result<()> {
        let wallet = sqlx::query(concat!(
            "SELECT custody_type, algorithm, address, public_key FROM pay_wallet WHERE id = $1",
            $share
        ))
        .bind(&key.wallet_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| {
            BaseRepository::storage_error("validate encrypted wallet key owner", error)
        })?
        .ok_or_else(|| Error::NotFound(format!("payment wallet {}", key.wallet_id)))?;
        let custody: String = wallet
            .try_get("custody_type")
            .map_err(|error| BaseRepository::storage_error("decode wallet custody", error))?;
        let algorithm: String = wallet
            .try_get("algorithm")
            .map_err(|error| BaseRepository::storage_error("decode wallet algorithm", error))?;
        let address: String = wallet
            .try_get("address")
            .map_err(|error| BaseRepository::storage_error("decode wallet address", error))?;
        let public_key: Option<String> = wallet
            .try_get("public_key")
            .map_err(|error| BaseRepository::storage_error("decode wallet public key", error))?;
        if custody != "local"
            || (key.status == WalletKeyStatus::Active
                && (algorithm != key.algorithm
                    || address != key.address
                    || public_key.as_deref() != Some(key.public_key.as_str())))
        {
            return Err(crate::Error::conflict(
                "key version does not match a local-custody wallet",
            ));
        }
        sqlx::query(
    "INSERT INTO wallet_key_version (wallet_id, version, algorithm, address, public_key, encryption_scheme, \
     kek_id, wrapped_data_key, wrap_nonce, key_ciphertext, key_nonce, aad, status, activated_at, retired_at, created_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
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
.map_err(|error| crate::Error::conflict(format!("encrypted wallet key insert rejected: {error}")))?;
        Ok(())
    }

    async fn lock_bound_policies(
        connection: &mut Connection,
        wallet_id: &str,
    ) -> Result<Vec<String>> {
        sqlx::query_scalar(concat!(
            "SELECT policy.id FROM pay_policy policy \
     JOIN pay_policy_wallet membership ON membership.policy_id = policy.id \
     WHERE membership.wallet_id = $1 ORDER BY policy.id",
            $policy_lock
        ))
        .bind(wallet_id)
        .fetch_all(connection)
        .await
        .map_err(|error| BaseRepository::storage_error("lock wallet payment policies", error))
    }

    async fn bound_policy_ids(connection: &mut Connection, wallet_id: &str) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT policy_id FROM pay_policy_wallet WHERE wallet_id = $1 ORDER BY policy_id",
        )
        .bind(wallet_id)
        .fetch_all(connection)
        .await
        .map_err(|error| BaseRepository::storage_error("revalidate wallet payment policies", error))
    }

    pub(crate) fn decode_row(row: &DbRow) -> Result<PaymentWallet> {
        let public_key: Option<String> = BaseRepository::field(row, "public_key")?;
        let budget_asset: Option<String> = BaseRepository::field(row, "budget_asset")?;
        let budget_decimals: Option<i16> = BaseRepository::field(row, "budget_decimals")?;
        let budget_unit = match (budget_asset, budget_decimals) {
            (None, None) => None,
            (Some(asset), Some(decimals)) => Some(BudgetUnit {
                asset,
                decimals: BaseRepository::decode_u8(decimals, "pay_wallet.budget_decimals")?,
            }),
            _ => {
                return Err(Error::Storage(
                    "PostgreSQL wallet contains an incomplete budget unit".to_owned(),
                ))
            }
        };
        Ok(PaymentWallet {
            wallet: crate::model::wallet::Wallet {
                id: BaseRepository::field(row, "id")?,
                algorithm: BaseRepository::field(row, "algorithm")?,
                address: BaseRepository::field(row, "address")?,
                public_key: public_key.unwrap_or_default(),
                created_at: BaseRepository::field(row, "created_at")?,
            },
            custody: PaymentStorageCodec::custody(&BaseRepository::field::<String>(
                row,
                "custody_type",
            )?)?,
            enabled: BaseRepository::field(row, "enabled")?,
            active_key_version: BaseRepository::field::<Option<i64>>(row, "active_key_version")?
                .map(|value| BaseRepository::decode_u32(value, "wallet_key_version.version"))
                .transpose()?,
            budget_unit,
            max_per_payment: AmountCodec::decode_optional_amount(
                BaseRepository::field::<Option<AmountValue>>(row, "max_per_payment")?.as_ref(),
                "pay_wallet.max_per_payment",
            )?,
            max_per_day: AmountCodec::decode_optional_amount(
                BaseRepository::field::<Option<AmountValue>>(row, "max_per_day")?.as_ref(),
                "pay_wallet.max_per_day",
            )?,
            updated_at: BaseRepository::field(row, "updated_at")?,
        })
    }

    pub(crate) fn decode_key_row(row: &DbRow) -> Result<EncryptedWalletKeyVersion> {
        Ok(EncryptedWalletKeyVersion {
            wallet_id: BaseRepository::field(row, "wallet_id")?,
            version: BaseRepository::decode_u32(
                BaseRepository::field(row, "version")?,
                "wallet_key_version.version",
            )?,
            algorithm: BaseRepository::field(row, "algorithm")?,
            address: BaseRepository::field(row, "address")?,
            public_key: BaseRepository::field(row, "public_key")?,
            encryption_scheme: BaseRepository::field(row, "encryption_scheme")?,
            kek_id: BaseRepository::field(row, "kek_id")?,
            wrapped_data_key: BaseRepository::field(row, "wrapped_data_key")?,
            wrap_nonce: BaseRepository::field(row, "wrap_nonce")?,
            key_ciphertext: BaseRepository::field(row, "key_ciphertext")?,
            key_nonce: BaseRepository::field(row, "key_nonce")?,
            aad: BaseRepository::field(row, "aad")?,
            status: PaymentStorageCodec::key_status(&BaseRepository::field::<String>(
                row, "status",
            )?)?,
            activated_at: BaseRepository::field(row, "activated_at")?,
            retired_at: BaseRepository::field(row, "retired_at")?,
            created_at: BaseRepository::field(row, "created_at")?,
        })
    }
}



};
}
pub(crate) use wallet_repository;

#[cfg(test)]
#[path = "../../../tests/storage/wallet/transactions_test.rs"]
mod tests;
