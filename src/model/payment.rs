use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::{MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS, MAX_SETTLEMENT_EFFECTIVE_TIME_PAST_SKEW_SECONDS};
use crate::Error;

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

/// Complete identity of a fungible asset on a particular network.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct AssetIdentity {
    pub network: String,
    pub asset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_contract: Option<String>,
    pub decimals: u8,
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

pub use super::domain::{PaymentIntent, PaymentStatus, ReservationStatus};

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
    pub offer_id: Option<String>,
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

/// Validates trusted chain/provider time using the same bounds as budget
/// aggregation.
pub(crate) fn validate_trusted_effective_time(
    created_at: i64,
    effective_at: i64,
    observed_at: i64,
) -> crate::Result<()> {
    let earliest = created_at.saturating_sub(MAX_SETTLEMENT_EFFECTIVE_TIME_PAST_SKEW_SECONDS);
    let latest = observed_at.saturating_add(MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS);
    if effective_at < 0 || effective_at < earliest || effective_at > latest {
        return Err(Error::Settlement(
            "trusted settlement effective time exceeds bounded clock skew".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/model/payment_test.rs"]
mod tests;
