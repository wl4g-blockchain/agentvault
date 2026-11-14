//! Transport-neutral application DTOs, commands, results, and shared value
//! objects. Authoritative persistence entities live in [`domain`]; DTOs reuse
//! those entities when their shape is identical instead of copying fields.

pub mod approval;
pub mod balance;
pub mod domain;
pub mod event;
pub mod payment;
pub mod payment_api;
pub mod policy;
pub mod price;
pub mod receipt;
pub mod reconciliation;
pub mod resource;
pub mod scenario;
pub mod settlement;
pub mod wallet;

pub use approval::ApprovalDecision;
pub use approval::ApprovalDecisionKind;
pub use approval::ApprovalDecisionResult;
pub use approval::ApprovalExpirationRun;
pub use approval::ApprovalGrant;
pub use approval::ApprovalGrantClaims;
pub use approval::ApprovalRequest;
pub use approval::ApprovalStatus;
pub use approval::PaymentScenarioApprover;
pub use approval::SubmitApprovalDecision;
pub use approval::VerifiedApprovalGrant;
pub use balance::WalletBalanceSnapshot;
pub use balance::WalletBalanceSyncRun;
pub use domain::LedgerEntryType;
pub use domain::PaymentClient;
pub use domain::PaymentEvent;
pub use domain::PaymentEventType;
pub use domain::PaymentLedgerEntry;
pub use domain::PaymentPrice;
pub use domain::PaymentReceipt;
pub use domain::PaymentScenario;
pub use domain::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;
pub use event::PaymentEventDelivery;
pub use event::PaymentEventDispatchRun;

pub use payment::Amount;
pub use payment::AssetIdentity;
pub use payment::AuthorizePayment;
pub use payment::BudgetUnit;
pub use payment::PaymentAuthorizationResult;
pub use payment::PaymentChallenge;
pub use payment::PaymentCredential;
pub use payment::PaymentDirection;
pub use payment::PaymentIntent;
pub use payment::PaymentStatus;
pub use payment::ReplayCheck;
pub use payment::ReservationStatus;
pub use payment::SignAuthorizedPayment;
pub use payment::SignedOutboundPayment;
pub use policy::PaymentPolicy;
pub use policy::PaymentPolicyLimits;
pub use policy::PolicyDecision;
pub use policy::PolicyEngine;
pub use policy::PolicyEvaluation;
pub use policy::WalletBudget;
pub use receipt::ConfirmedTransfer;
pub use receipt::EvmAuthorizationObservation;
pub use receipt::EvmAuthorizationQuery;
pub use receipt::OutboundEvidenceObservation;
pub use receipt::ReceiptObservation;
pub use receipt::RecordOutboundPaymentResponse;
pub use receipt::RecordedOutboundEvidence;
pub use receipt::TransferQuery;
pub use reconciliation::ConfirmedSettlement;
pub use reconciliation::ReconciliationEvidence;
pub use reconciliation::ReconciliationProviderObservation;
pub use reconciliation::ReconciliationRun;
pub use resource::ImportReport;
pub use scenario::AccessContext;
pub use settlement::IncomingVerification;
pub use settlement::IssuePaymentChallenge;
pub use settlement::SettleIncomingPayment;
pub use settlement::SettledIncomingPayment;
pub use settlement::SettlementOperation;
pub use settlement::SettlementProviderRequest;
pub use settlement::SettlementProviderResult;
pub use settlement::VerifyIncomingPayment;
pub use wallet::canonicalize_payment_recipient;
pub use wallet::canonicalize_wallet_address;
pub use wallet::payment_recipients_match;
pub use wallet::wallet_addresses_match;
pub use wallet::CreateEncryptedWallet;
pub use wallet::EncryptedWalletPrivateKey;
pub use wallet::Wallet;
pub use wallet::WalletImportSession;
pub use wallet::EVM_EOA_ALGORITHM;
pub use wallet::SOLANA_ED25519_ALGORITHM;

#[cfg(test)]
#[path = "../../tests/model/mod_test.rs"]
mod tests;

pub mod protocol;
