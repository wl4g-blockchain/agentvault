//! Transport-independent application handlers.

mod approval;
mod balance;
mod evm;
mod evm_receipt;
mod offer;
mod outbound_evidence;
mod outbound_signing;
mod outbox;
mod payment;
mod payment_validation;
mod provider;
mod reconciliation;
mod resource;
mod scenario;
mod settlement;
mod wallet;
mod x402;

pub(crate) use crate::model::scenario::UNRESTRICTED_PRINCIPAL;
pub use crate::model::ImportReport;
pub use crate::model::{
    AccessContext, ApprovalDecisionResult, ApprovalExpirationRun, AuthorizePayment, ConfirmedEvmTransfer,
    ConfirmedSettlement, ConfirmedTransfer, CreateEncryptedWallet, EncryptedWalletPrivateKey,
    EvmAuthorizationObservation, EvmAuthorizationQuery, EvmReceiptObservation, EvmTransferQuery, IncomingVerification,
    IssuePaymentChallenge, PaymentEventDelivery, PaymentEventDispatchRun, ReceiptObservation, ReconciliationEvidence,
    ReconciliationRun, RecordOutboundPaymentResponse, RecordedOutboundEvidence, SettleIncomingPayment,
    SettledIncomingPayment, SettlementOperation, SettlementProviderRequest, SettlementProviderResult,
    SignAuthorizedPayment, SignedOutboundPayment, SubmitApprovalDecision, TransferQuery, VerifyIncomingPayment,
    WalletBalanceSnapshot, WalletBalanceSyncRun, WalletImportSession,
};
pub use crate::protocol::ProtocolRegistry;
pub use approval::{
    ApprovalExpirationJob, ApprovalGrantTokenService, ApprovalHandler, ApprovalPrincipalAuthorizer,
    JwtApprovalGrantTokenService, StoredApprovalPrincipalAuthorizer,
};
pub use balance::{
    EvmRpcBalanceSource, SolanaRpcBalanceSource, WalletBalanceRegistry, WalletBalanceSource, WalletBalanceSync,
    WalletBalanceSyncJob, WalletBalanceSynchronizer,
};
pub use evm_receipt::{
    EvmAuthorizationSource, EvmEvidenceSource, EvmReceiptRegistry, EvmReceiptSource, EvmRpcReceiptConfig,
    EvmRpcReceiptSource, ReceiptRegistry, ReceiptSource, SolanaRpcReceiptConfig, SolanaRpcReceiptSource,
};
pub use offer::OfferHandler;
pub use outbound_evidence::{OutboundEvidenceDependencies, OutboundEvidenceHandler};
pub use outbound_signing::{OutboundSigningDependencies, OutboundSigningHandler};
pub use outbox::{PaymentEventDispatcher, PaymentEventDispatcherConfig, PaymentEventSink};
pub use payment::PaymentHandler;
pub use reconciliation::{
    PaymentEvidenceRouter, ProviderReconciliationRequest, ReconciliationConfig, ReconciliationEvidenceSource,
    ReconciliationJob, UnixClock, VerifyOnlyProviderEvidence,
};
#[cfg(test)]
pub(crate) use resource::encode_test_resource_documents;
pub use resource::{create_wallets_from_paths, import_from_paths};
pub use scenario::{ClientHandler, ScenarioHandler};

pub(crate) use settlement::provider_verification_ref;
pub use settlement::{
    ExternalSettlementProvider, HttpSettlementProvider, SettlementDependencies, SettlementHandler,
    SettlementProviderConfig, SettlementProviderRegistry,
};
pub use wallet::WalletHandler;
pub use x402::X402FacilitatorProvider;

#[cfg(test)]
#[path = "../../tests/handler/mod_test.rs"]
mod tests;
