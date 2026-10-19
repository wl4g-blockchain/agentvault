use std::{collections::BTreeMap, fmt, sync::Arc};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use jsonwebtoken::{decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    config::{ApprovalJwtConfig, SecretRefConfig},
    model::{
        Amount, ApprovalDecision, ApprovalDecisionKind, ApprovalGrant, ApprovalGrantClaims, ApprovalRequest,
        ApprovalStatus, PaymentEvent, PaymentEventType, PaymentIntent, PaymentStatus, ReservationStatus,
        VerifiedApprovalGrant,
    },
    storage::{
        ApprovalConsumption, ApprovalIntentTransition, ApprovalReservationGuard, ApprovalStorage, ApprovalWrite,
        ApproverStorage, OutboxMessage, OutboxStatus, PaymentStorage,
    },
    utils::secret::read_secret_ref,
    utils::secure_material::SensitiveMaterialProtector,
    Error, Result,
};

use super::common::{canonical_http_origin, utc_day_start};

const APPROVAL_JWT_KEY_BYTES: usize = 32;
const MAX_APPROVAL_JWT_SECRET_BYTES: usize = 1024;
const APPROVAL_CLAIMS_HASH_DOMAIN: &[u8] = b"agentvault.approval-jwt-claims.v1";

#[async_trait]
pub trait ApprovalPrincipalAuthorizer: Send + Sync {
    async fn authorize_approver(&self, principal: &str, scenario_id: &str) -> Result<()>;
}

/// Durable scenario-admin authorization backed by the same SQL authority as
/// payment ownership. No transport credential implicitly grants approval
/// rights, and disabled Client/Scenario rows fail closed inside the store.
pub struct StoredApprovalPrincipalAuthorizer {
    approvers: Arc<dyn ApproverStorage>,
}

impl StoredApprovalPrincipalAuthorizer {
    #[must_use]
    pub fn new(approvers: Arc<dyn ApproverStorage>) -> Self {
        Self { approvers }
    }
}

#[async_trait]
impl ApprovalPrincipalAuthorizer for StoredApprovalPrincipalAuthorizer {
    async fn authorize_approver(&self, principal: &str, scenario_id: &str) -> Result<()> {
        if principal == super::UNRESTRICTED_PRINCIPAL {
            return Ok(());
        }
        if principal.trim().is_empty() || scenario_id.trim().is_empty() {
            return Err(Error::Forbidden);
        }
        if self.approvers.is_authorized(scenario_id, principal).await? {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}

/// Issues and validates one-time approval JWTs without exposing plaintext
/// tokens to handlers, storage adapters, transports, or callers.
pub trait ApprovalGrantTokenService: Send + Sync {
    /// Signs a fresh JWT and returns only its claims, hash, and AEAD envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when bindings, key material, serialization, signing,
    /// or envelope protection fails.
    fn issue(&self, approval: &ApprovalRequest, decision: &ApprovalDecision, expires_at: i64) -> Result<ApprovalGrant>;

    /// Opens the stored envelope, verifies JWT signature and claims, and
    /// returns authenticated claims for the immediate consume transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the envelope cannot be opened or the JWT key,
    /// signature, lifetime, audience, or intent bindings are invalid.
    fn verify(&self, approval: &ApprovalRequest, now: i64) -> Result<VerifiedApprovalGrant>;
}

/// HS256 JWT service with an immutable, versioned signing key ring.
///
/// `kid` selects a configured key; the active key signs new grants while old
/// keys remain verification-only until their grants terminate. Full compact
/// JWT strings exist only inside zeroizing temporary buffers and are sealed by
/// the supplied [`SensitiveMaterialProtector`] before persistence.
pub struct JwtApprovalGrantTokenService {
    issuer: String,
    audience: String,
    active_key_id: String,
    signing_keys: BTreeMap<String, Zeroizing<[u8; APPROVAL_JWT_KEY_BYTES]>>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
}

impl fmt::Debug for JwtApprovalGrantTokenService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JwtApprovalGrantTokenService")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("active_key_id", &self.active_key_id)
            .field("key_ids", &self.signing_keys.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl JwtApprovalGrantTokenService {
    /// Builds a service from already resolved 32-byte HMAC keys.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid JWT identity, key identifiers, duplicate
    /// keys, an empty ring, or a missing active key.
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        active_key_id: impl Into<String>,
        signing_keys: impl IntoIterator<Item = (String, [u8; APPROVAL_JWT_KEY_BYTES])>,
        material_protector: Arc<dyn SensitiveMaterialProtector>,
    ) -> Result<Self> {
        Self::from_resolved_keys(
            issuer,
            audience,
            active_key_id,
            signing_keys
                .into_iter()
                .map(|(key_id, key)| (key_id, Zeroizing::new(key))),
            material_protector,
        )
    }

    fn from_resolved_keys(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        active_key_id: impl Into<String>,
        signing_keys: impl IntoIterator<Item = (String, Zeroizing<[u8; APPROVAL_JWT_KEY_BYTES]>)>,
        material_protector: Arc<dyn SensitiveMaterialProtector>,
    ) -> Result<Self> {
        let issuer = issuer.into();
        let audience = audience.into();
        let active_key_id = active_key_id.into();
        validate_jwt_identity("issuer", &issuer)?;
        validate_jwt_identity("audience", &audience)?;
        validate_key_id(&active_key_id)?;
        let mut key_ring = BTreeMap::new();
        for (key_id, key) in signing_keys {
            validate_key_id(&key_id)?;
            if key_ring.insert(key_id.clone(), key).is_some() {
                return Err(Error::Config(format!("duplicate approval JWT key id: {key_id}")));
            }
        }
        if key_ring.is_empty() || !key_ring.contains_key(&active_key_id) {
            return Err(Error::Config(
                "approval JWT active signing key is missing from the key ring".to_owned(),
            ));
        }
        Ok(Self {
            issuer,
            audience,
            active_key_id,
            signing_keys: key_ring,
            material_protector,
        })
    }

    /// Resolves the configured versioned keys from file/environment
    /// [`SecretRefConfig`] values. Secret contents must be unpadded base64url
    /// encodings of exactly 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when a reference cannot be read or decoded, or when
    /// the resulting service configuration is invalid.
    pub fn from_config(
        config: &ApprovalJwtConfig,
        material_protector: Arc<dyn SensitiveMaterialProtector>,
    ) -> Result<Self> {
        let keys = config
            .signing_keys
            .iter()
            .map(|(key_id, reference)| load_signing_key(key_id, reference).map(|key| (key_id.clone(), key)))
            .collect::<Result<Vec<_>>>()?;
        Self::from_resolved_keys(
            config.issuer.clone(),
            config.audience.clone(),
            config.active_key_id.clone(),
            keys,
            material_protector,
        )
    }

    fn claims_hash(claims: &ApprovalGrantClaims) -> Result<String> {
        let encoded = serde_json::to_vec(claims)?;
        let encoded_len =
            u64::try_from(encoded.len()).map_err(|_| Error::Crypto("approval JWT claims are too large".to_owned()))?;
        let mut hasher = Sha256::new();
        hasher.update(APPROVAL_CLAIMS_HASH_DOMAIN);
        hasher.update(encoded_len.to_be_bytes());
        hasher.update(encoded);
        Ok(hex::encode(hasher.finalize()))
    }

    fn validate_stored_claims(
        &self,
        approval: &ApprovalRequest,
        claims: &ApprovalGrantClaims,
        claims_hash: &str,
        now: i64,
    ) -> Result<()> {
        claims.validate().map_err(|_| Error::Forbidden)?;
        let stored_amount = approval
            .approved_amount
            .ok_or_else(|| Error::Storage("approved row has no amount".to_owned()))?;
        let stored_approver = approval
            .approved_by
            .as_deref()
            .ok_or_else(|| Error::Storage("approved row has no approver".to_owned()))?;
        let stored_jti = approval
            .grant_jti
            .as_deref()
            .ok_or_else(|| Error::Storage("approved row has no grant jti".to_owned()))?;
        let stored_hash = approval
            .grant_claims_hash
            .as_deref()
            .ok_or_else(|| Error::Storage("approved row has no grant claims hash".to_owned()))?;
        if claims.iss != self.issuer
            || claims.aud != self.audience
            || claims.jti != stored_jti
            || claims.approval_id != approval.id
            || claims.intent_id != approval.intent_id
            || claims.intent_hash != approval.intent_hash
            || claims.amount != stored_amount
            || claims.approver != stored_approver
            || Some(claims.iat) != approval.approved_at
            || claims.exp != approval.expires_at
            || claims.iat > now
            || claims.exp <= now
            || !constant_time_eq(claims_hash.as_bytes(), stored_hash.as_bytes())
        {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
}

impl ApprovalGrantTokenService for JwtApprovalGrantTokenService {
    fn issue(&self, approval: &ApprovalRequest, decision: &ApprovalDecision, expires_at: i64) -> Result<ApprovalGrant> {
        if decision.decision != ApprovalDecisionKind::Approve
            || approval.id != decision.approval_id
            || approval.intent_id != decision.intent_id
        {
            return Err(Error::InvalidRequest(
                "approval JWT can only be issued for the bound approval decision".to_owned(),
            ));
        }
        let amount = decision
            .approved_amount
            .ok_or_else(|| Error::InvalidRequest("approved amount is required".to_owned()))?;
        let claims = ApprovalGrantClaims {
            iss: self.issuer.clone(),
            aud: self.audience.clone(),
            jti: Uuid::new_v4().to_string(),
            approval_id: approval.id.clone(),
            intent_id: approval.intent_id.clone(),
            intent_hash: approval.intent_hash.clone(),
            amount,
            approver: decision.approved_by.clone(),
            iat: decision.decided_at,
            exp: expires_at,
        };
        claims
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid approval JWT claim: {field}")))?;
        let key = self
            .signing_keys
            .get(&self.active_key_id)
            .ok_or_else(|| Error::Crypto("active approval JWT signing key is unavailable".to_owned()))?;
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(self.active_key_id.clone());
        header.typ = Some("JWT".to_owned());
        let token = encode(&header, &claims, &EncodingKey::from_secret(key.as_ref()))
            .map(Zeroizing::new)
            .map_err(|_| Error::Crypto("approval JWT signing failed".to_owned()))?;
        let sealed_token = self.material_protector.seal(
            &grant_material_context(&claims.approval_id, &claims.jti),
            token.as_bytes(),
        )?;
        Ok(ApprovalGrant {
            claims_hash: Self::claims_hash(&claims)?,
            claims,
            sealed_token,
        })
    }

    fn verify(&self, approval: &ApprovalRequest, now: i64) -> Result<VerifiedApprovalGrant> {
        if !matches!(approval.status, ApprovalStatus::Approved | ApprovalStatus::Consumed)
            || !approval.has_persisted_grant()
        {
            return Err(Error::InvalidTransition(
                "approval JWT is not available for this approval".to_owned(),
            ));
        }
        let jti = approval
            .grant_jti
            .as_deref()
            .ok_or_else(|| Error::Storage("approved row has no grant jti".to_owned()))?;
        let sealed = approval
            .sealed_grant_token
            .as_ref()
            .ok_or_else(|| Error::Storage("approved row has no sealed grant JWT".to_owned()))?;
        let token = self
            .material_protector
            .open(&grant_material_context(&approval.id, jti), sealed)?;
        let token =
            std::str::from_utf8(&token).map_err(|_| Error::Crypto("sealed approval JWT is invalid".to_owned()))?;
        let header = decode_header(token).map_err(|_| Error::Forbidden)?;
        if header.alg != Algorithm::HS256 || header.typ.as_deref() != Some("JWT") {
            return Err(Error::Forbidden);
        }
        let key_id = header.kid.as_deref().ok_or(Error::Forbidden)?;
        let key = self.signing_keys.get(key_id).ok_or(Error::Forbidden)?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_issuer(&[self.issuer.as_str()]);
        let decoded = decode::<ApprovalGrantClaims>(token, &DecodingKey::from_secret(key.as_ref()), &validation)
            .map_err(|_| Error::Forbidden)?;
        let claims_hash = Self::claims_hash(&decoded.claims)?;
        self.validate_stored_claims(approval, &decoded.claims, &claims_hash, now)?;
        Ok(VerifiedApprovalGrant {
            claims: decoded.claims,
            claims_hash,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitApprovalDecision {
    pub approval_id: String,
    pub intent_id: String,
    pub decision: ApprovalDecisionKind,
    pub approved_amount: Option<Amount>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalDecisionResult {
    pub approval: ApprovalRequest,
}

pub struct ApprovalHandler {
    approvals: Arc<dyn ApprovalStorage>,
    payments: Arc<dyn PaymentStorage>,
    authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
    grant_tokens: Arc<dyn ApprovalGrantTokenService>,
    grant_ttl_seconds: i64,
}

impl ApprovalHandler {
    /// Creates an approval handler with a bounded one-time grant lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when the grant TTL is not positive.
    pub fn new(
        approvals: Arc<dyn ApprovalStorage>,
        payments: Arc<dyn PaymentStorage>,
        authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
        grant_tokens: Arc<dyn ApprovalGrantTokenService>,
        grant_ttl_seconds: i64,
    ) -> Result<Self> {
        if grant_ttl_seconds <= 0 {
            return Err(Error::Config("approval grant TTL must be positive".to_owned()));
        }
        Ok(Self {
            approvals,
            payments,
            authorizer,
            grant_tokens,
            grant_ttl_seconds,
        })
    }

    /// Applies an authenticated administrator's decision to a pending approval.
    /// Exact retries verify and reuse the JWT already sealed in the database.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval or intent does not exist, their
    /// bindings or lifecycle state are invalid, the principal is not allowed to
    /// approve the scenario, token protection fails, or the atomic write fails.
    pub async fn submit_decision(
        &self,
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> Result<ApprovalDecisionResult> {
        if let Some(replay) = self.replay_decision(principal, request, now).await? {
            return Ok(replay);
        }
        let (approval, intent, decision) = self.decision_context(principal, request, now).await?;
        let grant = if decision.decision == ApprovalDecisionKind::Approve {
            let expires_at = grant_expiration(approval.expires_at, now, self.grant_ttl_seconds);
            Some(self.grant_tokens.issue(&approval, &decision, expires_at)?)
        } else {
            None
        };
        let granted = grant.is_some();
        let mut rejected_intent = None;
        if !granted {
            let mut failed = intent.clone();
            failed.status = PaymentStatus::Failed;
            failed.reservation_status = ReservationStatus::Unreserved;
            failed.reserved_amount = Amount::ZERO;
            failed.next_reconcile_at = None;
            failed.updated_at = next_timestamp(intent.updated_at, now);
            rejected_intent = Some(failed);
        }
        let event = approval_decision_event(&approval, &intent, principal, granted, now);
        let outbox = vec![approval_decision_message(&approval, &intent, granted, now)];
        let persisted = self
            .approvals
            .decide(ApprovalWrite {
                decision: &decision,
                grant: grant.as_ref(),
                intent_transition: rejected_intent.as_ref().map(|failed| ApprovalIntentTransition {
                    intent: failed,
                    expected_updated_at: intent.updated_at,
                }),
                event: &event,
                outbox: &outbox,
            })
            .await?;
        Ok(ApprovalDecisionResult { approval: persisted })
    }

    async fn replay_decision(
        &self,
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> Result<Option<ApprovalDecisionResult>> {
        let approval = self
            .approvals
            .get(&request.approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        if approval.status == ApprovalStatus::Pending {
            return Ok(None);
        }
        if approval.intent_id != request.intent_id {
            return Err(Error::PaymentConflict("approval is bound to another intent".to_owned()));
        }
        let intent = self
            .payments
            .get(&request.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        self.authorizer
            .authorize_approver(principal, &intent.scenario_id)
            .await?;
        if approval.requested_by == principal || approval.approved_by.as_deref() != Some(principal) {
            return Err(Error::Forbidden);
        }

        match (approval.status, request.decision) {
            (ApprovalStatus::Approved, ApprovalDecisionKind::Approve) => {
                if request.approved_amount != approval.approved_amount {
                    return Err(Error::PaymentConflict("approval decision replay changed".to_owned()));
                }
                let verified = self.grant_tokens.verify(&approval, now)?;
                if verified.claims.intent_id != intent.id
                    || verified.claims.intent_hash != intent.request_hash
                    || verified.claims.amount != intent.amount
                {
                    return Err(Error::PaymentConflict("approval decision replay changed".to_owned()));
                }
                Ok(Some(ApprovalDecisionResult { approval }))
            }
            (ApprovalStatus::Consumed, ApprovalDecisionKind::Approve)
                if request.approved_amount == approval.approved_amount =>
            {
                // Consumption is the terminal proof that the persisted grant
                // was already authenticated and reserved exactly once. An
                // exact administrator retry must remain recoverable after the
                // short-lived JWT expires and must never consume it again.
                Ok(Some(ApprovalDecisionResult { approval }))
            }
            (ApprovalStatus::Rejected, ApprovalDecisionKind::Reject) if request.approved_amount.is_none() => {
                Ok(Some(ApprovalDecisionResult { approval }))
            }
            _ => Err(Error::PaymentConflict("approval decision replay changed".to_owned())),
        }
    }

    async fn decision_context(
        &self,
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> Result<(ApprovalRequest, PaymentIntent, ApprovalDecision)> {
        let approval = self
            .approvals
            .get(&request.approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        let intent = self
            .payments
            .get(&request.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        if approval.intent_id != intent.id
            || approval.intent_id != request.intent_id
            || approval.intent_hash != intent.request_hash
        {
            return Err(Error::PaymentConflict("approval is bound to another intent".to_owned()));
        }
        self.authorizer
            .authorize_approver(principal, &intent.scenario_id)
            .await?;
        if approval.requested_by == principal {
            return Err(Error::Forbidden);
        }
        if request.decision == ApprovalDecisionKind::Approve && request.approved_amount != Some(intent.amount) {
            return Err(Error::InvalidRequest(
                "fixed-charge approval must authorize the exact intent amount".to_owned(),
            ));
        }
        let decision = approval_decision_from_request(principal, request, now);
        approval
            .validate_decision(&decision, now)
            .map_err(|field| Error::InvalidTransition(format!("invalid approval decision: {field}")))?;
        Ok((approval, intent, decision))
    }

    /// Verifies and atomically consumes the internally stored one-time JWT,
    /// then reserves the bound payment budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval or intent is missing, the principal
    /// is unauthorized, the sealed JWT is invalid or expired, the grant was
    /// already consumed, or the authoritative reservation transaction fails.
    pub async fn consume(
        &self,
        principal: &str,
        approval_id: &str,
        intent_id: &str,
        now: i64,
    ) -> Result<PaymentIntent> {
        let approval = self
            .approvals
            .get(approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        let mut intent = self
            .payments
            .get(intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        self.authorizer
            .authorize_approver(principal, &intent.scenario_id)
            .await?;
        if approval.intent_id != intent.id
            || approval.intent_hash != intent.request_hash
            || approval.status != ApprovalStatus::Approved
            || intent.status != PaymentStatus::PendingApproval
        {
            return Err(Error::InvalidTransition(
                "approval grant is not available for this intent".to_owned(),
            ));
        }
        let grant = self.grant_tokens.verify(&approval, now)?;
        if grant.claims.intent_id != intent.id
            || grant.claims.intent_hash != intent.request_hash
            || grant.claims.amount != intent.amount
        {
            return Err(Error::Forbidden);
        }
        let expected_updated_at = intent.updated_at;
        intent.status = PaymentStatus::Authorized;
        intent.reservation_status = ReservationStatus::Reserved;
        intent.reserved_amount = grant.claims.amount;
        intent.updated_at = next_timestamp(intent.updated_at, now);
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::ApprovalConsumed,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"approval_id": approval.id}),
            created_at: now,
        };
        let outbox = vec![OutboxMessage {
            id: Uuid::new_v4().to_string(),
            aggregate_type: "payment_intent".to_owned(),
            aggregate_id: intent.id.clone(),
            event_type: "payment_authorized".to_owned(),
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({"approval_id": approval.id, "intent_id": intent.id, "run_id": intent.run_id}),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            available_at: now,
            lease_owner: None,
            lease_expires_at: None,
            last_error: None,
            created_at: now,
            delivered_at: None,
        }];
        let origin = canonical_http_origin(&intent.resource)?;
        self.approvals
            .consume_and_reserve(ApprovalConsumption {
                grant: &grant,
                intent: &intent,
                guard: ApprovalReservationGuard {
                    origin: &origin,
                    day_started_at: utc_day_start(now),
                    evaluated_at: now,
                },
                expected_intent_updated_at: expected_updated_at,
                consumed_at: now,
                event: &event,
                outbox: &outbox,
            })
            .await
    }
}

fn approval_decision_from_request(principal: &str, request: &SubmitApprovalDecision, now: i64) -> ApprovalDecision {
    ApprovalDecision {
        approval_id: request.approval_id.clone(),
        intent_id: request.intent_id.clone(),
        decision: request.decision,
        approved_amount: request.approved_amount,
        approved_by: principal.to_owned(),
        reason: request.reason.clone(),
        decided_at: now,
    }
}

fn next_timestamp(previous: i64, now: i64) -> i64 {
    now.max(previous.saturating_add(1))
}

fn grant_expiration(request_expires_at: i64, decided_at: i64, grant_ttl_seconds: i64) -> i64 {
    request_expires_at.min(decided_at.saturating_add(grant_ttl_seconds))
}

fn approval_decision_event(
    approval: &ApprovalRequest,
    intent: &PaymentIntent,
    principal: &str,
    granted: bool,
    now: i64,
) -> PaymentEvent {
    PaymentEvent {
        id: Uuid::new_v4().to_string(),
        intent_id: intent.id.clone(),
        event_type: if granted {
            PaymentEventType::ApprovalGranted
        } else {
            PaymentEventType::ApprovalRejected
        },
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload: json!({"approval_id": approval.id, "approved_by": principal}),
        created_at: now,
    }
}

fn approval_decision_message(
    approval: &ApprovalRequest,
    intent: &PaymentIntent,
    granted: bool,
    now: i64,
) -> OutboxMessage {
    OutboxMessage {
        id: Uuid::new_v4().to_string(),
        aggregate_type: "payment_approval".to_owned(),
        aggregate_id: approval.id.clone(),
        event_type: if granted {
            "approval_granted".to_owned()
        } else {
            "approval_rejected".to_owned()
        },
        destination: format!("scenario:{}.payments", intent.scenario_id),
        run_id: intent.run_id.clone(),
        payload: json!({"approval_id": approval.id, "intent_id": intent.id, "run_id": intent.run_id}),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        available_at: now,
        lease_owner: None,
        lease_expires_at: None,
        last_error: None,
        created_at: now,
        delivered_at: None,
    }
}

fn grant_material_context(approval_id: &str, jti: &str) -> String {
    format!("approval:{approval_id}:grant:{jti}")
}

fn load_signing_key(key_id: &str, reference: &SecretRefConfig) -> Result<Zeroizing<[u8; APPROVAL_JWT_KEY_BYTES]>> {
    validate_key_id(key_id)?;
    let encoded = read_secret_ref(
        reference,
        &format!("approval JWT signing key {key_id}"),
        MAX_APPROVAL_JWT_SECRET_BYTES,
    )?;
    let encoded = encoded.trim_end_matches(['\r', '\n']);
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map(Zeroizing::new)
        .map_err(|_| Error::Config(format!("approval JWT signing key {key_id} is not valid base64url")))?;
    if decoded.len() != APPROVAL_JWT_KEY_BYTES {
        return Err(Error::Config(format!(
            "approval JWT signing key {key_id} must decode to {APPROVAL_JWT_KEY_BYTES} bytes"
        )));
    }
    let mut key = Zeroizing::new([0_u8; APPROVAL_JWT_KEY_BYTES]);
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn validate_jwt_identity(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(Error::Config(format!("approval JWT {field} is invalid")));
    }
    Ok(())
}

fn validate_key_id(key_id: &str) -> Result<()> {
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(Error::Config("approval JWT key id is invalid".to_owned()));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
#[path = "../../tests/handler/approval_handler_test.rs"]
mod tests;
