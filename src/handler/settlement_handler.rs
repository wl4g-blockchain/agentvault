use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    model::{
        payment_recipients_match, settlement_posting_key, wallet::is_evm_payment_network, Amount, AssetIdentity,
        LedgerEntryType, PaymentChallenge, PaymentCredential, PaymentDirection, PaymentEvent, PaymentEventType,
        PaymentIntent, PaymentLedgerEntry, PaymentOffer, PaymentPolicy, PaymentReceipt, PaymentStatus,
        ReservationStatus,
    },
    protocol::x402::eip3009_authorization_identity,
    storage::{
        ChallengeIssuanceGuard, ChallengeWrite, EventStorage, IdempotentWrite, OfferStorage, OutboxMessage,
        OutboxStatus, PaymentProgressWrite, PaymentStorage, PaymentWallet, PolicyStorage, SettlementWrite,
        VerificationWrite, WalletStorage,
    },
    utils::secure_material::{SealedSensitiveMaterial, SensitiveMaterialProtector},
    Error, Result,
};

use super::common::{outbound_credential_context, validate_identifier, validate_trusted_effective_time};
use super::http_json_transport::{FixedHttpJsonTransport, ProviderHttpEndpoint};

use super::{
    common::{canonical_http_origin, utc_day_start},
    AccessContext, EvmAuthorizationObservation, EvmAuthorizationQuery, EvmAuthorizationSource, PolicyDecision,
    PolicyEngine, PolicyEvaluation, ProtocolRegistry, ScenarioHandler, WalletBudget,
};

const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// External provider operation. `AgentVault` never embeds a facilitator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementOperation {
    Verify,
    Settle,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementProviderRequest {
    pub protocol: String,
    pub run_id: String,
    pub payment_payload: Value,
    pub payment_requirements: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettlementProviderResult {
    #[serde(default, alias = "isValid")]
    pub valid: bool,
    #[serde(default, alias = "success")]
    pub settled: bool,
    #[serde(default)]
    pub payer: Option<String>,
    #[serde(default, alias = "transactionHash")]
    pub transaction: Option<String>,
    #[serde(default)]
    pub settlement_ref: Option<String>,
    /// Trusted locator for one independently confirmed effect within the
    /// transaction or provider settlement, such as `log:7` or `gas`.
    #[serde(default)]
    pub effect_ref: Option<String>,
    #[serde(default)]
    pub invalid_reason: Option<String>,
    /// Trusted provider interpretation of the confirmed asset identity.
    #[serde(default)]
    pub asset: Option<AssetIdentity>,
    /// Exact confirmed amount in base units.
    #[serde(default)]
    pub amount: Option<Amount>,
    /// Confirmed receiver; never taken from the incoming credential alone.
    #[serde(default)]
    pub recipient: Option<String>,
    /// Chain confirmations observed by the configured provider/RPC boundary.
    #[serde(default)]
    pub confirmations: Option<u64>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl SettlementProviderResult {
    pub(crate) fn validate_confirmed_for(&self, intent: &PaymentIntent, minimum_confirmations: u64) -> Result<()> {
        let asset_matches = self
            .asset
            .as_ref()
            .is_some_and(|asset| asset_identity_matches(asset, &intent.asset));
        let recipient_matches = self
            .recipient
            .as_deref()
            .is_some_and(|recipient| payment_recipients_match(&intent.asset.network, recipient, &intent.recipient));
        let confirmations_match = if minimum_confirmations == 0 {
            true
        } else {
            self.confirmations.is_some_and(|value| value >= minimum_confirmations)
        };
        if !self.valid
            || !self.settled
            || !asset_matches
            || self.amount != Some(intent.amount)
            || !recipient_matches
            || !confirmations_match
            || (self.transaction.as_deref().is_none_or(str::is_empty)
                && self.settlement_ref.as_deref().is_none_or(str::is_empty))
        {
            return Err(Error::Settlement(
                "provider settlement evidence does not match the payment intent".to_owned(),
            ));
        }
        Ok(())
    }
}

fn asset_identity_matches(observed: &AssetIdentity, expected: &AssetIdentity) -> bool {
    observed.network == expected.network
        && observed.asset.eq_ignore_ascii_case(&expected.asset)
        && observed.decimals == expected.decimals
        && match (&observed.token_contract, &expected.token_contract) {
            (Some(observed), Some(expected)) => observed.eq_ignore_ascii_case(expected),
            (None, None) => true,
            _ => false,
        }
}

#[async_trait]
pub trait ExternalSettlementProvider: Send + Sync {
    async fn execute(
        &self,
        operation: SettlementOperation,
        request: &SettlementProviderRequest,
    ) -> Result<SettlementProviderResult>;
}

/// Fixed protocol-to-provider routing. Challenge content cannot select or
/// override a provider endpoint.
pub struct SettlementProviderRegistry {
    providers: HashMap<String, SettlementProviderBinding>,
}

#[derive(Clone)]
struct SettlementProviderBinding {
    authority: String,
    confirmation_depths: BTreeMap<String, u64>,
    provider: Arc<dyn ExternalSettlementProvider>,
}

impl SettlementProviderBinding {
    fn confirmation_depth(&self, network: &str) -> Result<u64> {
        self.confirmation_depths.get(network).copied().ok_or_else(|| {
            Error::Settlement(format!(
                "external provider {} has no trusted confirmation depth for {network}",
                self.authority
            ))
        })
    }
}

/// Trusted provider observation plus the operator-owned evidence authority.
/// This crate-private value is consumed by the reconciliation scheduler and
/// is never exposed as a transport DTO.
pub(crate) enum ReconciliationProviderObservation {
    Observed {
        authority: String,
        minimum_confirmations: u64,
        result: Box<SettlementProviderResult>,
        effective_at: Option<i64>,
    },
    Expired {
        reason: String,
    },
}

impl SettlementProviderRegistry {
    /// Builds an immutable provider registry.
    ///
    /// # Errors
    ///
    /// Returns an error for a blank protocol or duplicate protocol
    /// registration. An empty registry represents a disabled payee direction.
    pub fn new(
        providers: impl IntoIterator<
            Item = (
                String,
                String,
                BTreeMap<String, u64>,
                Arc<dyn ExternalSettlementProvider>,
            ),
        >,
    ) -> Result<Self> {
        let mut registered = HashMap::new();
        for (protocol, authority, confirmation_depths, provider) in providers {
            if protocol.trim().is_empty()
                || authority.trim().is_empty()
                || confirmation_depths.is_empty()
                || confirmation_depths
                    .iter()
                    .any(|(network, depth)| network.trim().is_empty() || *depth == 0)
                || registered
                    .insert(
                        protocol.clone(),
                        SettlementProviderBinding {
                            authority,
                            confirmation_depths,
                            provider,
                        },
                    )
                    .is_some()
            {
                return Err(Error::Config(format!(
                    "invalid or duplicate settlement provider protocol: {protocol}"
                )));
            }
        }
        Ok(Self { providers: registered })
    }

    fn get(&self, protocol: &str) -> Result<SettlementProviderBinding> {
        self.providers
            .get(protocol)
            .cloned()
            .ok_or_else(|| Error::Settlement(format!("no external provider configured for {protocol}")))
    }
}

#[derive(Clone, Debug)]
pub struct IssuePaymentChallenge {
    pub scenario_id: String,
    pub run_id: String,
    pub offer_id: String,
    pub resource: String,
    pub origin: String,
    pub protocol: String,
    /// Stable digest of the protected HTTP/MCP business request. The trusted
    /// business SDK computes this once and reuses it for every retry.
    pub request_hash: String,
}

#[derive(Clone, Debug)]
pub struct VerifyIncomingPayment {
    pub scenario_id: String,
    pub run_id: String,
    pub challenge_hash: String,
    pub request_hash: String,
    pub credential: PaymentCredential,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingVerification {
    pub intent: PaymentIntent,
    pub valid: bool,
    pub verification_ref: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SettleIncomingPayment {
    pub scenario_id: String,
    pub run_id: String,
    pub intent_id: String,
    pub verification_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettledIncomingPayment {
    pub intent: PaymentIntent,
    pub receipt: PaymentReceipt,
}

/// Payee-side challenge, verification, settlement, and ledger orchestration.
/// All business identity is derived from the authenticated principal.
pub struct SettlementHandler {
    scenarios: Arc<ScenarioHandler>,
    offers: Arc<dyn OfferStorage>,
    policies: Arc<dyn PolicyStorage>,
    wallets: Arc<dyn WalletStorage>,
    payments: Arc<dyn PaymentStorage>,
    events: Arc<dyn EventStorage>,
    protocols: Arc<ProtocolRegistry>,
    providers: Arc<SettlementProviderRegistry>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
    authorizations: Arc<dyn EvmAuthorizationSource>,
    reconciliation_delay_seconds: i64,
}

/// Collaborators used by the payee settlement application service.
pub struct SettlementDependencies {
    pub scenarios: Arc<ScenarioHandler>,
    pub offers: Arc<dyn OfferStorage>,
    pub policies: Arc<dyn PolicyStorage>,
    pub wallets: Arc<dyn WalletStorage>,
    pub payments: Arc<dyn PaymentStorage>,
    pub events: Arc<dyn EventStorage>,
    pub protocols: Arc<ProtocolRegistry>,
    pub providers: Arc<SettlementProviderRegistry>,
    pub material_protector: Arc<dyn SensitiveMaterialProtector>,
    pub authorizations: Arc<dyn EvmAuthorizationSource>,
}

struct PreparedInboundChallenge {
    access: AccessContext,
    offer: PaymentOffer,
    policy: PaymentPolicy,
    wallet: PaymentWallet,
    challenge: PaymentChallenge,
}

enum SettlementRequestRecord {
    Inserted(PaymentIntent),
    Replay(PaymentIntent),
}

impl SettlementHandler {
    /// Creates a payee settlement orchestrator.
    ///
    /// # Errors
    ///
    /// Returns an error when the reconciliation delay is not positive.
    pub fn new(dependencies: SettlementDependencies, reconciliation_delay_seconds: i64) -> Result<Self> {
        if reconciliation_delay_seconds <= 0 {
            return Err(Error::Config("reconciliation delay must be positive".to_owned()));
        }
        Ok(Self {
            scenarios: dependencies.scenarios,
            offers: dependencies.offers,
            policies: dependencies.policies,
            wallets: dependencies.wallets,
            payments: dependencies.payments,
            events: dependencies.events,
            protocols: dependencies.protocols,
            providers: dependencies.providers,
            material_protector: dependencies.material_protector,
            authorizations: dependencies.authorizations,
            reconciliation_delay_seconds,
        })
    }

    /// Reconstructs settlement material exclusively from encrypted durable
    /// events. An x402 settlement that was durably requested is recovered from
    /// the canonical EIP-3009 `AuthorizationUsed` event plus the exact ERC-20
    /// transfer receipt; other states use the provider's read-only verify API.
    pub(crate) async fn reconciliation_observation(
        &self,
        intent: &PaymentIntent,
        observed_at: i64,
    ) -> Result<ReconciliationProviderObservation> {
        let settlement = self.load_reconciliation_settlement(intent).await?;
        if uses_eip3009_recovery(intent) {
            let Some(requested) = self.settlement_requested_event(intent).await? else {
                return Err(Error::Storage(
                    "inbound x402 reconciliation has no durable settlement operation".to_owned(),
                ));
            };
            return self
                .authorization_reconciliation_observation(intent, &settlement, &requested, observed_at)
                .await;
        }
        let request = SettlementProviderRequest {
            protocol: settlement.protocol,
            run_id: intent.run_id.clone(),
            payment_payload: settlement.payment_payload,
            payment_requirements: settlement.payment_requirements,
        };
        let binding = self.providers.get(&intent.protocol)?;
        let result = binding.provider.execute(SettlementOperation::Verify, &request).await?;
        let minimum_confirmations = binding.confirmation_depth(&intent.asset.network)?;
        Ok(ReconciliationProviderObservation::Observed {
            authority: binding.authority,
            minimum_confirmations,
            result: Box::new(result),
            effective_at: None,
        })
    }

    async fn authorization_reconciliation_observation(
        &self,
        intent: &PaymentIntent,
        settlement: &crate::protocol::ProtocolSettlement,
        requested: &PaymentEvent,
        observed_at: i64,
    ) -> Result<ReconciliationProviderObservation> {
        let from_block = settlement_request_from_block(intent, requested)?
            .ok_or_else(|| Error::Storage("x402 settlement checkpoint is missing".to_owned()))?;
        let (query, valid_before) = authorization_query(intent, settlement, from_block)?;
        let expiry = authorization_expiry_deadline(intent, valid_before);
        match self.authorizations.lookup_authorization(&query).await? {
            EvmAuthorizationObservation::Confirmed(evidence) => Ok(ReconciliationProviderObservation::Observed {
                authority: evidence.authority,
                minimum_confirmations: evidence.minimum_confirmations,
                result: Box::new(SettlementProviderResult {
                    valid: true,
                    settled: true,
                    payer: Some(query.payer),
                    transaction: Some(evidence.transaction),
                    settlement_ref: None,
                    effect_ref: Some(evidence.effect_ref),
                    invalid_reason: None,
                    asset: Some(intent.asset.clone()),
                    amount: Some(intent.amount),
                    recipient: Some(intent.recipient.clone()),
                    confirmations: Some(evidence.confirmations),
                    extensions: BTreeMap::new(),
                }),
                effective_at: Some(evidence.effective_at),
            }),
            EvmAuthorizationObservation::NoEffect {
                observed_at: chain_observed_at,
                ..
            } if authorization_is_definitively_expired(observed_at, chain_observed_at, expiry) => {
                Ok(ReconciliationProviderObservation::Expired {
                    reason: "EIP-3009 authorization expired without an on-chain AuthorizationUsed effect".to_owned(),
                })
            }
            EvmAuthorizationObservation::Pending | EvmAuthorizationObservation::NoEffect { .. } => {
                Ok(ReconciliationProviderObservation::Observed {
                    authority: format!("evm-rpc:{}", intent.asset.network),
                    minimum_confirmations: 0,
                    result: Box::new(SettlementProviderResult {
                        valid: true,
                        settled: false,
                        payer: Some(query.payer),
                        transaction: None,
                        settlement_ref: None,
                        effect_ref: None,
                        invalid_reason: None,
                        asset: Some(intent.asset.clone()),
                        amount: Some(intent.amount),
                        recipient: Some(intent.recipient.clone()),
                        confirmations: None,
                        extensions: BTreeMap::new(),
                    }),
                    effective_at: None,
                })
            }
            EvmAuthorizationObservation::Mismatch { reason } => Ok(ReconciliationProviderObservation::Observed {
                authority: format!("evm-rpc:{}", intent.asset.network),
                minimum_confirmations: 0,
                result: Box::new(SettlementProviderResult {
                    valid: false,
                    settled: false,
                    payer: Some(query.payer),
                    transaction: None,
                    settlement_ref: None,
                    effect_ref: None,
                    invalid_reason: Some(reason),
                    asset: None,
                    amount: None,
                    recipient: None,
                    confirmations: None,
                    extensions: BTreeMap::new(),
                }),
                effective_at: None,
            }),
        }
    }

    /// Issues and durably records a challenge from server-owned offer fields.
    ///
    /// # Errors
    ///
    /// Returns an error for unauthorized scenarios, invalid bindings, missing
    /// offers/wallets, unsupported protocols, or persistence failures.
    pub async fn issue_challenge(
        &self,
        principal: &str,
        command: &IssuePaymentChallenge,
        now: i64,
    ) -> Result<PaymentChallenge> {
        validate_issue_command(command)?;
        let access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        if let Some(existing) = self.payments.find_run(&command.scenario_id, &command.run_id).await? {
            ensure_replay_identity(&existing.intent, command)?;
            if existing.intent.direction != PaymentDirection::Inbound {
                return Err(Error::PaymentConflict(
                    "run_id is already bound to an outbound payment".to_owned(),
                ));
            }
            return self.load_challenge(&existing.intent).await;
        }
        let PreparedInboundChallenge {
            access,
            offer,
            policy,
            wallet,
            challenge,
        } = self.prepare_inbound_challenge(access, command, now).await?;
        let intent = build_inbound_intent(command, &offer, &wallet.wallet, &challenge, now)?;
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
            offer: offer.clone(),
            policy,
            wallet: wallet.clone(),
            origin: command.origin.clone(),
            day_started_at: utc_day_start(now),
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
                ensure_replay_identity(&stored, command)?;
                Ok(challenge)
            }
            IdempotentWrite::Replay(stored) => {
                ensure_replay_identity(&stored, command)?;
                self.load_challenge(&stored).await
            }
        }
    }

    async fn prepare_inbound_challenge(
        &self,
        access: AccessContext,
        command: &IssuePaymentChallenge,
        now: i64,
    ) -> Result<PreparedInboundChallenge> {
        let offer = self
            .offers
            .get(&command.offer_id)
            .await?
            .filter(|offer| {
                offer.enabled
                    && offer.scenario_id == command.scenario_id
                    && offer.resource == command.resource
                    && offer.supports_protocol(&command.protocol)
            })
            .ok_or_else(|| Error::NotFound("payment offer".to_owned()))?;
        if canonical_http_origin(&offer.resource)? != command.origin {
            return Err(Error::InvalidRequest(
                "protected resource origin does not match the offer".to_owned(),
            ));
        }
        let policy = self
            .policies
            .get(&offer.policy_id)
            .await?
            .filter(|policy| {
                policy.enabled
                    && policy.scenario_id == command.scenario_id
                    && policy.allows_direction(PaymentDirection::Inbound)
                    && policy.allows_wallet(&offer.wallet_id)
            })
            .ok_or_else(|| Error::PolicyDenied("offer has no enabled inbound policy-wallet binding".to_owned()))?;
        let wallet = self
            .wallets
            .get_payment_wallet(&offer.wallet_id)
            .await?
            .filter(|wallet| wallet.enabled)
            .ok_or_else(|| Error::NotFound("offer recipient wallet".to_owned()))?
            .canonicalized_for_payment_network(&offer.asset.network)
            .map_err(|field| Error::Storage(format!("invalid offer recipient wallet {field}")))?;
        let challenge = self
            .protocols
            .get(&command.protocol)?
            .build_challenge(&offer, &wallet.wallet.address, now)
            .await?;
        validate_payee_challenge(&challenge, &offer, &wallet.wallet, now)?;
        self.ensure_inbound_capacity(command, &offer, &policy, &challenge, now)
            .await?;
        Ok(PreparedInboundChallenge {
            access,
            offer,
            policy,
            wallet,
            challenge,
        })
    }

    async fn ensure_inbound_capacity(
        &self,
        command: &IssuePaymentChallenge,
        offer: &PaymentOffer,
        policy: &PaymentPolicy,
        challenge: &PaymentChallenge,
        now: i64,
    ) -> Result<()> {
        let budget = self
            .payments
            .budget_snapshot(
                &command.scenario_id,
                &offer.wallet_id,
                PaymentDirection::Inbound,
                &offer.asset.budget_unit(),
                utc_day_start(now),
                now,
            )
            .await?;
        let decision = PolicyEngine::evaluate(
            policy,
            PolicyEvaluation {
                direction: PaymentDirection::Inbound,
                challenge,
                wallet_id: &offer.wallet_id,
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

    /// Structurally verifies and durably binds a credential before invoking
    /// the fixed external provider's verify endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for unauthorized access, replay conflicts, invalid
    /// credentials, provider failures, or atomic persistence failures.
    pub async fn verify_payment(
        &self,
        principal: &str,
        command: &VerifyIncomingPayment,
        now: i64,
    ) -> Result<IncomingVerification> {
        validate_verify_command(command)?;
        let _access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        let intent = self
            .payments
            .get_for_challenge(&command.scenario_id, &command.challenge_hash)
            .await?
            .ok_or_else(|| Error::NotFound("payment challenge".to_owned()))?;
        ensure_incoming_identity(&intent, &command.run_id, &command.request_hash)?;
        if intent.status == PaymentStatus::Settled || intent.status == PaymentStatus::PendingSettlement {
            self.ensure_credential_replay(&intent, &command.credential).await?;
            return self.load_verification(&intent).await;
        }
        if intent.status == PaymentStatus::Failed || intent.status == PaymentStatus::Expired {
            return Ok(IncomingVerification {
                intent,
                valid: false,
                verification_ref: None,
            });
        }
        let (submitted, request) = match intent.status {
            PaymentStatus::Created => self.submit_credential(&intent, &command.credential, now).await?,
            PaymentStatus::Submitted => self.resume_submitted(&intent, &command.credential, now).await?,
            _ => {
                return Err(Error::InvalidTransition(format!(
                    "cannot verify inbound payment in {}",
                    intent.status.as_str()
                )))
            }
        };
        let result = self
            .providers
            .get(&submitted.protocol)?
            .provider
            .execute(SettlementOperation::Verify, &request)
            .await?;
        if !result.valid {
            let provider_evidence_hash = evidence_hash(&serde_json::to_value(&result)?)?;
            let failed = self
                .record_non_settlement(
                    &submitted,
                    PaymentStatus::Failed,
                    PaymentEventType::Failed,
                    json!({"stage": "verify", "provider_evidence_hash": provider_evidence_hash}),
                    now,
                )
                .await?;
            return Ok(IncomingVerification {
                intent: failed,
                valid: false,
                verification_ref: None,
            });
        }
        let verification_ref = provider_verification_ref(&submitted.id, &result)?;
        let pending = self
            .record_non_settlement(
                &submitted,
                PaymentStatus::PendingSettlement,
                PaymentEventType::CredentialVerified,
                json!({"verification_ref": verification_ref}),
                now,
            )
            .await?;
        Ok(IncomingVerification {
            intent: pending,
            valid: true,
            verification_ref: Some(verification_ref),
        })
    }

    async fn submit_credential(
        &self,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
        now: i64,
    ) -> Result<(PaymentIntent, SettlementProviderRequest)> {
        let challenge = self.load_challenge(intent).await?;
        let adapter = self.protocols.get(&intent.protocol)?;
        let mut credential = credential.clone();
        adapter.verify_credential(&challenge, &credential, now).await?;
        credential.credential_hash = adapter.credential_fingerprint(&challenge, &credential)?;
        let settlement = adapter.settle_payment(&challenge, &credential).await?;
        let settlement_bytes = Zeroizing::new(serde_json::to_vec(&settlement)?);
        let sealed_settlement = self
            .material_protector
            .seal(&settlement_material_context(intent), &settlement_bytes)?;
        let mut submitted = intent.clone();
        submitted.status = PaymentStatus::Submitted;
        submitted.next_reconcile_at = Some(now.saturating_add(self.reconciliation_delay_seconds));
        submitted.updated_at = next_timestamp(intent.updated_at, now);
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Submitted,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({
                "credential_fingerprint": credential.credential_hash.clone(),
                "sealed_settlement": sealed_settlement,
            }),
            created_at: submitted.updated_at,
        };
        let submitted = self
            .payments
            .verify_incoming(VerificationWrite {
                intent: &submitted,
                credential: &credential,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await?;
        let request = SettlementProviderRequest {
            protocol: settlement.protocol,
            run_id: submitted.run_id.clone(),
            payment_payload: settlement.payment_payload,
            payment_requirements: settlement.payment_requirements,
        };
        Ok((submitted, request))
    }

    async fn resume_submitted(
        &self,
        intent: &PaymentIntent,
        credential: &PaymentCredential,
        now: i64,
    ) -> Result<(PaymentIntent, SettlementProviderRequest)> {
        let challenge = self.load_challenge(intent).await?;
        let adapter = self.protocols.get(&intent.protocol)?;
        adapter.verify_credential(&challenge, credential, now).await?;
        let fingerprint = adapter.credential_fingerprint(&challenge, credential)?;
        let (stored_fingerprint, settlement) = self.load_submission(intent).await?;
        if fingerprint != stored_fingerprint {
            return Err(Error::PaymentConflict(
                "credential retry does not match the submitted payment proof".to_owned(),
            ));
        }
        Ok((
            intent.clone(),
            SettlementProviderRequest {
                protocol: settlement.protocol,
                run_id: intent.run_id.clone(),
                payment_payload: settlement.payment_payload,
                payment_requirements: settlement.payment_requirements,
            },
        ))
    }

    /// Requests external settlement and commits the confirmed financial fact,
    /// intent transition, event, and notification atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for unauthorized access, invalid state/bindings,
    /// provider failures, missing evidence, or persistence failures.
    pub async fn settle_payment(
        &self,
        principal: &str,
        command: &SettleIncomingPayment,
        now: i64,
    ) -> Result<SettledIncomingPayment> {
        validate_settle_command(command)?;
        let _access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        let intent = self
            .payments
            .get_for_scenario(&command.scenario_id, &command.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        ensure_settlement_intent_binding(&intent, command)?;
        if intent.status == PaymentStatus::Settled {
            let receipt = self.load_receipt(&intent).await?;
            return Ok(settled_response(intent, receipt));
        }
        if intent.status != PaymentStatus::PendingSettlement {
            return Err(Error::InvalidTransition(format!(
                "cannot settle inbound payment in {}",
                intent.status.as_str()
            )));
        }
        let (stored_ref, settlement) = self.load_settlement_material(&intent).await?;
        if stored_ref != command.verification_ref {
            return Err(Error::PaymentConflict(
                "verification reference does not match the verified credential".to_owned(),
            ));
        }
        let intent = match self.record_settlement_requested(&intent, now).await? {
            SettlementRequestRecord::Inserted(intent) => intent,
            SettlementRequestRecord::Replay(intent) => {
                if intent.status == PaymentStatus::Settled {
                    let receipt = self.load_receipt(&intent).await?;
                    return Ok(settled_response(intent, receipt));
                }
                let recovered = self.recover_requested_settlement(&intent, now).await?;
                return Ok(recovered);
            }
        };
        let request = SettlementProviderRequest {
            protocol: settlement.protocol,
            run_id: intent.run_id.clone(),
            payment_payload: settlement.payment_payload,
            payment_requirements: settlement.payment_requirements,
        };
        let provider = self.providers.get(&intent.protocol)?;
        let minimum_confirmations = provider.confirmation_depth(&intent.asset.network)?;
        let result = provider.provider.execute(SettlementOperation::Settle, &request).await?;
        if !result.valid || !result.settled || (result.transaction.is_none() && result.settlement_ref.is_none()) {
            let status = if !result.valid && result.invalid_reason.is_some() {
                PaymentStatus::Failed
            } else {
                PaymentStatus::Unknown
            };
            let provider_evidence_hash = evidence_hash(&serde_json::to_value(&result)?)?;
            let updated = self
                .record_non_settlement(
                    &intent,
                    status,
                    if status == PaymentStatus::Failed {
                        PaymentEventType::Failed
                    } else {
                        PaymentEventType::Retried
                    },
                    json!({"stage": "settle", "provider_evidence_hash": provider_evidence_hash}),
                    now,
                )
                .await?;
            return Err(Error::Settlement(format!(
                "settlement is not confirmed; intent {} is {}",
                updated.id,
                updated.status.as_str()
            )));
        }
        if let Err(error) = result.validate_confirmed_for(&intent, minimum_confirmations) {
            let provider_evidence_hash = evidence_hash(&serde_json::to_value(&result)?)?;
            self.record_non_settlement(
                &intent,
                PaymentStatus::Unknown,
                PaymentEventType::Retried,
                json!({
                    "stage": "settle_evidence_validation",
                    "provider_evidence_hash": provider_evidence_hash,
                }),
                now,
            )
            .await?;
            return Err(error);
        }
        let settled = self
            .commit_confirmed(&intent, &provider.authority, &result, now, None)
            .await?;
        Ok(settled)
    }

    async fn recover_requested_settlement(&self, intent: &PaymentIntent, now: i64) -> Result<SettledIncomingPayment> {
        match self.reconciliation_observation(intent, now).await? {
            ReconciliationProviderObservation::Expired { reason } => {
                let expired = self
                    .record_non_settlement(
                        intent,
                        PaymentStatus::Expired,
                        PaymentEventType::Failed,
                        json!({"stage": "settlement_recovery", "reason": reason}),
                        now,
                    )
                    .await?;
                Err(Error::Settlement(format!(
                    "settlement authorization expired; intent {} is {}",
                    expired.id,
                    expired.status.as_str()
                )))
            }
            ReconciliationProviderObservation::Observed {
                authority,
                minimum_confirmations,
                result,
                effective_at,
            } => {
                if result.valid && result.settled {
                    result.validate_confirmed_for(intent, minimum_confirmations)?;
                    return self
                        .commit_confirmed(intent, &authority, &result, now, effective_at)
                        .await;
                }
                if !result.valid {
                    let evidence = evidence_hash(&serde_json::to_value(&result)?)?;
                    self.record_non_settlement(
                        intent,
                        PaymentStatus::Unknown,
                        PaymentEventType::Retried,
                        json!({"stage": "settlement_recovery", "provider_evidence_hash": evidence}),
                        now,
                    )
                    .await?;
                }
                Err(Error::Settlement(
                    "durably requested settlement is awaiting trusted chain or provider evidence".to_owned(),
                ))
            }
        }
    }

    async fn record_settlement_requested(&self, intent: &PaymentIntent, now: i64) -> Result<SettlementRequestRecord> {
        if let Some(event) = self.settlement_requested_event(intent).await? {
            settlement_request_from_block(intent, &event)?;
            let current = self
                .payments
                .get(&intent.id)
                .await?
                .ok_or_else(|| Error::Storage("settlement intent disappeared".to_owned()))?;
            return Ok(SettlementRequestRecord::Replay(current));
        }
        let from_block = if uses_eip3009_recovery(intent) {
            Some(
                self.authorizations
                    .latest_block(&intent.asset.network)
                    .await?
                    .saturating_sub(1),
            )
        } else {
            None
        };
        let mut requested = intent.clone();
        requested.updated_at = next_timestamp(intent.updated_at, now);
        requested.next_reconcile_at = Some(now.saturating_add(self.reconciliation_delay_seconds));
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::SettlementRequested,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: settlement_request_payload(intent, from_block),
            created_at: requested.updated_at,
        };
        match self
            .payments
            .commit_progress(PaymentProgressWrite {
                intent: &requested,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await
        {
            Ok(stored) => Ok(SettlementRequestRecord::Inserted(stored)),
            Err(error) => {
                let Some(winner) = self.settlement_requested_event(intent).await? else {
                    return Err(error);
                };
                settlement_request_from_block(intent, &winner)?;
                let current = self
                    .payments
                    .get(&intent.id)
                    .await?
                    .ok_or_else(|| Error::Storage("settlement intent disappeared".to_owned()))?;
                Ok(SettlementRequestRecord::Replay(current))
            }
        }
    }

    async fn settlement_requested_event(&self, intent: &PaymentIntent) -> Result<Option<PaymentEvent>> {
        self.events
            .find_for_intent_type(&intent.id, PaymentEventType::SettlementRequested, 0)
            .await
    }

    async fn record_non_settlement(
        &self,
        intent: &PaymentIntent,
        status: PaymentStatus,
        event_type: PaymentEventType,
        payload: Value,
        now: i64,
    ) -> Result<PaymentIntent> {
        let mut updated = intent.clone();
        updated.status = status;
        updated.reservation_status = reservation_after_non_settlement(status, updated.reservation_status);
        updated.updated_at = next_timestamp(intent.updated_at, now);
        updated.next_reconcile_at = if status == PaymentStatus::Unknown || status == PaymentStatus::PendingSettlement {
            Some(now.saturating_add(self.reconciliation_delay_seconds))
        } else {
            None
        };
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload,
            created_at: updated.updated_at,
        };
        self.payments
            .commit_progress(PaymentProgressWrite {
                intent: &updated,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await
    }

    async fn commit_confirmed(
        &self,
        intent: &PaymentIntent,
        authority: &str,
        result: &SettlementProviderResult,
        now: i64,
        effective_at: Option<i64>,
    ) -> Result<SettledIncomingPayment> {
        let confirmed_at = next_timestamp(intent.updated_at, now);
        let effective_at = effective_at.unwrap_or(confirmed_at);
        validate_trusted_effective_time(intent.created_at, effective_at, now)?;
        let settled = confirmed_intent(intent, result, confirmed_at, effective_at);
        let receipt = confirmed_receipt(intent, result, effective_at)?;
        let ledger = confirmed_ledger(intent, authority, result, effective_at, confirmed_at)?;
        let event = confirmed_event(intent, result, &receipt, confirmed_at);
        let outbox = confirmed_outbox(intent, confirmed_at);
        let stored = match self
            .payments
            .commit_settlement(SettlementWrite {
                intent: &settled,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                ledger_entries: std::slice::from_ref(&ledger),
                outbox: std::slice::from_ref(&outbox),
            })
            .await
        {
            Ok(stored) => stored,
            Err(Error::PaymentConflict(reason)) => {
                let current = self
                    .payments
                    .get(&intent.id)
                    .await?
                    .ok_or_else(|| Error::Storage("concurrently settled payment intent disappeared".to_owned()))?;
                if !is_concurrent_settlement_winner(intent, &current) {
                    return Err(Error::PaymentConflict(reason));
                }
                let receipt = self.load_receipt(&current).await?;
                tracing::info!(
                    event_name = "agentvault.payee.settlement_race_recovered",
                    intent_id = %current.id,
                    run_id = %current.run_id,
                    "returned the durable receipt committed by a concurrent reconciliation worker"
                );
                return Ok(settled_response(current, receipt));
            }
            Err(error) => return Err(error),
        };
        Ok(settled_response(stored, receipt))
    }

    async fn load_challenge(&self, intent: &PaymentIntent) -> Result<PaymentChallenge> {
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

    async fn load_reconciliation_settlement(
        &self,
        intent: &PaymentIntent,
    ) -> Result<crate::protocol::ProtocolSettlement> {
        if self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Submitted, 0)
            .await?
            .is_some()
        {
            return self.load_submission(intent).await.map(|(_, settlement)| settlement);
        }
        if intent.direction != PaymentDirection::Outbound {
            return Err(Error::Storage(
                "inbound reconciliation has no durable provider submission".to_owned(),
            ));
        }

        let challenge = self.load_challenge(intent).await?;
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Signed, 0)
            .await?
            .ok_or_else(|| Error::Storage("outbound reconciliation has no durable signed credential".to_owned()))?;
        let sealed = event
            .payload
            .get("sealed_credential")
            .cloned()
            .map(serde_json::from_value::<SealedSensitiveMaterial>)
            .transpose()?
            .ok_or_else(|| Error::Storage("signed event has no sealed credential".to_owned()))?;
        let plaintext = self
            .material_protector
            .open(&outbound_credential_context(&intent.id), &sealed)?;
        let credential: PaymentCredential = serde_json::from_slice(&plaintext)?;
        let adapter = self.protocols.get(&intent.protocol)?;
        let fingerprint = adapter.credential_fingerprint(&challenge, &credential)?;
        if event.signature_ref.as_deref() != Some(fingerprint.as_str()) || credential.credential_hash != fingerprint {
            return Err(Error::PaymentConflict(
                "durable outbound credential fingerprint is inconsistent".to_owned(),
            ));
        }
        adapter.settle_payment(&challenge, &credential).await
    }

    async fn load_verification(&self, intent: &PaymentIntent) -> Result<IncomingVerification> {
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::CredentialVerified, 0)
            .await?
            .ok_or_else(|| Error::Storage("credential verification event is missing".to_owned()))?;
        let verification_ref = event
            .payload
            .get("verification_ref")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Storage("verification reference is missing".to_owned()))?;
        Ok(IncomingVerification {
            intent: intent.clone(),
            valid: true,
            verification_ref: Some(verification_ref),
        })
    }

    async fn ensure_credential_replay(&self, intent: &PaymentIntent, credential: &PaymentCredential) -> Result<()> {
        let challenge = self.load_challenge(intent).await?;
        if credential.protocol != intent.protocol || credential.challenge_hash != challenge.challenge_hash {
            return Err(Error::PaymentConflict(
                "credential retry is bound to another protocol or challenge".to_owned(),
            ));
        }
        let adapter = self.protocols.get(&intent.protocol)?;
        let fingerprint = adapter.credential_fingerprint(&challenge, credential)?;
        let (stored_fingerprint, _) = self.load_submission(intent).await?;
        if fingerprint != stored_fingerprint {
            return Err(Error::PaymentConflict(
                "credential retry does not match the submitted payment proof".to_owned(),
            ));
        }
        Ok(())
    }

    async fn load_settlement_material(
        &self,
        intent: &PaymentIntent,
    ) -> Result<(String, crate::protocol::ProtocolSettlement)> {
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::CredentialVerified, 0)
            .await?
            .ok_or_else(|| Error::Storage("credential verification event is missing".to_owned()))?;
        let verification_ref = event
            .payload
            .get("verification_ref")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Storage("verification reference is missing".to_owned()))?;
        let (_, settlement) = self.load_submission(intent).await?;
        Ok((verification_ref, settlement))
    }

    async fn load_submission(&self, intent: &PaymentIntent) -> Result<(String, crate::protocol::ProtocolSettlement)> {
        let event = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Submitted, 0)
            .await?
            .ok_or_else(|| Error::Storage("durable submission event is missing".to_owned()))?;
        let fingerprint = event
            .payload
            .get("credential_fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Storage("durable credential fingerprint is missing".to_owned()))?;
        let sealed = event
            .payload
            .get("sealed_settlement")
            .cloned()
            .map(serde_json::from_value::<SealedSensitiveMaterial>)
            .transpose()?
            .ok_or_else(|| Error::Storage("durable settlement material is missing".to_owned()))?;
        let plaintext = self
            .material_protector
            .open(&settlement_material_context(intent), &sealed)?;
        Ok((fingerprint, serde_json::from_slice(&plaintext)?))
    }

    async fn load_receipt(&self, intent: &PaymentIntent) -> Result<PaymentReceipt> {
        let settled = self
            .events
            .find_for_intent_type(&intent.id, PaymentEventType::Settled, 0)
            .await?;
        let reconciled = if settled.is_some() {
            None
        } else {
            self.events
                .find_for_intent_type(&intent.id, PaymentEventType::Reconciled, 0)
                .await?
        };
        durable_receipt(settled.as_ref(), reconciled.as_ref())
    }
}

fn durable_receipt(settled: Option<&PaymentEvent>, reconciled: Option<&PaymentEvent>) -> Result<PaymentReceipt> {
    settled
        .or(reconciled)
        .and_then(|event| event.payload.get("receipt").cloned())
        .map(serde_json::from_value)
        .transpose()?
        .ok_or_else(|| Error::Storage("settled intent has no durable receipt".to_owned()))
}

fn is_concurrent_settlement_winner(expected: &PaymentIntent, current: &PaymentIntent) -> bool {
    current.status == PaymentStatus::Settled && current.same_immutable_identity_as(expected)
}

fn authorization_query(
    intent: &PaymentIntent,
    settlement: &crate::protocol::ProtocolSettlement,
    from_block: u64,
) -> Result<(EvmAuthorizationQuery, i64)> {
    if !uses_eip3009_recovery(intent) || settlement.protocol != "x402" {
        return Err(Error::Settlement(
            "EIP-3009 recovery requires an inbound x402 intent".to_owned(),
        ));
    }
    let token_contract = intent
        .asset
        .token_contract
        .clone()
        .ok_or_else(|| Error::Settlement("x402 recovery intent has no token contract".to_owned()))?;
    let authorization = eip3009_authorization_identity(&settlement.payment_payload)?;
    Ok((
        EvmAuthorizationQuery {
            network: intent.asset.network.clone(),
            token_contract,
            payer: authorization.payer,
            nonce: authorization.nonce,
            recipient: intent.recipient.clone(),
            amount: intent.amount,
            from_block,
        },
        authorization.valid_before,
    ))
}

fn uses_eip3009_recovery(intent: &PaymentIntent) -> bool {
    intent.direction == PaymentDirection::Inbound
        && intent.protocol == "x402"
        && is_evm_payment_network(&intent.asset.network)
}

fn settlement_request_payload(intent: &PaymentIntent, from_block: Option<u64>) -> Value {
    let mut payload = serde_json::Map::from_iter([
        ("run_id".to_owned(), Value::String(intent.run_id.clone())),
        (
            "recovery".to_owned(),
            Value::String(
                if uses_eip3009_recovery(intent) {
                    "eip3009_authorization_event"
                } else {
                    "provider_verify"
                }
                .to_owned(),
            ),
        ),
    ]);
    if let Some(from_block) = from_block {
        payload.insert("from_block".to_owned(), Value::from(from_block));
    }
    Value::Object(payload)
}

fn settlement_request_from_block(intent: &PaymentIntent, event: &PaymentEvent) -> Result<Option<u64>> {
    let payload = event
        .payload
        .as_object()
        .ok_or_else(|| Error::Storage("settlement operation payload is malformed".to_owned()))?;
    let expected_recovery = if uses_eip3009_recovery(intent) {
        "eip3009_authorization_event"
    } else {
        "provider_verify"
    };
    let exact_fields = payload.len() == if uses_eip3009_recovery(intent) { 3 } else { 2 }
        && payload.contains_key("run_id")
        && payload.contains_key("recovery")
        && (!uses_eip3009_recovery(intent) || payload.contains_key("from_block"));
    if event.intent_id != intent.id
        || event.event_type != PaymentEventType::SettlementRequested
        || event.attempt_no != 0
        || event.signature_ref.is_some()
        || event.tx_hash.is_some()
        || !exact_fields
        || payload.get("run_id").and_then(Value::as_str) != Some(intent.run_id.as_str())
        || payload.get("recovery").and_then(Value::as_str) != Some(expected_recovery)
    {
        return Err(Error::Storage(
            "durable settlement operation does not match the payment intent".to_owned(),
        ));
    }
    let from_block = payload.get("from_block").map(|value| {
        value
            .as_u64()
            .ok_or_else(|| Error::Storage("settlement recovery checkpoint is malformed".to_owned()))
    });
    from_block.transpose()
}

fn authorization_expiry_deadline(intent: &PaymentIntent, valid_before: i64) -> i64 {
    intent.expires_at.unwrap_or(valid_before).max(valid_before)
}

fn authorization_is_definitively_expired(observed_at: i64, chain_observed_at: i64, expiry: i64) -> bool {
    observed_at >= expiry && chain_observed_at >= expiry
}

fn reservation_after_non_settlement(status: PaymentStatus, reservation_status: ReservationStatus) -> ReservationStatus {
    if matches!(status, PaymentStatus::Failed | PaymentStatus::Expired)
        && reservation_status == ReservationStatus::Reserved
    {
        ReservationStatus::Released
    } else {
        reservation_status
    }
}

fn validate_issue_command(command: &IssuePaymentChallenge) -> Result<()> {
    validate_identifier("scenario_id", &command.scenario_id)?;
    validate_identifier("run_id", &command.run_id)?;
    validate_identifier("offer_id", &command.offer_id)?;
    validate_identifier("protocol", &command.protocol)?;
    validate_identifier("request_hash", &command.request_hash)?;
    if command.resource.trim().is_empty() || canonical_http_origin(&command.resource)? != command.origin {
        return Err(Error::InvalidRequest(
            "resource and canonical origin binding is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn build_inbound_intent(
    command: &IssuePaymentChallenge,
    offer: &PaymentOffer,
    wallet: &crate::model::wallet::Wallet,
    challenge: &PaymentChallenge,
    now: i64,
) -> Result<PaymentIntent> {
    let intent = PaymentIntent {
        id: Uuid::new_v4().to_string(),
        scenario_id: command.scenario_id.clone(),
        run_id: command.run_id.clone(),
        wallet_id: Some(offer.wallet_id.clone()),
        policy_id: Some(offer.policy_id.clone()),
        offer_id: Some(offer.id.clone()),
        direction: PaymentDirection::Inbound,
        protocol: command.protocol.clone(),
        asset: offer.asset.clone(),
        recipient: wallet.address.clone(),
        amount: offer.amount,
        settled_amount: Amount::ZERO,
        resource: offer.resource.clone(),
        challenge_hash: Some(challenge.challenge_hash.clone()),
        request_hash: command.request_hash.clone(),
        status: PaymentStatus::Created,
        reservation_status: ReservationStatus::Reserved,
        reserved_amount: offer.amount,
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at: now,
        updated_at: now,
        expires_at: Some(challenge.expires_at),
        settled_at: None,
    };
    intent
        .validate()
        .map_err(|field| Error::InvalidRequest(format!("invalid inbound intent field: {field}")))?;
    Ok(intent)
}

fn validate_verify_command(command: &VerifyIncomingPayment) -> Result<()> {
    validate_identifier("scenario_id", &command.scenario_id)?;
    validate_identifier("run_id", &command.run_id)?;
    validate_identifier("challenge_hash", &command.challenge_hash)?;
    validate_identifier("request_hash", &command.request_hash)?;
    validate_identifier("credential.protocol", &command.credential.protocol)?;
    validate_identifier("credential.challenge_hash", &command.credential.challenge_hash)?;
    validate_identifier("credential.credential_hash", &command.credential.credential_hash)?;
    if command.credential.challenge_hash != command.challenge_hash {
        return Err(Error::PaymentConflict(
            "credential is bound to another challenge".to_owned(),
        ));
    }
    Ok(())
}

fn validate_settle_command(command: &SettleIncomingPayment) -> Result<()> {
    validate_identifier("scenario_id", &command.scenario_id)?;
    validate_identifier("run_id", &command.run_id)?;
    validate_identifier("intent_id", &command.intent_id)?;
    validate_identifier("verification_ref", &command.verification_ref)
}

fn ensure_settlement_intent_binding(intent: &PaymentIntent, command: &SettleIncomingPayment) -> Result<()> {
    if intent.run_id != command.run_id || intent.direction != PaymentDirection::Inbound {
        return Err(Error::Forbidden);
    }
    Ok(())
}

fn ensure_replay_identity(intent: &PaymentIntent, command: &IssuePaymentChallenge) -> Result<()> {
    if intent.scenario_id != command.scenario_id
        || intent.run_id != command.run_id
        || intent.offer_id.as_deref() != Some(command.offer_id.as_str())
        || intent.resource != command.resource
        || intent.protocol != command.protocol
        || intent.request_hash != command.request_hash
    {
        return Err(Error::PaymentConflict(
            "run_id was reused for different challenge content".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_incoming_identity(intent: &PaymentIntent, run_id: &str, request_hash: &str) -> Result<()> {
    if intent.direction != PaymentDirection::Inbound || intent.run_id != run_id {
        return Err(Error::Forbidden);
    }
    match intent.check_replay(request_hash) {
        crate::model::ReplayCheck::Replay => Ok(()),
        crate::model::ReplayCheck::Conflict => Err(Error::PaymentConflict(
            "run_id was reused for different request content".to_owned(),
        )),
    }
}

fn validate_payee_challenge(
    challenge: &PaymentChallenge,
    offer: &PaymentOffer,
    wallet: &crate::model::wallet::Wallet,
    now: i64,
) -> Result<()> {
    if challenge.scenario_id != offer.scenario_id
        || challenge.offer_id.as_deref() != Some(offer.id.as_str())
        || challenge.resource != offer.resource
        || challenge.asset != offer.asset
        || challenge.amount != offer.amount
        || !wallet.address_matches(&challenge.recipient)
        || challenge.expires_at <= now
        || challenge.challenge_hash.trim().is_empty()
    {
        return Err(Error::PaymentProtocol(
            "adapter produced a challenge inconsistent with the offer".to_owned(),
        ));
    }
    Ok(())
}

fn evidence_hash(value: &Value) -> Result<String> {
    let encoded = serde_json::to_vec(value)?;
    let mut hash = Sha256::new();
    hash.update(b"agentvault.settlement-evidence.v1");
    hash.update(u64::try_from(encoded.len()).unwrap_or(u64::MAX).to_be_bytes());
    hash.update(encoded);
    Ok(hex::encode(hash.finalize()))
}

pub(crate) fn provider_verification_ref(intent_id: &str, result: &SettlementProviderResult) -> Result<String> {
    evidence_hash(&json!({
        "intent_id": intent_id,
        "operation": "verify",
        "result": result,
    }))
}

fn next_timestamp(previous: i64, now: i64) -> i64 {
    now.max(previous.saturating_add(1))
}

fn settlement_material_context(intent: &PaymentIntent) -> String {
    format!("payment-intent:{}:{}", intent.id, intent.protocol)
}

fn confirmed_intent(
    intent: &PaymentIntent,
    result: &SettlementProviderResult,
    updated_at: i64,
    effective_at: i64,
) -> PaymentIntent {
    let mut settled = intent.clone();
    settled.status = PaymentStatus::Settled;
    settled.settled_amount = intent.amount;
    settled.tx_hash.clone_from(&result.transaction);
    settled.settlement_ref.clone_from(&result.settlement_ref);
    settled.next_reconcile_at = None;
    if settled.reservation_status == ReservationStatus::Reserved {
        settled.reservation_status = ReservationStatus::Consumed;
    }
    settled.updated_at = updated_at;
    // Preserve the provider timestamp in the receipt and ledger, while the
    // local intent lifecycle remains monotonic across bounded clock skew.
    settled.settled_at = Some(effective_at.max(intent.created_at));
    settled
}

fn confirmed_receipt(
    intent: &PaymentIntent,
    result: &SettlementProviderResult,
    confirmed_at: i64,
) -> Result<PaymentReceipt> {
    let receipt = PaymentReceipt {
        intent_id: intent.id.clone(),
        scenario_id: intent.scenario_id.clone(),
        direction: PaymentDirection::Inbound,
        protocol: intent.protocol.clone(),
        asset: intent.asset.clone(),
        recipient: intent.recipient.clone(),
        payer: result.payer.clone(),
        requested_amount: intent.amount,
        settled_amount: intent.amount,
        status: PaymentStatus::Settled,
        tx_hash: result.transaction.clone(),
        settlement_ref: result.settlement_ref.clone(),
        evidence_hash: evidence_hash(&serde_json::to_value(result)?)?,
        confirmed_at,
    };
    receipt
        .validate()
        .map_err(|field| Error::Settlement(format!("invalid confirmed receipt field: {field}")))?;
    Ok(receipt)
}

fn confirmed_ledger(
    intent: &PaymentIntent,
    authority: &str,
    result: &SettlementProviderResult,
    effective_at: i64,
    created_at: i64,
) -> Result<PaymentLedgerEntry> {
    let posting_key = settlement_posting_key(
        authority,
        &intent.asset.network,
        result.transaction.as_deref(),
        result.settlement_ref.as_deref(),
        result.effect_ref.as_deref(),
    )
    .map_err(|field| Error::Settlement(format!("invalid settlement posting evidence: {field}")))?;
    let ledger = PaymentLedgerEntry {
        id: Uuid::new_v4().to_string(),
        intent_id: Some(intent.id.clone()),
        scenario_id: Some(intent.scenario_id.clone()),
        wallet_id: intent
            .wallet_id
            .clone()
            .ok_or_else(|| Error::Storage("inbound intent has no wallet".to_owned()))?,
        entry_type: LedgerEntryType::Payment,
        direction: PaymentDirection::Inbound,
        asset: intent.asset.clone(),
        amount: intent.amount,
        posting_key,
        tx_hash: result.transaction.clone(),
        settlement_ref: result.settlement_ref.clone(),
        effect_ref: result.effect_ref.clone(),
        transfer_ref: None,
        reversal_of: None,
        effective_at,
        created_at,
    };
    ledger
        .validate()
        .map_err(|field| Error::Settlement(format!("invalid ledger field: {field}")))?;
    Ok(ledger)
}

fn confirmed_event(
    intent: &PaymentIntent,
    result: &SettlementProviderResult,
    receipt: &PaymentReceipt,
    confirmed_at: i64,
) -> PaymentEvent {
    PaymentEvent {
        id: Uuid::new_v4().to_string(),
        intent_id: intent.id.clone(),
        event_type: PaymentEventType::Settled,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: result.transaction.clone(),
        payload: json!({"receipt": receipt}),
        created_at: confirmed_at,
    }
}

fn confirmed_outbox(intent: &PaymentIntent, confirmed_at: i64) -> OutboxMessage {
    OutboxMessage {
        id: Uuid::new_v4().to_string(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: intent.id.clone(),
        event_type: "payment_settled".to_owned(),
        destination: format!("scenario:{}.payments", intent.scenario_id),
        run_id: intent.run_id.clone(),
        payload: json!({"intent_id": intent.id, "scenario_id": intent.scenario_id, "run_id": intent.run_id}),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        available_at: confirmed_at,
        lease_owner: None,
        lease_expires_at: None,
        last_error: None,
        created_at: confirmed_at,
        delivered_at: None,
    }
}

fn settled_response(intent: PaymentIntent, receipt: PaymentReceipt) -> SettledIncomingPayment {
    SettledIncomingPayment { intent, receipt }
}

#[derive(Clone, Debug)]
pub struct SettlementProviderConfig {
    pub base_url: String,
    pub verify_path: String,
    pub settle_path: String,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    /// Plain HTTP is accepted only for a loopback provider and only when this
    /// development switch is explicit.
    pub allow_insecure_loopback: bool,
}

impl Default for SettlementProviderConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            verify_path: "verify".to_owned(),
            settle_path: "settle".to_owned(),
            timeout: Duration::from_secs(10),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            allow_insecure_loopback: false,
        }
    }
}

/// Redirect-free HTTP adapter for hosted facilitators and providers.
///
/// Paths are fixed at construction time. Credentials are therefore never
/// forwarded to a URL supplied by a payment challenge.
pub struct HttpSettlementProvider {
    transport: FixedHttpJsonTransport,
}

impl HttpSettlementProvider {
    /// Builds a provider client with an optional pre-validated secret header
    /// map. Header values are intentionally not retained in configuration
    /// models that may be serialized or logged.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe endpoint, invalid fixed paths, zero
    /// limits, or an HTTP client construction failure.
    pub fn new(config: &SettlementProviderConfig, headers: HeaderMap) -> Result<Self> {
        Ok(Self {
            transport: FixedHttpJsonTransport::new(config, headers)?,
        })
    }
}

#[async_trait]
impl ExternalSettlementProvider for HttpSettlementProvider {
    async fn execute(
        &self,
        operation: SettlementOperation,
        request: &SettlementProviderRequest,
    ) -> Result<SettlementProviderResult> {
        let endpoint = match operation {
            SettlementOperation::Verify => ProviderHttpEndpoint::Verify,
            SettlementOperation::Settle => ProviderHttpEndpoint::Settle,
        };
        let response = self.transport.post_json(endpoint, &request.run_id, request).await?;
        if !response.status.is_success() {
            return Err(Error::Settlement(format!("provider returned HTTP {}", response.status)));
        }
        serde_json::from_slice(&response.body)
            .map_err(|error| Error::Settlement(format!("invalid provider response: {error}")))
    }
}

#[cfg(test)]
#[path = "../../tests/handler/settlement_handler_test.rs"]
mod tests;
