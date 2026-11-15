//! Payment transaction implementation shared across providers.
macro_rules! payment_repository {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        use crate::model::Amount;

        use crate::model::ApprovalRequest;

        use crate::model::BudgetUnit;

        use crate::model::PaymentChallenge;

        use crate::model::PaymentCredential;

        use crate::model::PaymentDirection;

        use crate::model::PaymentEvent;

        use crate::model::PaymentIntent;

        use crate::model::PaymentStatus;

        use crate::model::ReservationStatus;

        use crate::model::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;

        use crate::storage::codec::AmountEncoding as _;

        use crate::storage::payment::PaymentStorageRules;

        use crate::storage::payment::ReservationMutation;

        use crate::storage::AuthorizationAction;

        use crate::storage::AuthorizationGuard;

        use crate::storage::AuthorizationRecord;

        use crate::storage::AuthorizationWrite;

        use crate::storage::BudgetSnapshot;

        use crate::storage::ChallengeIssuanceGuard;

        use crate::storage::ChallengeWrite;

        use crate::storage::IIntentRepository;

        use crate::storage::IdempotentWrite;

        use crate::storage::OutboundEvidenceWrite;

        use crate::storage::Page;

        use crate::storage::PaymentProgressWrite;

        use crate::storage::PaymentQuery;

        use crate::storage::ReconciliationClaim;

        use crate::storage::ReconciliationLeaseRequest;

        use crate::storage::SettlementWrite;

        use crate::storage::VerificationWrite;

        use crate::Error;

        use crate::Result;

        use async_trait::async_trait;

        use sqlx::Row as _;
        crate::storage::payment::r#macro::operations::payment_operations!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );
        crate::storage::payment::r#macro::rows::payment_rows!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        crate::storage::payment::payment_authority!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        crate::storage::policy::r#macro::payment_budget!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        crate::storage::payment::r#macro::payment_reconciliation!(
            $repository,
            $update,
            $share,
            $skip,
            $intent_lock,
            $client_lock,
            $policy_lock,
            $principal_lock
        );

        crate::storage::payment::r#macro::payment_mutations!(
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
pub(crate) use payment_repository;

#[cfg(any(test, feature = "test-support"))]
#[path = "../../../../tests/storage/payment/transactions_test.rs"]
mod transactions_tests;

// Intent aggregate write primitives; callable only within the authoritative storage transactions.
#[path = "macro/mutations.rs"]
mod mutations;
pub(crate) use mutations::payment_mutations;

#[cfg(test)]
#[path = "../../../../tests/storage/payment/mutations_test.rs"]
mod mutations_tests;

// Leased payment reconciliation storage, sharing the same intent transaction authority.
#[path = "macro/lease.rs"]
mod lease;
pub(crate) use lease::payment_reconciliation;

#[cfg(test)]
#[path = "../../../../tests/storage/payment/reconciliation_test.rs"]
mod reconciliation_tests;

#[path = "macro/operations.rs"]
pub(crate) mod operations;
#[path = "macro/rows.rs"]
pub(crate) mod rows;
