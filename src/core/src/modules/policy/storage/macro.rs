//! Shared policy SQL transactions; concrete providers supply only driver types and locks.

macro_rules! policy_repository {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        use crate::{
            model::BudgetUnit, model::PaymentPolicy, model::PaymentPolicyLimits, storage::codec::AmountEncoding as _,
            storage::policy::PolicyStorageRules, storage::IPolicyRepository, storage::Page, storage::PageRequest,
            Error, Result,
        };

        use async_trait::async_trait;

        use sqlx::Row as _;
        crate::storage::policy::r#macro::operations::policy_operations!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        struct EncodedLimits {
            max_per_payment: Option<AmountValue>,
            max_per_day: Option<AmountValue>,
            approval_threshold: Option<AmountValue>,
            approval_enabled: bool,
            approval_overage: Option<AmountValue>,
        }
        crate::storage::policy::r#macro::rows::policy_rows!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        struct EncodedPolicy {
            protocols: JsonValue,
            networks: JsonValue,
            assets: JsonValue,
            allowed_recipients: JsonValue,
            allowed_origins: JsonValue,
            inbound: EncodedLimits,
            outbound: EncodedLimits,
        }
        crate::storage::policy::r#macro::rows_1::policy_rows_1!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
        crate::storage::policy::r#macro::rows_2::policy_rows_2!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
    };
}
pub(crate) use policy_repository;

#[cfg(test)]
#[path = "../../../../tests/storage/policy/transactions_test.rs"]
mod tests;

// Payment budget shared across SQL providers.

#[path = "macro/budget.rs"]
mod budget;
pub(crate) use budget::payment_budget;

#[cfg(test)]
#[path = "../../../../tests/storage/payment/budget_test.rs"]
mod budget_tests;

#[path = "macro/operations.rs"]
pub(crate) mod operations;
#[path = "macro/rows.rs"]
pub(crate) mod rows;
#[path = "macro/rows_1.rs"]
pub(crate) mod rows_1;
#[path = "macro/rows_2.rs"]
pub(crate) mod rows_2;
