//! Table-aligned payment entities, value objects and aggregate commands.
//! HTTP service projections belong to payer/payee; recovery ports belong to storage.

pub use crate::modules::approval::model as approval;
pub use crate::modules::event::model as event;
pub use crate::modules::payment::model as payment;
pub use crate::modules::policy::model as policy;
pub use crate::modules::price::model as price;
pub use crate::modules::scenario::model as scenario;
pub use crate::modules::wallet::model as wallet;
pub use crate::modules::wallet_balance::model as wallet_balance;

pub use approval::ApprovalDecision;
pub use approval::ApprovalDecisionKind;
pub use approval::ApprovalDecisionResult;
pub use approval::ApprovalExpirationRun;
pub use approval::ApprovalGrant;
pub use approval::ApprovalGrantClaims;
pub use approval::ApprovalRequest;
pub use approval::ApprovalStatus;
pub use approval::SubmitApprovalDecision;
pub use approval::VerifiedApprovalGrant;
pub use approver::PaymentScenarioApprover;
pub use client::PaymentClient;
pub use event::PaymentEvent;
pub use event::PaymentEventDelivery;
pub use event::PaymentEventDispatchRun;
pub use event::PaymentEventType;
pub use ledger::LedgerEntryType;
pub use ledger::PaymentLedgerEntry;
pub use ledger::MAX_SETTLEMENT_EFFECTIVE_TIME_FUTURE_SKEW_SECONDS;
pub use payment::PaymentReceipt;
pub use price::PaymentPrice;
pub use scenario::PaymentScenario;
pub use wallet_balance::WalletBalanceSnapshot;
pub use wallet_balance::WalletBalanceSyncRun;

pub use payment::Amount;
pub use payment::AssetIdentity;
pub use payment::AuthorizePayment;
pub use payment::BudgetUnit;
pub use payment::ConfirmedTransfer;
pub use payment::EvmAuthorizationObservation;
pub use payment::EvmAuthorizationQuery;
pub use payment::OutboundEvidenceObservation;
pub use payment::PaymentAuthorizationResult;
pub use payment::PaymentChallenge;
pub use payment::PaymentCredential;
pub use payment::PaymentDirection;
pub use payment::PaymentIntent;
pub use payment::PaymentStatus;
pub use payment::ReceiptObservation;
pub use payment::RecordOutboundPaymentResponse;
pub use payment::RecordedOutboundEvidence;
pub use payment::ReplayCheck;
pub use payment::ReservationStatus;
pub use payment::SignAuthorizedPayment;
pub use payment::SignedOutboundPayment;
pub use payment::TransferQuery;
pub use policy::PaymentPolicy;
pub use policy::PaymentPolicyLimits;
pub use policy::PolicyDecision;
pub use policy::PolicyEngine;
pub use policy::PolicyEvaluation;
pub use policy::WalletBudget;
pub use scenario::AccessContext;
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
#[path = "../tests/model/mod_test.rs"]
mod tests;

pub use crate::modules::approver::model as approver;
pub use crate::modules::client::model as client;
pub use crate::modules::ledger::model as ledger;

pub use payment::ProtocolSigningRequest;

pub use payment::{HttpCredentialDto, OperationContextDto, PaymentCredentialDto};

pub use crate::modules::outbox::model as outbox;
pub use crate::modules::wallet_key::model as wallet_key;

#[cfg(test)]
#[path = "../tests/model/mod_entity_test.rs"]
mod entity_tests;
