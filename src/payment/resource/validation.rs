use super::{
    Amount, BTreeSet, Error, ImportBatch, PaymentClient, PaymentPolicy, PaymentPrice, PaymentScenario,
    ResourceImporter, Result, Wallet, WalletSource, WalletSpec,
};

impl ResourceImporter {
    pub(crate) fn validate_batch(batch: &ImportBatch) -> Result<()> {
        ResourceImporter::unique(&batch.clients, |item| item.id.as_str(), "client id")?;
        ResourceImporter::unique(&batch.clients, |item| item.auth_principal.as_str(), "client principal")?;
        ResourceImporter::unique(&batch.wallets, |item| item.id.as_str(), "wallet id")?;
        ResourceImporter::unique(&batch.scenarios, |item| item.id.as_str(), "scenario id")?;
        ResourceImporter::unique(&batch.policies, |item| item.id.as_str(), "policy id")?;
        ResourceImporter::unique(&batch.prices, |item| item.id.as_str(), "price id")?;
        let mut approvers = BTreeSet::new();
        for approver in &batch.approvers {
            if !approvers.insert((&approver.scenario_id, &approver.auth_principal)) {
                return Err(Error::InvalidRequest(
                    "duplicate scenario approver in console import".to_owned(),
                ));
            }
        }
        for wallet in &batch.wallets {
            ResourceImporter::validate_wallet_spec(wallet)?;
        }
        for policy in &batch.policies {
            ResourceImporter::unique_sorted_strings(&policy.wallet_ids, "policy wallet id")?;
            ResourceImporter::unique_sorted_strings(&policy.protocols, "policy protocol")?;
            ResourceImporter::unique_sorted_strings(&policy.networks, "policy network")?;
            ResourceImporter::unique_sorted_strings(&policy.allowed_recipients, "policy recipient")?;
            ResourceImporter::unique_sorted_strings(&policy.allowed_origins, "policy origin")?;
            if policy.assets.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(Error::InvalidRequest(
                    "duplicate policy asset in console import".to_owned(),
                ));
            }
        }
        for price in &batch.prices {
            ResourceImporter::unique_sorted_strings(&price.protocols, "price protocol")?;
        }
        Ok(())
    }

    pub(crate) fn normalize_batch(batch: &mut ImportBatch) {
        batch.clients.sort_by(|left, right| left.id.cmp(&right.id));
        batch.wallets.sort_by(|left, right| left.id.cmp(&right.id));
        batch.scenarios.sort_by(|left, right| left.id.cmp(&right.id));
        batch.policies.sort_by(|left, right| left.id.cmp(&right.id));
        batch.prices.sort_by(|left, right| left.id.cmp(&right.id));
        batch.approvers.sort_by(|left, right| {
            (&left.scenario_id, &left.auth_principal).cmp(&(&right.scenario_id, &right.auth_principal))
        });
        for policy in &mut batch.policies {
            policy.wallet_ids.sort();
            policy.protocols.sort();
            policy.networks.sort();
            policy.allowed_recipients.sort();
            policy.allowed_origins.sort();
            policy.assets.sort_by(|left, right| {
                (&left.network, &left.asset, &left.token_contract, left.decimals).cmp(&(
                    &right.network,
                    &right.asset,
                    &right.token_contract,
                    right.decimals,
                ))
            });
        }
        for price in &mut batch.prices {
            price.protocols.sort();
        }
    }

    pub(crate) fn unique_sorted_strings(values: &[String], description: &str) -> Result<()> {
        if values.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::InvalidRequest(format!(
                "duplicate {description} in console import"
            )));
        }
        Ok(())
    }

    pub(crate) fn unique<'a, T>(items: &'a [T], key: impl Fn(&'a T) -> &'a str, description: &str) -> Result<()> {
        let mut seen = BTreeSet::new();
        if items.iter().any(|item| !seen.insert(key(item))) {
            return Err(Error::InvalidRequest(format!(
                "duplicate {description} in console import"
            )));
        }
        Ok(())
    }

    pub(crate) fn validate_wallet_spec(wallet: &WalletSpec) -> Result<()> {
        if wallet.id.trim().is_empty() {
            return Err(Error::InvalidRequest("imported wallet id is empty".to_owned()));
        }
        match &wallet.source {
            WalletSource::Local {
                algorithm,
                private_key_env,
            } if private_key_env.trim().is_empty()
                || (!crate::model::wallet::is_evm_wallet_algorithm(algorithm)
                    && !crate::model::wallet::is_solana_wallet_algorithm(algorithm)) =>
            {
                return Err(Error::InvalidRequest(
                    "local wallet algorithm or private_key_env is invalid".to_owned(),
                ));
            }
            WalletSource::External { algorithm, address, .. }
                if algorithm.trim().is_empty() || address.trim().is_empty() =>
            {
                return Err(Error::InvalidRequest(
                    "external wallet algorithm or address is empty".to_owned(),
                ));
            }
            _ => {}
        }
        if let Some(unit) = &wallet.budget_unit {
            unit.validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid wallet budget unit {field}")))?;
        } else if wallet.max_per_payment.is_some() || wallet.max_per_day.is_some() {
            return Err(Error::InvalidRequest("wallet limits require a budget_unit".to_owned()));
        }
        if wallet.max_per_payment.is_some_and(Amount::is_zero)
            || wallet.max_per_day.is_some_and(Amount::is_zero)
            || wallet
                .max_per_payment
                .zip(wallet.max_per_day)
                .is_some_and(|(payment, day)| payment > day)
        {
            return Err(Error::InvalidRequest("invalid wallet limits".to_owned()));
        }
        Ok(())
    }

    pub(crate) fn same_client(left: &PaymentClient, right: &PaymentClient) -> bool {
        left.id == right.id
            && left.name == right.name
            && left.auth_principal == right.auth_principal
            && left.enabled == right.enabled
    }

    pub(crate) fn same_scenario(left: &PaymentScenario, right: &PaymentScenario) -> bool {
        left.id == right.id
            && left.client_id == right.client_id
            && left.name == right.name
            && left.enabled == right.enabled
    }

    pub(crate) fn same_wallet(left: &Wallet, right: &Wallet) -> bool {
        left.id == right.id
            && left.algorithm == right.algorithm
            && left.address == right.address
            && left.public_key == right.public_key
    }

    pub(crate) fn same_policy(left: &PaymentPolicy, right: &PaymentPolicy) -> bool {
        let mut normalized = right.clone();
        normalized.created_at = left.created_at;
        normalized.updated_at = left.updated_at;
        left == &normalized
    }

    pub(crate) fn same_price(left: &PaymentPrice, right: &PaymentPrice) -> bool {
        let mut normalized = right.clone();
        normalized.created_at = left.created_at;
        normalized.updated_at = left.updated_at;
        left == &normalized
    }

    pub(crate) fn drift(kind: &str, id: &str) -> Error {
        Error::PaymentConflict(format!(
            "imported {kind} {id} exists with different authoritative content"
        ))
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/resource/validation_test.rs"]
mod tests;
