use super::{
    crypto, ApproverSpec, Arc, ClientSpec, Error, ImportBatch, ImportPorts, ImportReport, PolicySpec, PriceSpec,
    ResourceImporter, Result, ScenarioSpec, SecretReader, StoragePorts, Wallet, WalletCreationConfig, WalletCustody,
    WalletKeyManager, WalletSource, WalletSpec, Zeroizing,
};

impl ResourceImporter {
    pub(crate) fn import_ports(storage: StoragePorts, local_keys: Arc<dyn WalletKeyManager>) -> ImportPorts {
        ImportPorts {
            clients: storage.clients,
            scenarios: storage.scenarios,
            policies: storage.policies,
            prices: storage.prices,
            approvers: storage.approvers,
            wallets: storage.wallets,
            local_keys,
        }
    }

    pub(crate) async fn apply_batch(ports: &ImportPorts, batch: ImportBatch) -> Result<ImportReport> {
        let imported_at = time::OffsetDateTime::now_utc().unix_timestamp();
        if imported_at < 0 {
            return Err(Error::InvalidTransition(
                "console import requires a non-negative UTC timestamp".to_owned(),
            ));
        }
        let mut report = ImportReport::default();

        ResourceImporter::import_clients(ports, batch.clients, imported_at, &mut report).await?;
        ResourceImporter::import_wallets(ports, batch.wallets, imported_at, &mut report).await?;
        ResourceImporter::import_scenarios(ports, batch.scenarios, imported_at, &mut report).await?;
        ResourceImporter::import_policies(ports, batch.policies, imported_at, &mut report).await?;
        ResourceImporter::import_prices(ports, batch.prices, imported_at, &mut report).await?;
        ResourceImporter::import_approvers(ports, batch.approvers, imported_at, &mut report).await?;
        Ok(report)
    }

    pub(crate) async fn import_clients(
        ports: &ImportPorts,
        specs: Vec<ClientSpec>,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        for spec in specs {
            let desired = spec.into_domain(imported_at);
            match ports.clients.get(&desired.id).await? {
                Some(existing) if ResourceImporter::same_client(&existing, &desired) => report.reused(),
                Some(_) => return Err(ResourceImporter::drift("client", &desired.id)),
                None => {
                    ports.clients.insert(&desired).await?;
                    report.created();
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn import_wallets(
        ports: &ImportPorts,
        specs: Vec<WalletSpec>,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        for spec in specs {
            ResourceImporter::import_wallet(ports, spec, imported_at, report).await?;
        }
        Ok(())
    }

    pub(crate) async fn import_scenarios(
        ports: &ImportPorts,
        specs: Vec<ScenarioSpec>,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        for spec in specs {
            let desired = spec.into_domain(imported_at);
            match ports.scenarios.get(&desired.id).await? {
                Some(existing) if ResourceImporter::same_scenario(&existing, &desired) => report.reused(),
                Some(_) => return Err(ResourceImporter::drift("scenario", &desired.id)),
                None => {
                    ports.scenarios.register(&desired).await?;
                    report.created();
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn import_policies(
        ports: &ImportPorts,
        specs: Vec<PolicySpec>,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        for spec in specs {
            let desired = spec.into_domain(imported_at);
            desired
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid imported policy {field}")))?;
            match ports.policies.get(&desired.id).await? {
                Some(existing) if ResourceImporter::same_policy(&existing, &desired) => report.reused(),
                Some(_) => return Err(ResourceImporter::drift("policy", &desired.id)),
                None => {
                    ports.policies.insert(&desired).await?;
                    report.created();
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn import_prices(
        ports: &ImportPorts,
        specs: Vec<PriceSpec>,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        for spec in specs {
            let desired = spec.into_domain(imported_at);
            desired
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid imported price {field}")))?;
            match ports.prices.get(&desired.id).await? {
                Some(existing) if ResourceImporter::same_price(&existing, &desired) => report.reused(),
                Some(_) => return Err(ResourceImporter::drift("price", &desired.id)),
                None => {
                    ports.prices.insert(&desired).await?;
                    report.created();
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn import_approvers(
        ports: &ImportPorts,
        specs: Vec<ApproverSpec>,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        for spec in specs {
            let desired = spec.into_domain(imported_at);
            match ports
                .approvers
                .get(&desired.scenario_id, &desired.auth_principal)
                .await?
            {
                Some(existing) if existing.enabled == desired.enabled => report.reused(),
                Some(_) => {
                    return Err(ResourceImporter::drift(
                        "scenario approver",
                        &format!("{}/{}", desired.scenario_id, desired.auth_principal),
                    ));
                }
                None => {
                    ports.approvers.upsert(&desired).await?;
                    report.created();
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn import_wallet(
        ports: &ImportPorts,
        spec: WalletSpec,
        imported_at: i64,
        report: &mut ImportReport,
    ) -> Result<()> {
        let (expected, private_key) = ResourceImporter::wallet_material(&spec, imported_at)?;
        if let Some(existing) = ports.wallets.get_payment_wallet(&spec.id).await? {
            if !ResourceImporter::same_wallet(&existing.wallet, &expected)
            || existing.custody != ResourceImporter::wallet_custody(&spec.source)
            || existing.enabled != spec.enabled
            // A policy-wallet binding may derive the wallet denomination after
            // an import whose wallet resource did not declare aggregate limits.
            // Explicit wallet configuration remains authoritative and must
            // still match exactly.
            || spec
                .budget_unit
                .as_ref()
                .is_some_and(|unit| existing.budget_unit.as_ref() != Some(unit))
            || existing.max_per_payment != spec.max_per_payment
            || existing.max_per_day != spec.max_per_day
            || (matches!(&spec.source, WalletSource::Local { .. }) && existing.active_key_version.is_none())
            {
                return Err(ResourceImporter::drift("wallet", &spec.id));
            }
            report.reused();
            return Ok(());
        }

        let config = WalletCreationConfig {
            enabled: spec.enabled,
            budget_unit: spec.budget_unit.clone(),
            max_per_payment: spec.max_per_payment,
            max_per_day: spec.max_per_day,
        };
        match &spec.source {
            WalletSource::Local { algorithm, .. } => {
                let private_key = private_key
                    .ok_or_else(|| Error::InvalidTransition("local wallet import key is unavailable".to_owned()))?;
                ports
                    .local_keys
                    .create_local(&spec.id, algorithm, &private_key, &config, imported_at)
                    .await?;
            }
            WalletSource::External { .. } => {
                ports.wallets.create_external_configured(&expected, &config).await?;
            }
        }
        report.created();
        Ok(())
    }

    pub(crate) fn wallet_material(spec: &WalletSpec, created_at: i64) -> Result<(Wallet, Option<Zeroizing<Vec<u8>>>)> {
        match &spec.source {
            WalletSource::Local {
                algorithm,
                private_key_env,
            } => {
                let private_key = ResourceImporter::private_key_from_env(private_key_env, algorithm)?;
                let (address, public_key) = crypto::WalletCrypto::payment_key_metadata(algorithm, &private_key)?;
                let wallet = spec.domain_wallet(address, public_key, created_at);
                Ok((wallet, Some(private_key)))
            }
            WalletSource::External {
                algorithm: _,
                address,
                public_key,
            } => {
                let wallet = spec
                    .domain_wallet(address.clone(), public_key.clone(), created_at)
                    .canonicalized()
                    .map_err(|field| Error::InvalidRequest(format!("invalid external wallet {field}")))?;
                Ok((wallet, None))
            }
        }
    }

    pub(crate) fn private_key_from_env(variable: &str, algorithm: &str) -> Result<Zeroizing<Vec<u8>>> {
        let encoded = SecretReader::read_env_value(variable.trim(), "imported wallet private key", 1024)?;
        let encoded = encoded.strip_prefix("0x").unwrap_or(encoded.as_str());
        let decoded = Zeroizing::new(
            hex::decode(encoded)
                .map_err(|_| Error::InvalidKey(format!("private key in {variable} is not hexadecimal")))?,
        );
        crypto::WalletCrypto::validate_private_key_for_algorithm(algorithm, &decoded)?;
        Ok(decoded)
    }

    pub(crate) const fn wallet_custody(source: &WalletSource) -> WalletCustody {
        match source {
            WalletSource::Local { .. } => WalletCustody::Local,
            WalletSource::External { .. } => WalletCustody::External,
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/resource/apply_test.rs"]
mod tests;
