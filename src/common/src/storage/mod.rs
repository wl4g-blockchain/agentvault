//! Durable payment persistence ports and SQL adapters.
//!
//! `storage` owns database-agnostic contracts, while `postgres` and `sqlite`
//! own dialect-specific prepared SQL. Correctness-sensitive mutations remain
//! explicit aggregate transactions; no generic row-update escape hatch is
//! exposed to handlers.

pub mod base_postgres;
pub mod base_sqlite;
#[path = "storage.rs"]
mod port;

pub use port::*;

#[cfg(any(test, feature = "test-support"))]
#[path = "../../tests/storage/mod_test.rs"]
pub mod schema_tests;

pub mod approval;
pub use approval::{
    ApprovalConsumption, ApprovalIntentTransition, ApprovalReservationGuard, ApprovalWrite, IApprovalRepository,
};
pub mod approver;
pub use approver::IApproverRepository;
pub mod client;
pub use client::IClientRepository;
pub mod event;
pub use event::IEventRepository;
pub mod ledger;
pub use ledger::{ILedgerRepository, LedgerQuery, LedgerWrite};
pub mod price;
pub use price::IPriceRepository;
pub mod outbox;
pub use outbox::{IOutboxRepository, OutboxDeliveryClass, OutboxLeaseRequest, OutboxMessage, OutboxStatus};
pub mod payment;
pub use payment::{
    AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
    ChallengeIssuanceGuard, ChallengeWrite, IIntentRepository, OutboundEvidenceWrite, PaymentProgressWrite,
    PaymentQuery, ReconciliationClaim, ReconciliationLeaseRequest, SettlementWrite, VerificationWrite,
};
pub mod policy;
pub use policy::IPolicyRepository;
pub mod scenario;
pub use scenario::{IScenarioRepository, ScenarioConfiguration};
pub mod signing;
pub use signing::{
    ISigningRepository, IntentBoundPaymentSignatureRequest, OutboundSigningAuthorization, OutboundSigningIdentity,
    OutboundSigningRead, PersistedSignedPayment, SigningGuard, SigningWrite, WalletPaymentSignature,
    WalletPaymentSigner,
};
pub mod wallet;
pub use wallet::{
    EncryptedWalletKeyVersion, IWalletBalanceRepository, IWalletRepository, PaymentWallet, WalletBalanceRecord,
    WalletCreationConfig, WalletCustody, WalletKeyManager, WalletKeyStatus,
};

pub(crate) mod schema;

mod entity;
pub use entity::*;
mod query;
pub use query::*;

pub mod reconciliation;
pub mod reconciliation_posting;
pub mod resource;
