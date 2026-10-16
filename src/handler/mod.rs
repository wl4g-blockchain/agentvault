//! Transport-independent application handlers.

mod approval_handler;
mod balance_handler;
mod client_handler;
mod common;
mod evm_receipt;
mod evm_rpc;
mod http_json_transport;
pub mod job;
mod offer_handler;
mod outbound_evidence;
mod outbound_signing_handler;
mod payment_handler;
mod policy_handler;
mod provisioning;
mod scenario_handler;
mod settlement_handler;
mod x402_facilitator;
mod x402_wire;

pub use approval_handler::{
    ApprovalDecisionResult, ApprovalGrantTokenService, ApprovalHandler, ApprovalPrincipalAuthorizer,
    JwtApprovalGrantTokenService, StoredApprovalPrincipalAuthorizer, SubmitApprovalDecision,
};
pub use balance_handler::{EvmRpcBalanceSource, WalletBalanceRegistry, WalletBalanceSnapshot, WalletBalanceSource};
pub use client_handler::ClientHandler;
pub use evm_receipt::{
    ConfirmedEvmTransfer, EvmAuthorizationObservation, EvmAuthorizationQuery, EvmAuthorizationSource,
    EvmEvidenceSource, EvmReceiptObservation, EvmReceiptRegistry, EvmReceiptSource, EvmRpcReceiptConfig,
    EvmRpcReceiptSource, EvmTransferQuery,
};
pub use offer_handler::{OfferHandler, ProtocolRegistry};
pub(crate) use outbound_evidence::OutboundEvidenceObservation;
pub use outbound_evidence::{
    OutboundEvidenceDependencies, OutboundEvidenceHandler, RecordOutboundPaymentResponse, RecordedOutboundEvidence,
};
pub use outbound_signing_handler::{
    OutboundSigningDependencies, OutboundSigningHandler, SignAuthorizedPayment, SignedOutboundPayment,
};
pub use payment_handler::{AuthorizePayment, PaymentHandler};
pub use policy_handler::{PolicyDecision, PolicyEngine, PolicyEvaluation, WalletBudget};
pub use provisioning::{provision_from_path, ProvisioningReport};
pub(crate) use scenario_handler::UNRESTRICTED_PRINCIPAL;
pub use scenario_handler::{AccessContext, RegisterScenario, ScenarioHandler};

pub(crate) use settlement_handler::{provider_verification_ref, ReconciliationProviderObservation};
pub use settlement_handler::{
    ExternalSettlementProvider, HttpSettlementProvider, IncomingVerification, IssuePaymentChallenge,
    SettleIncomingPayment, SettledIncomingPayment, SettlementDependencies, SettlementHandler, SettlementOperation,
    SettlementProviderConfig, SettlementProviderRegistry, SettlementProviderRequest, SettlementProviderResult,
    VerifyIncomingPayment,
};
pub use x402_facilitator::X402FacilitatorProvider;

#[cfg(test)]
#[path = "../../tests/handler/mod_test.rs"]
mod tests;
