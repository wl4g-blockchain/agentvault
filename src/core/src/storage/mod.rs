//! Durable payment persistence ports and SQL adapters.
//
// `storage` owns database-agnostic contracts, while `postgres` and `sqlite`
// own dialect-specific prepared SQL. Correctness-sensitive mutations remain
// explicit aggregate transactions; no generic row-update escape hatch is
// exposed to handlers.

#[path = "storage.rs"]
mod port;
pub mod postgres;
pub mod sqlite;

pub use port::*;

#[cfg(any(test, feature = "test-support"))]
#[path = "../../tests/storage/mod_test.rs"]
pub mod schema_tests;

pub use crate::modules::approval::storage as approval;
pub use crate::modules::approver::storage as approver;
pub use crate::modules::client::storage as client;
pub use crate::modules::event::storage as event;
pub use crate::modules::ledger::storage as ledger;
pub use crate::modules::outbox::storage as outbox;
pub use crate::modules::payment::storage as payment;
pub use crate::modules::policy::storage as policy;
pub use crate::modules::price::storage as price;
pub use crate::modules::scenario::storage as scenario;
pub use crate::modules::signing::storage as signing;
pub use crate::modules::wallet::storage as wallet;
pub use approval::{
    ApprovalConsumption, ApprovalIntentTransition, ApprovalReservationGuard, ApprovalWrite, IApprovalRepository,
};
pub use approver::IApproverRepository;
pub use client::IClientRepository;
pub use event::IEventRepository;
pub use ledger::{ILedgerRepository, LedgerQuery, LedgerWrite};
pub use outbox::{IOutboxRepository, OutboxDeliveryClass, OutboxLeaseRequest, OutboxMessage, OutboxStatus};
pub use payment::{
    AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
    ChallengeIssuanceGuard, ChallengeWrite, IIntentRepository, OutboundEvidenceWrite, PaymentProgressWrite,
    PaymentQuery, ReconciliationClaim, ReconciliationLeaseRequest, SettlementWrite, VerificationWrite,
};
pub use policy::IPolicyRepository;
pub use price::IPriceRepository;
pub use scenario::{IScenarioRepository, ScenarioConfiguration};
pub use signing::{
    ISigningRepository, IntentBoundPaymentSignatureRequest, OutboundSigningAuthorization, OutboundSigningIdentity,
    OutboundSigningRead, PersistedSignedPayment, SigningGuard, SigningWrite, WalletPaymentSignature,
    WalletPaymentSigner,
};
pub use wallet::{
    EncryptedWalletKeyVersion, IWalletBalanceRepository, IWalletRepository, PaymentWallet, WalletBalanceRecord,
    WalletCreationConfig, WalletCustody, WalletKeyManager, WalletKeyStatus,
};

pub(crate) mod schema;

mod entity;
pub use entity::*;
mod query;
pub use query::*;

pub use crate::modules::payment::handler::reconciliation;
pub mod reconciliation_posting;

pub mod codec;

pub use reconciliation::{
    ConfirmedSettlement, ReconciliationEvidence, ReconciliationProviderObservation, ReconciliationRun,
};
