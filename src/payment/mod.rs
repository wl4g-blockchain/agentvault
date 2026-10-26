//! Payment execution components. Domain handlers own row-facing application APIs;
//! these components own protocol, custody, settlement and recovery orchestration.

pub mod approval;
pub mod approval_grant;
pub mod authorization;
pub mod balance;
pub mod evidence;
pub mod facilitator;
pub mod outbox;
pub mod price;
pub mod provider;
pub(crate) mod provider_http;
pub mod receipt;
pub mod reconciliation;
mod reconciliation_posting;
pub mod recovery;
pub mod resource;
pub mod settlement;
pub mod signing;
pub(crate) mod validation;
pub mod wallet_import;

use crate::handler::ScenarioHandler;
pub(crate) use crate::model::scenario::UNRESTRICTED_PRINCIPAL;
pub use crate::model::{
    AccessContext, EvmAuthorizationObservation, EvmAuthorizationQuery, ReceiptObservation, SettlementOperation,
    SettlementProviderRequest, SettlementProviderResult, TransferQuery,
};
pub use crate::protocol::ProtocolRegistry;
pub use approval::{ApprovalPrincipalAuthorizer, ApprovalWorkflow, StoredApprovalPrincipalAuthorizer};
pub use approval_grant::{ApprovalGrantTokenService, JwtApprovalGrantTokenService};
pub use authorization::PaymentAuthorizer;
pub use balance::{
    EvmRpcBalanceSource, SolanaRpcBalanceSource, WalletBalanceRegistry, WalletBalanceSource, WalletBalanceSync,
    WalletBalanceSynchronizer,
};
pub use evidence::{MerchantEvidenceRecorder, OutboundEvidenceDependencies};
pub use facilitator::X402FacilitatorProvider;
pub use outbox::{PaymentEventDispatcher, PaymentEventDispatcherConfig, PaymentEventSink};
pub use price::PriceResolver;
pub use provider::{
    ExternalSettlementProvider, HttpSettlementProvider, SettlementProviderConfig, SettlementProviderRegistry,
};
pub use receipt::{
    EvmAuthorizationSource, EvmEvidenceSource, EvmRpcReceiptConfig, EvmRpcReceiptSource, ReceiptRegistry,
    ReceiptSource, SolanaRpcReceiptConfig, SolanaRpcReceiptSource,
};
pub use reconciliation::{PaymentReconciler, ReconciliationConfig};
pub use recovery::{
    PaymentEvidenceRouter, ProviderReconciliationRequest, ReconciliationEvidenceSource, VerifyOnlyProviderEvidence,
};
pub use resource::ResourceImporter;
pub(crate) use settlement::SettlementFacts;
pub use settlement::{SettlementCoordinator, SettlementDependencies};
pub use signing::{OutboundSigner, OutboundSigningDependencies};
pub use wallet_import::WalletImportService;

#[cfg(test)]
#[path = "../../tests/payment/mod_test.rs"]
mod tests;

pub mod api;
pub use api::{PaymentApi, PaymentApiDependencies};
