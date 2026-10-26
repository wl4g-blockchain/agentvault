use super::*;
use crate::payment::validation::PaymentValidation;

impl SettlementCoordinator {
    /// Issues and durably records a challenge from server-owned price fields.
    ///
    /// # Errors
    ///
    /// Returns an error for unauthorized scenarios, invalid bindings, missing
    /// prices/wallets, unsupported protocols, or persistence failures.
    pub async fn issue_challenge(
        &self,
        principal: &str,
        command: &IssuePaymentChallenge,
        now: i64,
    ) -> Result<PaymentChallenge> {
        SettlementFacts::validate_issue_command(command)?;
        let access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        if let Some(existing) = self.payments.find_run(&command.scenario_id, &command.run_id).await? {
            SettlementFacts::ensure_replay_identity(&existing.intent, command)?;
            if existing.intent.direction != PaymentDirection::Inbound {
                return Err(Error::PaymentConflict(
                    "run_id is already bound to an outbound payment".to_owned(),
                ));
            }
            return self.load_challenge(&existing.intent).await;
        }
        let PreparedInboundChallenge {
            access,
            price,
            policy,
            wallet,
            challenge,
        } = self.prepare_inbound_challenge(access, command, now).await?;
        let intent = SettlementFacts::build_inbound_intent(command, &price, &wallet.wallet, &challenge, now)?;
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::ChallengeCreated,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"challenge": challenge}),
            created_at: now,
        };
        let guard = ChallengeIssuanceGuard {
            // Keep the SQL guard authoritative even when the HTTP edge is
            // deliberately open: the resolved scenario owner, not the
            // internal unrestricted marker, is revalidated under lock.
            principal: access.principal.clone(),
            client: access.client,
            scenario: access.scenario,
            price: price.clone(),
            policy,
            wallet: wallet.clone(),
            origin: command.origin.clone(),
            day_started_at: PaymentValidation::utc_day_start(now),
            evaluated_at: now,
        };
        let record = self
            .payments
            .record_challenge(ChallengeWrite {
                intent: &intent,
                challenge: &challenge,
                guard: &guard,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await?;
        match record {
            IdempotentWrite::Inserted(stored) => {
                SettlementFacts::ensure_replay_identity(&stored, command)?;
                Ok(challenge)
            }
            IdempotentWrite::Replay(stored) => {
                SettlementFacts::ensure_replay_identity(&stored, command)?;
                self.load_challenge(&stored).await
            }
        }
    }

    pub(super) async fn prepare_inbound_challenge(
        &self,
        access: AccessContext,
        command: &IssuePaymentChallenge,
        now: i64,
    ) -> Result<PreparedInboundChallenge> {
        let price = self
            .prices
            .get(&command.price_id)
            .await?
            .filter(|price| {
                price.enabled
                    && price.scenario_id == command.scenario_id
                    && price.resource == command.resource
                    && price.supports_protocol(&command.protocol)
            })
            .ok_or_else(|| Error::NotFound("payment price".to_owned()))?;
        if PaymentValidation::canonical_http_origin(&price.resource)? != command.origin {
            return Err(Error::InvalidRequest(
                "protected resource origin does not match the price".to_owned(),
            ));
        }
        let policy = self
            .policies
            .get(&price.policy_id)
            .await?
            .filter(|policy| {
                policy.enabled
                    && policy.scenario_id == command.scenario_id
                    && policy.allows_direction(PaymentDirection::Inbound)
                    && policy.allows_wallet(&price.wallet_id)
            })
            .ok_or_else(|| Error::PolicyDenied("price has no enabled inbound policy-wallet binding".to_owned()))?;
        let wallet = self
            .wallets
            .get_payment_wallet(&price.wallet_id)
            .await?
            .filter(|wallet| wallet.enabled)
            .ok_or_else(|| Error::NotFound("price recipient wallet".to_owned()))?
            .canonicalized_for_payment_network(&price.asset.network)
            .map_err(|field| Error::Storage(format!("invalid price recipient wallet {field}")))?;
        let challenge = self
            .protocols
            .get(&command.protocol)?
            .build_challenge(&price, &wallet.wallet.address, now)
            .await?;
        SettlementFacts::validate_payee_challenge(&challenge, &price, &wallet.wallet, now)?;
        self.ensure_inbound_capacity(command, &price, &policy, &challenge, now)
            .await?;
        Ok(PreparedInboundChallenge {
            access,
            price,
            policy,
            wallet,
            challenge,
        })
    }

    pub(super) async fn ensure_inbound_capacity(
        &self,
        command: &IssuePaymentChallenge,
        price: &PaymentPrice,
        policy: &PaymentPolicy,
        challenge: &PaymentChallenge,
        now: i64,
    ) -> Result<()> {
        let budget = self
            .payments
            .budget_snapshot(
                &command.scenario_id,
                &price.wallet_id,
                PaymentDirection::Inbound,
                &price.asset.budget_unit(),
                PaymentValidation::utc_day_start(now),
                now,
            )
            .await?;
        let decision = PolicyEngine::evaluate(
            policy,
            PolicyEvaluation {
                direction: PaymentDirection::Inbound,
                challenge,
                wallet_id: &price.wallet_id,
                origin: &command.origin,
                scenario_usage: budget.scenario_spent_and_reserved,
                wallet_budget: WalletBudget {
                    usage: budget.wallet_spent_and_reserved,
                    shared_across_scenarios: budget.wallet_shared_across_scenarios,
                    max_per_payment: budget.wallet_max_per_payment,
                    max_per_day: budget.wallet_max_per_day,
                },
                now,
            },
        )?;
        match decision {
            PolicyDecision::Allowed => Ok(()),
            PolicyDecision::RequiresApproval => Err(Error::PolicyDenied(
                "inbound capacity requires administrator approval before challenge issuance".to_owned(),
            )),
            PolicyDecision::Denied(reason) => Err(Error::PolicyDenied(reason)),
        }
    }

    pub(super) async fn load_challenge(&self, intent: &PaymentIntent) -> Result<PaymentChallenge> {
        let expected_event = match intent.direction {
            PaymentDirection::Inbound => PaymentEventType::ChallengeCreated,
            PaymentDirection::Outbound => PaymentEventType::ChallengeReceived,
        };
        self.events
            .find_for_intent_type(&intent.id, expected_event, 0)
            .await?
            .and_then(|event| event.payload.get("challenge").cloned())
            .map(serde_json::from_value)
            .transpose()?
            .filter(|challenge: &PaymentChallenge| {
                intent.challenge_hash.as_deref() == Some(challenge.challenge_hash.as_str())
            })
            .ok_or_else(|| Error::Storage("durable challenge event is missing or inconsistent".to_owned()))
    }
}

#[cfg(test)]
#[path = "../../../tests/payment/settlement/challenge_test.rs"]
mod tests;
