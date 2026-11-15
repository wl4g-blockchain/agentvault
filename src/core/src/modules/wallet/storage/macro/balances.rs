macro_rules! wallet_balances {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
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

            async fn get_balance(&self, wallet_id: &str, asset: &AssetIdentity) -> Result<Option<WalletBalanceRecord>> {
                asset
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid wallet balance asset {field}")))?;
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
                    let asset_available: AmountValue = row
                        .try_get("asset_available")
                        .map_err(|error| BaseRepository::storage_error("decode wallet asset balance", error))?;
                    let native_available: AmountValue = row
                        .try_get("native_available")
                        .map_err(|error| BaseRepository::storage_error("decode wallet native balance", error))?;
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
    };
}
pub(crate) use wallet_balances;
