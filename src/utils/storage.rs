//! Shared typed SQL value codecs. No repository operations or domain workflows.
use crate::{
    model::{
        Amount, ApprovalStatus, LedgerEntryType, PaymentDirection, PaymentEventType, PaymentStatus, ReservationStatus,
    },
    storage::{EntityValue, OutboxStatus, WalletCustody, WalletKeyStatus},
    Error, Result,
};
use bigdecimal::BigDecimal;
use sqlx::{
    decode::Decode,
    encode::{Encode, IsNull},
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef},
    Postgres, Type, TypeInfo as _,
};
use std::str::FromStr as _;

pub(crate) struct PaymentStorageCodec;
impl PaymentStorageCodec {
    pub(crate) fn domain_error(error: Error) -> Error {
        match error {
            Error::StorageConflict(message) => Error::PaymentConflict(message),
            error => error,
        }
    }

    pub(crate) fn event_type(value: &str) -> Result<PaymentEventType> {
        match value {
            "challenge_created" => Ok(PaymentEventType::ChallengeCreated),
            "challenge_received" => Ok(PaymentEventType::ChallengeReceived),
            "sign_requested" => Ok(PaymentEventType::SignRequested),
            "signed" => Ok(PaymentEventType::Signed),
            "approval_requested" => Ok(PaymentEventType::ApprovalRequested),
            "approval_granted" => Ok(PaymentEventType::ApprovalGranted),
            "approval_rejected" => Ok(PaymentEventType::ApprovalRejected),
            "approval_consumed" => Ok(PaymentEventType::ApprovalConsumed),
            "credential_verified" => Ok(PaymentEventType::CredentialVerified),
            "submitted" => Ok(PaymentEventType::Submitted),
            "settlement_requested" => Ok(PaymentEventType::SettlementRequested),
            "settled" => Ok(PaymentEventType::Settled),
            "retried" => Ok(PaymentEventType::Retried),
            "failed" => Ok(PaymentEventType::Failed),
            "reconciled" => Ok(PaymentEventType::Reconciled),
            _ => Err(Self::invalid_enum("payment event type", value)),
        }
    }
    pub(crate) fn custody(value: &str) -> Result<WalletCustody> {
        match value {
            "local" => Ok(WalletCustody::Local),
            "external" => Ok(WalletCustody::External),
            _ => Err(Self::invalid_enum("wallet custody", value)),
        }
    }
    pub(crate) fn key_status(value: &str) -> Result<WalletKeyStatus> {
        match value {
            "active" => Ok(WalletKeyStatus::Active),
            "retired" => Ok(WalletKeyStatus::Retired),
            "compromised" => Ok(WalletKeyStatus::Compromised),
            _ => Err(Self::invalid_enum("wallet key status", value)),
        }
    }
    pub(crate) fn encode_postgres_amount(value: Amount) -> Result<PgAmount> {
        BigDecimal::from_str(&value.to_string())
            .map(PgAmount)
            .map_err(|_| Error::Storage("encode PostgreSQL amount failed".to_owned()))
    }

    pub(crate) fn decode_postgres_amount(value: &PgAmount, field: &str) -> Result<Amount> {
        Amount::from_str(&value.0.to_string())
            .map_err(|_| Error::Storage(format!("PostgreSQL {field} contains an invalid amount")))
    }

    pub(crate) fn direction(value: &str) -> Result<PaymentDirection> {
        match value {
            "inbound" => Ok(PaymentDirection::Inbound),
            "outbound" => Ok(PaymentDirection::Outbound),
            _ => Err(Self::invalid_enum("direction", value)),
        }
    }
    pub(crate) fn payment_status(value: &str) -> Result<PaymentStatus> {
        match value {
            "created" => Ok(PaymentStatus::Created),
            "pending_approval" => Ok(PaymentStatus::PendingApproval),
            "authorized" => Ok(PaymentStatus::Authorized),
            "signed" => Ok(PaymentStatus::Signed),
            "submitted" => Ok(PaymentStatus::Submitted),
            "pending_settlement" => Ok(PaymentStatus::PendingSettlement),
            "settled" => Ok(PaymentStatus::Settled),
            "failed" => Ok(PaymentStatus::Failed),
            "expired" => Ok(PaymentStatus::Expired),
            "unknown" => Ok(PaymentStatus::Unknown),
            _ => Err(Self::invalid_enum("payment status", value)),
        }
    }
    pub(crate) fn reservation_status(value: &str) -> Result<ReservationStatus> {
        match value {
            "unreserved" => Ok(ReservationStatus::Unreserved),
            "reserved" => Ok(ReservationStatus::Reserved),
            "consumed" => Ok(ReservationStatus::Consumed),
            "released" => Ok(ReservationStatus::Released),
            _ => Err(Self::invalid_enum("reservation status", value)),
        }
    }
    pub(crate) fn outbox_status(value: &str) -> Result<OutboxStatus> {
        match value {
            "pending" => Ok(OutboxStatus::Pending),
            "delivered" => Ok(OutboxStatus::Delivered),
            "dead_letter" => Ok(OutboxStatus::DeadLetter),
            _ => Err(Self::invalid_enum("outbox status", value)),
        }
    }
    pub(crate) fn approval_status(value: &str) -> Result<ApprovalStatus> {
        match value {
            "pending" => Ok(ApprovalStatus::Pending),
            "approved" => Ok(ApprovalStatus::Approved),
            "rejected" => Ok(ApprovalStatus::Rejected),
            "expired" => Ok(ApprovalStatus::Expired),
            "consumed" => Ok(ApprovalStatus::Consumed),
            _ => Err(Self::invalid_enum("approval status", value)),
        }
    }
    pub(crate) fn ledger_type(value: &str) -> Result<LedgerEntryType> {
        match value {
            "payment" => Ok(LedgerEntryType::Payment),
            "fee" => Ok(LedgerEntryType::Fee),
            "refund" => Ok(LedgerEntryType::Refund),
            "deposit" => Ok(LedgerEntryType::Deposit),
            "transfer" => Ok(LedgerEntryType::Transfer),
            "reversal" => Ok(LedgerEntryType::Reversal),
            _ => Err(Self::invalid_enum("ledger entry type", value)),
        }
    }
    pub(crate) fn decode_sqlite_amount(value: &str, field: &str) -> Result<Amount> {
        Amount::from_str(value).map_err(|_| Error::Storage(format!("SQLite {field} contains an invalid amount")))
    }

    fn invalid_enum(field: &str, value: &str) -> Error {
        Error::Storage(format!("stored {field} contains unsupported value {value}"))
    }
}
/// Exact codec for the `pay_uint128` domain. The `PostgreSQL` binary
/// representation is `NUMERIC`, but `SQLx` deliberately checks the domain OID;
/// this wrapper declares both identities compatible without using a float.
#[derive(Clone, Debug)]
pub(crate) struct PgAmount(BigDecimal);
impl Type<Postgres> for PgAmount {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("pay_uint128")
    }

    fn compatible(info: &PgTypeInfo) -> bool {
        info.name() == "pay_uint128" || BigDecimal::compatible(info)
    }
}
impl Encode<'_, Postgres> for PgAmount {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> std::result::Result<IsNull, BoxDynError> {
        self.0.encode_by_ref(buffer)
    }

    fn size_hint(&self) -> usize {
        self.0.size_hint()
    }
}
impl<'row> Decode<'row, Postgres> for PgAmount {
    fn decode(value: PgValueRef<'row>) -> std::result::Result<Self, BoxDynError> {
        BigDecimal::decode(value).map(Self)
    }
}
impl Type<Postgres> for Amount {
    fn type_info() -> PgTypeInfo {
        PgAmount::type_info()
    }
    fn compatible(info: &PgTypeInfo) -> bool {
        PgAmount::compatible(info)
    }
}
impl Encode<'_, Postgres> for Amount {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> std::result::Result<IsNull, BoxDynError> {
        PgAmount(BigDecimal::from(self.value())).encode_by_ref(buffer)
    }
}
impl Decode<'_, Postgres> for Amount {
    fn decode(value: PgValueRef<'_>) -> std::result::Result<Self, BoxDynError> {
        let value = PgAmount::decode(value)?;
        crate::utils::storage::PaymentStorageCodec::decode_postgres_amount(&value, "entity amount").map_err(Into::into)
    }
}
impl From<&Amount> for crate::storage::EntityValue {
    fn from(value: &Amount) -> Self {
        Self::Decimal(Some(bigdecimal::BigDecimal::from(value.value())))
    }
}
impl sqlx::Type<sqlx::Sqlite> for Amount {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(info: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Sqlite>>::compatible(info)
    }
}
impl<'query> sqlx::Encode<'query, sqlx::Sqlite> for Amount {
    fn encode_by_ref(
        &self,
        buffer: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'query>>,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<sqlx::Sqlite>>::encode(self.to_string(), buffer)
    }
}
impl sqlx::Decode<'_, sqlx::Sqlite> for Amount {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'_>) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<sqlx::Sqlite>>::decode(value)?;
        crate::utils::storage::PaymentStorageCodec::decode_sqlite_amount(&value, "entity amount").map_err(Into::into)
    }
}
macro_rules! postgres_text_entity {
    ($owner:ty; $($entity:ty => $decode:ident),* $(,)?) => {$ (
        impl sqlx::Type<sqlx::Postgres> for $entity {
            fn type_info() -> sqlx::postgres::PgTypeInfo { <String as sqlx::Type<sqlx::Postgres>>::type_info() }
            fn compatible(info: &sqlx::postgres::PgTypeInfo) -> bool { <String as sqlx::Type<sqlx::Postgres>>::compatible(info) }
        }
        impl sqlx::Decode<'_, sqlx::Postgres> for $entity {
            fn decode(value: sqlx::postgres::PgValueRef<'_>) -> std::result::Result<Self, sqlx::error::BoxDynError> {
                <$owner>::$decode(&<String as sqlx::Decode<sqlx::Postgres>>::decode(value)?).map_err(Into::into)
            }
        }
    )*};
}

macro_rules! sqlite_text_entity {
    ($owner:ty; $($entity:ty => $decode:ident),* $(,)?) => {$ (
        impl sqlx::Type<sqlx::Sqlite> for $entity {
            fn type_info() -> sqlx::sqlite::SqliteTypeInfo { <String as sqlx::Type<sqlx::Sqlite>>::type_info() }
            fn compatible(info: &sqlx::sqlite::SqliteTypeInfo) -> bool { <String as sqlx::Type<sqlx::Sqlite>>::compatible(info) }
        }
        impl sqlx::Decode<'_, sqlx::Sqlite> for $entity {
            fn decode(value: sqlx::sqlite::SqliteValueRef<'_>) -> std::result::Result<Self, sqlx::error::BoxDynError> {
                <$owner>::$decode(&<String as sqlx::Decode<sqlx::Sqlite>>::decode(value)?).map_err(Into::into)
            }
        }
    )*};
}

macro_rules! entity_text_values {
    ($($ty:ty),* $(,)?) => {$ (
        impl From<&$ty> for EntityValue { fn from(value: &$ty) -> Self { Self::from(value.as_str()) } }
    )*};
}
entity_text_values! { PaymentDirection, PaymentStatus, ReservationStatus,
PaymentEventType, LedgerEntryType }

postgres_text_entity! { PaymentStorageCodec; PaymentDirection => direction, PaymentStatus => payment_status, ReservationStatus => reservation_status, PaymentEventType => event_type, LedgerEntryType => ledger_type }
sqlite_text_entity! { PaymentStorageCodec; PaymentDirection => direction, PaymentStatus => payment_status, ReservationStatus => reservation_status, PaymentEventType => event_type, LedgerEntryType => ledger_type }

#[cfg(test)]
#[path = "../../tests/utils/storage_test.rs"]
mod tests;

pub(crate) trait AmountEncoding {
    type Value;
    fn encode_amount(value: Amount) -> Result<Self::Value>;
}
pub(crate) struct PostgresAmountCodec;
impl AmountEncoding for PostgresAmountCodec {
    type Value = PgAmount;
    fn encode_amount(value: Amount) -> Result<PgAmount> {
        PaymentStorageCodec::encode_postgres_amount(value)
    }
}
impl PostgresAmountCodec {
    pub(crate) fn encode_optional_amount(value: Option<Amount>) -> Result<Option<PgAmount>> {
        value.map(Self::encode_amount).transpose()
    }
    pub(crate) fn decode_amount(value: &PgAmount, field: &str) -> Result<Amount> {
        PaymentStorageCodec::decode_postgres_amount(value, field)
    }
    pub(crate) fn decode_optional_amount(value: Option<&PgAmount>, field: &str) -> Result<Option<Amount>> {
        value.map(|v| Self::decode_amount(v, field)).transpose()
    }
}
pub(crate) struct SqliteAmountCodec;
impl AmountEncoding for SqliteAmountCodec {
    type Value = String;
    fn encode_amount(value: Amount) -> Result<String> {
        Ok(value.to_string())
    }
}
impl SqliteAmountCodec {
    pub(crate) fn encode_optional_amount(value: Option<Amount>) -> Result<Option<String>> {
        value.map(Self::encode_amount).transpose()
    }
    pub(crate) fn decode_amount(value: &str, field: &str) -> Result<Amount> {
        PaymentStorageCodec::decode_sqlite_amount(value, field)
    }
    pub(crate) fn decode_optional_amount(value: Option<&String>, field: &str) -> Result<Option<Amount>> {
        value.map(|v| Self::decode_amount(v, field)).transpose()
    }
}
