//! Transport-neutral application DTOs, commands, results, and shared value
//! objects. Authoritative persistence entities live in [`domain`]; DTOs reuse
//! those entities when their shape is identical instead of copying fields.

pub mod approval;
pub mod balance;
pub mod domain;
pub mod event;
pub(crate) mod mqtt;
pub mod payment;
pub mod payment_api;
pub mod policy;
pub(crate) mod price;
pub mod receipt;
pub mod reconciliation;
pub mod resource;
pub mod scenario;
pub mod settlement;
pub mod wallet;

pub use approval::{
    ApprovalDecision, ApprovalDecisionKind, ApprovalDecisionResult, ApprovalExpirationRun, ApprovalGrant,
    ApprovalGrantClaims, ApprovalRequest, ApprovalStatus, PaymentScenarioApprover, SubmitApprovalDecision,
    VerifiedApprovalGrant,
};
pub use balance::{WalletBalanceSnapshot, WalletBalanceSyncRun};
pub(crate) use domain::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;
pub use domain::{
    LedgerEntryType, PaymentClient, PaymentEvent, PaymentEventType, PaymentLedgerEntry, PaymentPrice, PaymentReceipt,
    PaymentScenario,
};
pub use event::{PaymentEventDelivery, PaymentEventDispatchRun};

pub use payment::{
    Amount, AssetIdentity, AuthorizePayment, BudgetUnit, PaymentAuthorizationResult, PaymentChallenge,
    PaymentCredential, PaymentDirection, PaymentIntent, PaymentStatus, ReplayCheck, ReservationStatus,
    SignAuthorizedPayment, SignedOutboundPayment,
};
pub use policy::{PaymentPolicy, PaymentPolicyLimits, PolicyDecision, PolicyEngine, PolicyEvaluation, WalletBudget};
pub(crate) use receipt::OutboundEvidenceObservation;
pub use receipt::{
    ConfirmedTransfer, EvmAuthorizationObservation, EvmAuthorizationQuery, ReceiptObservation,
    RecordOutboundPaymentResponse, RecordedOutboundEvidence, TransferQuery,
};
pub(crate) use reconciliation::ReconciliationProviderObservation;
pub use reconciliation::{ConfirmedSettlement, ReconciliationEvidence, ReconciliationRun};
pub use resource::ImportReport;
pub use scenario::AccessContext;
pub use settlement::{
    IncomingVerification, IssuePaymentChallenge, SettleIncomingPayment, SettledIncomingPayment, SettlementOperation,
    SettlementProviderRequest, SettlementProviderResult, VerifyIncomingPayment,
};
pub use wallet::{
    canonicalize_payment_recipient, canonicalize_wallet_address, payment_recipients_match, wallet_addresses_match,
    CreateEncryptedWallet, EncryptedWalletPrivateKey, Wallet, WalletImportSession, EVM_EOA_ALGORITHM,
    SOLANA_ED25519_ALGORITHM,
};

#[cfg(test)]
#[path = "../../tests/model/mod_test.rs"]
mod tests;
