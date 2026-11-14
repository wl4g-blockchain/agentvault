use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// An amount expressed in an asset's smallest indivisible unit.
///
/// JSON encodes amounts as decimal strings so values are never rounded by
/// consumers whose native number type cannot represent 128-bit integers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(u128);

impl Amount {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.0.checked_add(rhs.0) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn checked_sub(self, rhs: Self) -> Option<Self> {
        match self.0.checked_sub(rhs.0) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl From<u128> for Amount {
    fn from(value: u128) -> Self {
        Self(value)
    }
}

impl From<Amount> for u128 {
    fn from(value: Amount) -> Self {
        value.0
    }
}

impl FromStr for Amount {
    type Err = std::num::ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for Amount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AmountVisitor;

        impl de::Visitor<'_> for AmountVisitor {
            type Value = Amount;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a non-negative base-10 integer encoded as a string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(AmountVisitor)
    }
}

crate::storage::repository_entity! { "asset_identity", Catalog;
    keys: [], immutable: [], revision: None;
    /// Complete identity of a fungible asset on a particular network.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, sqlx::FromRow)]
    #[serde(deny_unknown_fields)]
    pub struct AssetIdentity {
        pub network: String,
        pub asset: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub token_contract: Option<String>,
        #[sqlx(rename = "asset_decimals", try_from = "i16")]
        pub decimals: u8,
    }
}

/// Unit shared by scalar policy and wallet limits.
///
/// Network and token contract deliberately do not participate: the same
/// asset denomination must consume one shared budget when it moves across
/// supported networks or contract deployments.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct BudgetUnit {
    pub asset: String,
    pub decimals: u8,
}

impl BudgetUnit {
    /// Validates the denomination used by scalar budget amounts.
    ///
    /// # Errors
    ///
    /// Returns `asset` when the asset identifier is empty.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.asset.trim().is_empty() {
            return Err("asset");
        }
        Ok(())
    }

    #[must_use]
    pub fn matches(&self, asset: &AssetIdentity) -> bool {
        self.asset == asset.asset && self.decimals == asset.decimals
    }
}

impl AssetIdentity {
    /// Validates the stable identity fields needed for authorization and
    /// settlement matching.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name when an identity is ambiguous.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.network.trim().is_empty() {
            return Err("network");
        }
        if self.asset.trim().is_empty() {
            return Err("asset");
        }
        if self
            .token_contract
            .as_ref()
            .is_some_and(|contract| contract.trim().is_empty())
        {
            return Err("token_contract");
        }
        Ok(())
    }

    /// Returns the denomination in which policy and wallet limits are
    /// expressed, intentionally excluding network and token contract.
    #[must_use]
    pub fn budget_unit(&self) -> BudgetUnit {
        BudgetUnit {
            asset: self.asset.clone(),
            decimals: self.decimals,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PaymentDirection {
    Inbound,
    Outbound,
}

impl PaymentDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayCheck {
    Replay,
    Conflict,
}

/// Protocol-neutral representation of a merchant payment challenge.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentChallenge {
    pub scenario_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_id: Option<String>,
    pub resource: String,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub amount: Amount,
    pub recipient: String,
    pub nonce: String,
    pub challenge_hash: String,
    pub expires_at: i64,
    /// Adapter-owned fields. The payment core treats this as opaque data.
    pub payload: Value,
}

/// Protocol-neutral credential returned to a payer SDK or received by a payee.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentCredential {
    pub protocol: String,
    pub challenge_hash: String,
    pub credential_hash: String,
    /// Adapter-owned fields. The payment core never infers settlement from it.
    pub payload: Value,
}

/// Result of policy evaluation and budget reservation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum PaymentAuthorizationResult {
    Denied { intent_id: String, reason: String },
    RequiresApproval { intent_id: String, approval_id: String },
    Authorized { intent_id: String, reserved_amount: Amount },
}

/// Authenticated outbound authorization command.
#[derive(Clone, Debug)]
pub struct AuthorizePayment {
    pub scenario_id: String,
    pub run_id: String,
    pub challenge: PaymentChallenge,
}

/// Request to sign one already-authorized outbound intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignAuthorizedPayment {
    pub intent_id: String,
    pub challenge: PaymentChallenge,
}

/// Credential returned after its recovery copy and the durable Signed state
/// have committed atomically. The persisted entity is reused directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedOutboundPayment {
    pub intent: PaymentIntent,
    pub credential: PaymentCredential,
    pub replayed: bool,
}

#[cfg(test)]
#[path = "../../tests/model/payment_test.rs"]
mod tests;

mod entity {
    use serde::{Deserialize, Serialize};

    use crate::{model::Amount, model::AssetIdentity, model::PaymentDirection, model::ReplayCheck};

    #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "snake_case")]
    pub enum PaymentStatus {
        Created,
        PendingApproval,
        Authorized,
        Signed,
        Submitted,
        PendingSettlement,
        Settled,
        Failed,
        Expired,
        Unknown,
    }

    impl PaymentStatus {
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Created => "created",
                Self::PendingApproval => "pending_approval",
                Self::Authorized => "authorized",
                Self::Signed => "signed",
                Self::Submitted => "submitted",
                Self::PendingSettlement => "pending_settlement",
                Self::Settled => "settled",
                Self::Failed => "failed",
                Self::Expired => "expired",
                Self::Unknown => "unknown",
            }
        }

        #[must_use]
        pub const fn is_final(self) -> bool {
            matches!(self, Self::Settled | Self::Failed | Self::Expired)
        }
    }

    /// Durable state of a budget reservation.
    #[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
    #[serde(rename_all = "snake_case")]
    pub enum ReservationStatus {
        #[default]
        Unreserved,
        Reserved,
        Consumed,
        Released,
    }

    impl ReservationStatus {
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Unreserved => "unreserved",
                Self::Reserved => "reserved",
                Self::Consumed => "consumed",
                Self::Released => "released",
            }
        }
    }

    crate::storage::repository_entity! { "pay_intent", ReadOnly;
        keys: ["id"], immutable: ["created_at"], revision: None;
        /// Current persisted state of one inbound or outbound payment operation.
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
        #[serde(deny_unknown_fields)]
        pub struct PaymentIntent {
            pub id: String,
            pub scenario_id: String,
            pub run_id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub wallet_id: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub policy_id: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub price_id: Option<String>,
            pub direction: PaymentDirection,
            pub protocol: String,
            #[sqlx(flatten)]
            pub asset: AssetIdentity,
            pub recipient: String,
            pub amount: Amount,
            pub settled_amount: Amount,
            pub resource: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub challenge_hash: Option<String>,
            pub request_hash: String,
            pub status: PaymentStatus,
            pub reservation_status: ReservationStatus,
            pub reserved_amount: Amount,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub tx_hash: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub settlement_ref: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub next_reconcile_at: Option<i64>,
            pub created_at: i64,
            pub updated_at: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub expires_at: Option<i64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub settled_at: Option<i64>,
        }
    }

    impl PaymentIntent {
        #[must_use]
        pub fn check_replay(&self, request_hash: &str) -> ReplayCheck {
            if self.request_hash == request_hash {
                ReplayCheck::Replay
            } else {
                ReplayCheck::Conflict
            }
        }

        /// Returns the policy and wallet selected internally by `AgentVault`.
        ///
        /// # Errors
        ///
        /// Returns the missing or empty authority field.
        pub fn authority_ids(&self) -> Result<(&str, &str), &'static str> {
            let policy_id = self
                .policy_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("policy_id")?;
            let wallet_id = self
                .wallet_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("wallet_id")?;
            Ok((policy_id, wallet_id))
        }

        #[must_use]
        pub fn same_immutable_identity_as(&self, other: &Self) -> bool {
            self.id == other.id
                && self.scenario_id == other.scenario_id
                && self.run_id == other.run_id
                && self.wallet_id == other.wallet_id
                && self.policy_id == other.policy_id
                && self.price_id == other.price_id
                && self.direction == other.direction
                && self.protocol == other.protocol
                && self.asset == other.asset
                && self.recipient == other.recipient
                && self.amount == other.amount
                && self.resource == other.resource
                && self.challenge_hash == other.challenge_hash
                && self.request_hash == other.request_hash
                && self.created_at == other.created_at
                && self.expires_at == other.expires_at
        }

        #[must_use]
        pub const fn needs_reconciliation(&self) -> bool {
            matches!(
                self.status,
                PaymentStatus::Signed
                    | PaymentStatus::Submitted
                    | PaymentStatus::PendingSettlement
                    | PaymentStatus::Unknown
            )
        }

        /// Checks protocol- and storage-independent invariants.
        ///
        /// # Errors
        ///
        /// Returns the invalid field or invariant name.
        pub fn validate(&self) -> Result<(), &'static str> {
            if self.id.trim().is_empty() {
                return Err("id");
            }
            if self.scenario_id.trim().is_empty() {
                return Err("scenario_id");
            }
            if self.run_id.trim().is_empty() {
                return Err("run_id");
            }
            self.authority_ids()?;
            match self.direction {
                PaymentDirection::Inbound
                    if self
                        .price_id
                        .as_deref()
                        .is_none_or(|price_id| price_id.trim().is_empty()) =>
                {
                    return Err("price_id");
                }
                PaymentDirection::Outbound if self.price_id.is_some() => return Err("price_id"),
                _ => {}
            }
            if self.protocol.trim().is_empty() {
                return Err("protocol");
            }
            self.asset.validate()?;
            if self.recipient.trim().is_empty() {
                return Err("recipient");
            }
            if self.amount.is_zero() {
                return Err("amount");
            }
            if self.request_hash.trim().is_empty() {
                return Err("request_hash");
            }
            if self.settled_amount > self.amount {
                return Err("settled_amount");
            }
            if self.reserved_amount > self.amount {
                return Err("reserved_amount");
            }
            if self.status == PaymentStatus::Settled && self.settled_at.is_none() {
                return Err("settled_at");
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/model/payment_entity_test.rs"
        ));
    }
}
pub use entity::*;

mod receipt {
    use super::{Amount, PaymentIntent};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TransferQuery {
        pub network: String,
        pub transaction: String,
        pub token_contract: String,
        pub payer: String,
        pub recipient: String,
        pub amount: Amount,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct EvmAuthorizationQuery {
        pub network: String,
        pub token_contract: String,
        pub payer: String,
        pub nonce: String,
        pub recipient: String,
        pub amount: Amount,
        pub from_block: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ConfirmedTransfer {
        pub authority: String,
        pub transaction: String,
        pub effect_ref: String,
        pub confirmations: u64,
        pub minimum_confirmations: u64,
        pub effective_at: i64,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ReceiptObservation {
        Confirmed(ConfirmedTransfer),
        Pending,
        Mismatch { reason: String },
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum EvmAuthorizationObservation {
        Confirmed(ConfirmedTransfer),
        Pending,
        NoEffect { observed_block: u64, observed_at: i64 },
        Mismatch { reason: String },
    }

    /// Trusted observation produced after independently checking an outbound
    /// merchant receipt against the configured chain evidence source.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum OutboundEvidenceObservation {
        Confirmed {
            authority: String,
            transaction: String,
            payer: String,
            effect_ref: String,
            confirmations: u64,
            minimum_confirmations: u64,
            effective_at: i64,
        },
        Pending,
        Unknown {
            reason: String,
        },
        Unavailable {
            reason: String,
        },
    }

    #[derive(Clone, Debug)]
    pub struct RecordOutboundPaymentResponse {
        pub scenario_id: String,
        pub run_id: String,
        pub intent_id: String,
        pub protocol: String,
        pub header_name: String,
        pub header_value: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RecordedOutboundEvidence {
        pub intent: PaymentIntent,
        pub accepted: bool,
        pub replayed: bool,
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/model/receipt_test.rs"));
    }
}
pub use receipt::*;

mod receipt_projection {
    use serde::{Deserialize, Serialize};

    use crate::{model::Amount, model::AssetIdentity, model::PaymentDirection, model::PaymentStatus};

    /// Protocol-neutral evidence returned after settlement observation.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentReceipt {
        pub intent_id: String,
        pub scenario_id: String,
        pub direction: PaymentDirection,
        pub protocol: String,
        pub asset: AssetIdentity,
        pub recipient: String,
        /// Payer identity observed by the trusted facilitator/provider or chain
        /// verifier, never accepted from an unverified merchant response.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub payer: Option<String>,
        pub requested_amount: Amount,
        pub settled_amount: Amount,
        pub status: PaymentStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tx_hash: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub settlement_ref: Option<String>,
        /// Hash of trusted provider, facilitator, or chain evidence.
        pub evidence_hash: String,
        pub confirmed_at: i64,
    }

    impl PaymentReceipt {
        /// Ensures a receipt cannot claim confirmation without evidence and a
        /// settlement reference.
        ///
        /// # Errors
        ///
        /// Returns the invalid field or invariant name.
        pub fn validate(&self) -> Result<(), &'static str> {
            if self.intent_id.trim().is_empty() {
                return Err("intent_id");
            }
            if self.scenario_id.trim().is_empty() {
                return Err("scenario_id");
            }
            if self.protocol.trim().is_empty() {
                return Err("protocol");
            }
            self.asset.validate()?;
            if self.requested_amount.is_zero() {
                return Err("requested_amount");
            }
            if self.payer.as_deref().is_some_and(str::is_empty) {
                return Err("payer");
            }
            if self.settled_amount > self.requested_amount {
                return Err("settled_amount");
            }
            if self.status == PaymentStatus::Settled {
                if self.settled_amount.is_zero() {
                    return Err("settled_amount");
                }
                if self.tx_hash.is_none() && self.settlement_ref.is_none() {
                    return Err("settlement_reference");
                }
                if self.evidence_hash.trim().is_empty() {
                    return Err("evidence_hash");
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/model/receipt_entity_test.rs"
        ));
    }
}
pub use receipt_projection::*;

mod validation {
    use crate::{Error, Result};
    use reqwest::Url;

    pub struct PaymentValidation;

    impl PaymentValidation {
        /// Validates bounded identifiers accepted by payment application commands.
        ///
        /// # Errors
        /// Returns an invalid-request error for an empty, oversized or unsafe identifier.
        pub fn validate_identifier(field: &str, value: &str) -> Result<()> {
            if value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
            {
                return Err(Error::InvalidRequest(format!("{field} is invalid")));
            }
            Ok(())
        }

        /// Returns a normalized HTTP(S) origin after rejecting URL forms that can
        /// confuse credential, policy, or audit boundaries.
        ///
        /// # Errors
        /// Returns an invalid-request error for an unsafe or non-HTTP resource URL.
        pub fn canonical_http_origin(resource: &str) -> Result<String> {
            let parsed = Url::parse(resource)
                .map_err(|_| Error::InvalidRequest("payment resource is not an absolute URL".to_owned()))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err(Error::InvalidRequest("payment resource URL is unsafe".to_owned()));
            }
            Ok(parsed.origin().ascii_serialization())
        }

        #[must_use]
        pub const fn utc_day_start(now: i64) -> i64 {
            now.saturating_sub(now.rem_euclid(86_400))
        }

        /// Validates a trusted chain/provider timestamp against bounded clock skew.
        /// Transaction identity, amount, recipient, and authorization-window checks
        /// remain the responsibility of the evidence adapter that produced it.
        /// Domain-separated AEAD context shared by signing and crash-recovery paths.
        #[must_use]
        pub fn outbound_credential_context(intent_id: &str) -> String {
            format!("payment:{intent_id}:outbound-credential")
        }
    }
    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/model/payment_validation_test.rs"
        ));
    }
}
pub use validation::PaymentValidation;

/// Protocol-owned request for one intent-bound wallet signature.
///
/// The core signer accepts this adapter-produced digest only; no transport or
/// business caller can supply an arbitrary digest. `payload` is an unsigned
/// protocol template and must not be logged.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolSigningRequest {
    pub digest: Vec<u8>,
    pub payload: Value,
}

mod http_projection {
    use super::{Deserialize, PaymentCredential, Serialize, Value};
    use crate::{Error, Result};
    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct OperationContextDto {
        pub scenario_id: String,
        pub run_id: String,
    }
    impl OperationContextDto {
        ///
        /// # Errors
        /// Returns an invalid-request error for an unsafe scenario or run identifier.
        pub fn validate(&self) -> Result<()> {
            for (name, value) in [
                ("scenario_id", self.scenario_id.as_str()),
                ("run_id", self.run_id.as_str()),
            ] {
                Self::validate_identifier(name, value)?;
            }
            Ok(())
        }
    }
    impl OperationContextDto {
        ///
        /// # Errors
        /// Returns an invalid-request error for an empty, oversized or unsafe identifier.
        pub fn validate_identifier(name: &str, value: &str) -> Result<()> {
            if value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
            {
                return Err(Error::InvalidRequest(format!("{name} is invalid")));
            }
            Ok(())
        }
    }
    #[derive(Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentCredentialDto {
        pub protocol: String,
        pub challenge_hash: String,
        pub credential_hash: String,
        pub payload: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub http: Option<HttpCredentialDto>,
    }
    impl PaymentCredentialDto {
        #[must_use]
        pub fn into_domain(self) -> PaymentCredential {
            PaymentCredential {
                protocol: self.protocol,
                challenge_hash: self.challenge_hash,
                credential_hash: self.credential_hash,
                payload: self.payload,
            }
        }
    }

    #[derive(Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct HttpCredentialDto {
        pub bound_origin: String,
        pub header_name: String,
        pub header_value: String,
    }
    impl std::fmt::Debug for PaymentCredentialDto {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PaymentCredentialDto")
                .field("protocol", &self.protocol)
                .field("challenge_hash", &"<redacted>")
                .field("credential_hash", &"<redacted>")
                .field("payload", &"<redacted>")
                .field("http", &self.http)
                .finish()
        }
    }

    impl std::fmt::Debug for HttpCredentialDto {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("HttpCredentialDto")
                .field("bound_origin", &self.bound_origin)
                .field("header_name", &self.header_name)
                .field("header_value", &"<redacted>")
                .finish()
        }
    }
}
pub use http_projection::*;
