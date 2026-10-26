use crate::config::ApprovalJwtConfig;
use crate::config::SecretRefConfig;
use crate::model::ApprovalDecision;
use crate::model::ApprovalDecisionKind;
use crate::model::ApprovalGrant;
use crate::model::ApprovalGrantClaims;
use crate::model::ApprovalRequest;
use crate::model::ApprovalStatus;
use crate::model::VerifiedApprovalGrant;
use crate::utils::secret::SecretReader;
use crate::utils::secure_material::SensitiveMaterialProtector;
use crate::Error;
use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use jsonwebtoken::{decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, fmt, sync::Arc};
use uuid::Uuid;
use zeroize::Zeroizing;

const APPROVAL_JWT_KEY_BYTES: usize = 32;
const MAX_APPROVAL_JWT_SECRET_BYTES: usize = 1024;
const APPROVAL_CLAIMS_HASH_DOMAIN: &[u8] = b"agentvault.approval-jwt-claims.v1";

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
        JwtApprovalGrantTokenService::validate_jwt_identity("issuer", &issuer)?;
        JwtApprovalGrantTokenService::validate_jwt_identity("audience", &audience)?;
        JwtApprovalGrantTokenService::validate_key_id(&active_key_id)?;
        let mut key_ring = BTreeMap::new();
        for (key_id, key) in signing_keys {
            JwtApprovalGrantTokenService::validate_key_id(&key_id)?;
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
            .map(|(key_id, reference)| {
                JwtApprovalGrantTokenService::load_signing_key(key_id, reference).map(|key| (key_id.clone(), key))
            })
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
            || !JwtApprovalGrantTokenService::constant_time_eq(claims_hash.as_bytes(), stored_hash.as_bytes())
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
            &JwtApprovalGrantTokenService::grant_material_context(&claims.approval_id, &claims.jti),
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
        let token = self.material_protector.open(
            &JwtApprovalGrantTokenService::grant_material_context(&approval.id, jti),
            sealed,
        )?;
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

impl JwtApprovalGrantTokenService {
    pub(crate) fn grant_material_context(approval_id: &str, jti: &str) -> String {
        format!("approval:{approval_id}:grant:{jti}")
    }

    pub(crate) fn load_signing_key(
        key_id: &str,
        reference: &SecretRefConfig,
    ) -> Result<Zeroizing<[u8; APPROVAL_JWT_KEY_BYTES]>> {
        JwtApprovalGrantTokenService::validate_key_id(key_id)?;
        let encoded = SecretReader::read_ref(
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

    pub(crate) fn validate_jwt_identity(field: &str, value: &str) -> Result<()> {
        if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(Error::Config(format!("approval JWT {field} is invalid")));
        }
        Ok(())
    }

    pub(crate) fn validate_key_id(key_id: &str) -> Result<()> {
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

    pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
        if left.len() != right.len() {
            return false;
        }
        left.iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

#[cfg(test)]
#[path = "../../tests/payment/approval_grant_test.rs"]
mod tests;
