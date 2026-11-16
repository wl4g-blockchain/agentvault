macro_rules! wallet_rows {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
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
pub(crate) use wallet_rows;
