//! Payment authority shared across SQL providers.

macro_rules! payment_authority {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        impl $repository {
            async fn validate_intent_authority(connection: &mut Connection, intent: &PaymentIntent) -> Result<()> {
                // Denials remain immutable audit facts. Their relational identity is
                // still enforced by foreign keys, while the rejected scope is allowed
                // to describe precisely why authorization failed.
                if intent.status == PaymentStatus::Failed {
                    return Ok(());
                }
                let (policy_id, wallet_id) = intent
                    .authority_ids()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
                let locked: Option<String> =
                    sqlx::query_scalar(concat!("SELECT id FROM pay_policy WHERE id = $1", $share))
                        .bind(policy_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("lock payment intent policy", error))?;
                if locked.is_none() {
                    return Err(Error::NotFound(format!("payment policy {policy_id}")));
                }
                let policy = PolicyRepository::load_policy_connection(connection, policy_id)
                    .await?
                    .ok_or_else(|| Error::NotFound(format!("payment policy {policy_id}")))?;
                let limit = policy
                    .limits_for(intent.direction)
                    .ok_or_else(|| crate::Error::conflict("policy does not authorize payment intent direction"))?;
                if policy.scenario_id != intent.scenario_id
                    || !policy.wallet_ids.iter().any(|allowed| allowed == wallet_id)
                    || !policy.budget_unit.matches(&intent.asset)
                    || !policy.protocols.contains(&intent.protocol)
                    || !policy.networks.contains(&intent.asset.network)
                    || !policy.assets.contains(&intent.asset)
                    || intent.amount > limit.max_per_payment
                {
                    return Err(crate::Error::conflict(
                        "payment intent exceeds or falls outside its directional policy scope",
                    ));
                }
                Ok(())
            }

            async fn lock_authorization_scope(
                connection: &mut Connection,
                client_id: &str,
                scenario_id: &str,
                policy_id: &str,
                offer_id: Option<&str>,
                wallet_id: &str,
            ) -> Result<()> {
                let client: Option<String> =
                    sqlx::query_scalar(concat!("SELECT id FROM pay_client WHERE id = $1", $share))
                        .bind(client_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("lock payment client", error))?;
                if client.is_none() {
                    return Err(Error::Forbidden);
                }
                let scenario_owner: Option<String> = sqlx::query_scalar(concat!(
                    "SELECT client_id FROM pay_scenario WHERE id = $1 AND client_id = $2",
                    $update
                ))
                .bind(scenario_id)
                .bind(client_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| BaseRepository::storage_error("lock payment scenario budget", error))?;
                if scenario_owner.is_none() {
                    return Err(Error::Forbidden);
                }
                Self::lock_policy_wallet(connection, policy_id, wallet_id).await?;
                if let Some(offer_id) = offer_id {
                    let present: Option<String> = sqlx::query_scalar(concat!(
                        "SELECT id FROM pay_price WHERE id = $1 AND policy_id = $2 AND wallet_id = $3",
                        $share
                    ))
                    .bind(offer_id)
                    .bind(policy_id)
                    .bind(wallet_id)
                    .fetch_optional(connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("lock payment offer", error))?;
                    if present.is_none() {
                        return Err(crate::Error::conflict("payment offer authority no longer exists"));
                    }
                }
                Ok(())
            }

            pub(crate) async fn lock_policy_wallet(
                connection: &mut Connection,
                policy_id: &str,
                wallet_id: &str,
            ) -> Result<()> {
                let policy_present: Option<String> =
                    sqlx::query_scalar(concat!("SELECT id FROM pay_policy WHERE id = $1", $update))
                        .bind(policy_id)
                        .fetch_optional(&mut *connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("lock payment policy", error))?;
                if policy_present.is_none() {
                    return Err(crate::Error::conflict("payment policy no longer exists"));
                }
                let wallet_present: Option<String> =
                    sqlx::query_scalar(concat!("SELECT id FROM pay_wallet WHERE id = $1", $update))
                        .bind(wallet_id)
                        .fetch_optional(connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("lock payment wallet budget", error))?;
                if wallet_present.is_none() {
                    return Err(crate::Error::conflict("payment wallet no longer exists"));
                }
                Ok(())
            }

            async fn validate_authorization_guard(
                connection: &mut Connection,
                intent: &PaymentIntent,
                guard: &AuthorizationGuard,
                action: AuthorizationAction,
            ) -> Result<()> {
                intent
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
                let (_, wallet_id) = intent
                    .authority_ids()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
                if intent.direction != PaymentDirection::Outbound
                    || intent.scenario_id != guard.scenario.id
                    || intent.policy_id.as_deref() != Some(guard.policy.id.as_str())
                    || guard.scenario.client_id != guard.client.id
                    || !guard
                        .policy
                        .wallet_ids
                        .iter()
                        .any(|candidate| candidate == wallet_id)
                {
                    return Err(crate::Error::conflict(
                        "authorization guard is not bound to the payment intent",
                    ));
                }
                Self::lock_authorization_scope(
                    connection,
                    &guard.client.id,
                    &guard.scenario.id,
                    &guard.policy.id,
                    None,
                    wallet_id,
                )
                .await?;
                let client = BaseRepository::select_by_connection::<crate::model::PaymentClient>(
                    connection,
                    crate::storage::EntityQuery::new()
                        .eq("id", &guard.client.id)
                        .eq("auth_principal", &guard.principal),
                )
                .await?
                .ok_or(Error::Forbidden)?;
                let scenario = BaseRepository::select_by_connection::<crate::model::PaymentScenario>(
                    connection,
                    crate::storage::EntityQuery::new()
                        .eq("id", &guard.scenario.id)
                        .eq("client_id", &guard.client.id),
                )
                .await?
                .ok_or(Error::Forbidden)?;
                let policy = PolicyRepository::load_policy_connection(connection, &guard.policy.id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("authorization policy no longer exists"))?;
                let wallet = WalletRepository::load_payment_wallet_connection(connection, wallet_id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("authorization wallet no longer exists"))?;
                if client != guard.client || scenario != guard.scenario || policy != guard.policy {
                    return Err(crate::Error::conflict(
                        "authorization snapshot changed before commit",
                    ));
                }
                if action != AuthorizationAction::RecordDenied {
                    if !client.authenticates(&guard.principal)
                        || !scenario.enabled
                        || !policy.enabled
                        || !wallet.signing_ready_for_payment_network(&intent.asset.network)
                        || wallet.updated_at != guard.expected_wallet_updated_at
                        || wallet.active_key_version != guard.expected_wallet_key_version
                        || wallet.budget_unit.as_ref() != Some(&policy.budget_unit)
                        || !policy.allows_wallet(wallet_id)
                        || !policy.allows_direction(intent.direction)
                        || !policy.allows_protocol(&intent.protocol)
                        || !policy.allows_asset(&intent.asset)
                        || !policy.allows_payment_recipient(&intent.asset.network, &intent.recipient)
                        || !policy.allows_origin(&guard.origin)
                        || intent
                            .expires_at
                            .is_some_and(|expires_at| expires_at <= guard.evaluated_at)
                    {
                        return Err(Error::PolicyDenied(
                            "payment authorization changed or falls outside current policy".to_owned(),
                        ));
                    }
                    let limits = policy
                        .limits_for(intent.direction)
                        .ok_or_else(|| Error::PolicyDenied("payment direction is disabled".to_owned()))?;
                    if intent.amount > limits.max_per_payment {
                        return Err(Error::PolicyDenied(
                            "payment exceeds the per-payment policy limit".to_owned(),
                        ));
                    }
                }
                Ok(())
            }

            pub(crate) async fn validate_current_payment_scope(
                connection: &mut Connection,
                intent: &PaymentIntent,
                origin: Option<&str>,
            ) -> Result<()> {
                intent
                    .validate()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
                let (policy_id, wallet_id) = intent
                    .authority_ids()
                    .map_err(|field| Error::InvalidRequest(format!("invalid payment intent {field}")))?;
                Self::lock_scenario_budget(connection, &intent.scenario_id).await?;
                let scenario_row = sqlx::query(concat!(
                    "SELECT s.*, c.enabled AS client_enabled FROM pay_scenario s \
     JOIN pay_client c ON c.id = s.client_id WHERE s.id = $1",
                    $client_lock
                ))
                .bind(&intent.scenario_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| BaseRepository::storage_error("reload payment scenario lifecycle", error))?
                .ok_or_else(|| crate::Error::conflict("payment scenario no longer exists"))?;
                let scenario = ScenarioRepository::decode_row(&scenario_row)?;
                let client_enabled: bool = scenario_row
                    .try_get("client_enabled")
                    .map_err(|error| BaseRepository::storage_error("decode payment client lifecycle", error))?;
                Self::lock_policy_wallet(connection, policy_id, wallet_id).await?;
                let policy = PolicyRepository::load_policy_connection(connection, policy_id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("payment policy no longer exists"))?;
                let wallet = WalletRepository::load_payment_wallet_connection(connection, wallet_id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("payment wallet no longer exists"))?;
                if !scenario.enabled
                    || !client_enabled
                    || !policy.enabled
                    || policy.scenario_id != intent.scenario_id
                    || !policy.allows_wallet(wallet_id)
                    || !policy.allows_direction(intent.direction)
                    || !policy.allows_protocol(&intent.protocol)
                    || !policy.allows_asset(&intent.asset)
                    || wallet.budget_unit.as_ref() != Some(&policy.budget_unit)
                    || !wallet.enabled
                    || (intent.direction == PaymentDirection::Outbound
                        && !wallet.signing_ready_for_payment_network(&intent.asset.network))
                    || (intent.direction == PaymentDirection::Outbound
                        && !policy.allows_payment_recipient(&intent.asset.network, &intent.recipient))
                    || origin.is_some_and(|origin| !policy.allows_origin(origin))
                {
                    return Err(Error::PolicyDenied(
                        "payment scope is no longer authorized".to_owned(),
                    ));
                }
                Ok(())
            }

            async fn validate_challenge_guard(
                connection: &mut Connection,
                intent: &PaymentIntent,
                challenge: &PaymentChallenge,
                guard: &ChallengeIssuanceGuard,
            ) -> Result<()> {
                if intent.direction != PaymentDirection::Inbound
                    || intent.status != PaymentStatus::Created
                    || intent.reservation_status != ReservationStatus::Reserved
                    || intent.reserved_amount != intent.amount
                    || intent.offer_id.as_deref() != Some(guard.offer.id.as_str())
                    || intent.policy_id.as_deref() != Some(guard.policy.id.as_str())
                    || intent.wallet_id.as_deref() != Some(guard.wallet.wallet.id.as_str())
                    || intent.challenge_hash.as_deref() != Some(challenge.challenge_hash.as_str())
                    || challenge.scenario_id != intent.scenario_id
                    || challenge.offer_id != intent.offer_id
                    || challenge.resource != intent.resource
                    || challenge.protocol != intent.protocol
                    || challenge.asset != intent.asset
                    || challenge.amount != intent.amount
                    || challenge.recipient != intent.recipient
                    || challenge.expires_at != intent.expires_at.unwrap_or_default()
                    || challenge.expires_at <= guard.evaluated_at
                {
                    return Err(crate::Error::conflict(
                        "inbound challenge aggregate is not internally bound",
                    ));
                }
                Self::lock_authorization_scope(
                    connection,
                    &guard.client.id,
                    &guard.scenario.id,
                    &guard.policy.id,
                    Some(&guard.offer.id),
                    &guard.wallet.wallet.id,
                )
                .await?;
                let client = BaseRepository::select_by_connection::<crate::model::PaymentClient>(
                    connection,
                    crate::storage::EntityQuery::new()
                        .eq("id", &guard.client.id)
                        .eq("auth_principal", &guard.principal),
                )
                .await?
                .ok_or(Error::Forbidden)?;
                let scenario = BaseRepository::select_by_connection::<crate::model::PaymentScenario>(
                    connection,
                    crate::storage::EntityQuery::new()
                        .eq("id", &guard.scenario.id)
                        .eq("client_id", &guard.client.id),
                )
                .await?
                .ok_or(Error::Forbidden)?;
                let offer = BaseRepository::select_by_connection::<crate::model::PaymentPrice>(
                    connection,
                    crate::storage::EntityQuery::new().eq("id", &guard.offer.id),
                )
                .await?
                .ok_or_else(|| crate::Error::conflict("payment offer no longer exists"))?;
                let policy = PolicyRepository::load_policy_connection(connection, &guard.policy.id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("payment policy no longer exists"))?;
                let wallet = WalletRepository::load_payment_wallet_connection(connection, &guard.wallet.wallet.id)
                    .await?
                    .ok_or_else(|| crate::Error::conflict("payment wallet no longer exists"))?;
                let canonical_wallet = wallet
                    .clone()
                    .canonicalized_for_payment_network(&intent.asset.network)
                    .map_err(|_| Error::PolicyDenied("offer recipient wallet is invalid for its network".to_owned()))?;
                if client != guard.client
                    || scenario != guard.scenario
                    || offer != guard.offer
                    || policy != guard.policy
                    || wallet != guard.wallet
                    || !client.authenticates(&guard.principal)
                    || !scenario.enabled
                    || !offer.enabled
                    || !policy.enabled
                    || !wallet.enabled
                    || policy.scenario_id != scenario.id
                    || offer.scenario_id != scenario.id
                    || offer.policy_id != policy.id
                    || offer.wallet_id != wallet.wallet.id
                    || !policy.allows_wallet(&wallet.wallet.id)
                    || !policy.allows_direction(PaymentDirection::Inbound)
                    || !policy.allows_protocol(&intent.protocol)
                    || !policy.allows_asset(&intent.asset)
                    || !policy.allows_origin(&guard.origin)
                    || wallet.budget_unit.as_ref() != Some(&policy.budget_unit)
                    || canonical_wallet.wallet.address != intent.recipient
                {
                    return Err(Error::PolicyDenied(
                        "inbound challenge changed or falls outside current policy".to_owned(),
                    ));
                }
                Ok(())
            }
        }
    };
}
pub(crate) use payment_authority;

#[cfg(test)]
#[path = "../../../tests/storage/payment/authority_test.rs"]
mod tests;
