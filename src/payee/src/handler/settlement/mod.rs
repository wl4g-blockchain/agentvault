use super::{
    AccessContext, EvmAuthorizationObservation, EvmAuthorizationQuery, EvmAuthorizationSource, ProtocolRegistry,
    ScenarioHandler,
};
use crate::handler::provider::SettlementProviderRegistry;
use crate::model::wallet::is_evm_payment_network;
use crate::model::Amount;
use crate::model::LedgerEntryType;
use crate::model::PaymentChallenge;
use crate::model::PaymentCredential;
use crate::model::PaymentDirection;
use crate::model::PaymentEvent;
use crate::model::PaymentEventType;
use crate::model::PaymentIntent;
use crate::model::PaymentLedgerEntry;
use crate::model::PaymentPolicy;
use crate::model::PaymentPrice;
use crate::model::PaymentReceipt;
use crate::model::PaymentStatus;
use crate::model::PolicyDecision;
use crate::model::PolicyEngine;
use crate::model::PolicyEvaluation;
use crate::model::ReservationStatus;
use crate::model::WalletBudget;
use crate::storage::reconciliation::ReconciliationProviderObservation;
use crate::storage::reconciliation::SettlementOperation;
use crate::storage::reconciliation::SettlementProviderRequest;
use crate::storage::reconciliation::SettlementProviderResult;
use crate::storage::ChallengeIssuanceGuard;
use crate::storage::ChallengeWrite;
use crate::storage::IEventRepository;
use crate::storage::IIntentRepository;
use crate::storage::IPolicyRepository;
use crate::storage::IPriceRepository;
use crate::storage::IWalletRepository;
use crate::storage::IdempotentWrite;
use crate::storage::OutboxMessage;
use crate::storage::OutboxStatus;
use crate::storage::PaymentProgressWrite;
use crate::storage::PaymentWallet;
use crate::storage::SettlementWrite;
use crate::storage::VerificationWrite;
use crate::util::secure_material::SealedSensitiveMaterial;
use crate::util::secure_material::SensitiveMaterialProtector;
use crate::Error;
use crate::Result;
use agentvault_protocol::x402::X402Evm;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;
use zeroize::Zeroizing;

mod challenge;
mod commit;
mod facts;
mod recovery;
mod verification;
pub(crate) use facts::SettlementFacts;
/// Payee-side challenge, verification, settlement, and ledger orchestration.
/// All business identity is derived from the authenticated principal.
pub struct SettlementCoordinator {
    scenarios: Arc<ScenarioHandler>,
    prices: Arc<dyn IPriceRepository>,
    policies: Arc<dyn IPolicyRepository>,
    wallets: Arc<dyn IWalletRepository>,
    payments: Arc<dyn IIntentRepository>,
    events: Arc<dyn IEventRepository>,
    protocols: Arc<ProtocolRegistry>,
    providers: Arc<SettlementProviderRegistry>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
    authorizations: Arc<dyn EvmAuthorizationSource>,
    reconciliation_delay_seconds: i64,
}

/// Collaborators used by the payee settlement application service.
pub struct SettlementDependencies {
    pub scenarios: Arc<ScenarioHandler>,
    pub prices: Arc<dyn IPriceRepository>,
    pub policies: Arc<dyn IPolicyRepository>,
    pub wallets: Arc<dyn IWalletRepository>,
    pub payments: Arc<dyn IIntentRepository>,
    pub events: Arc<dyn IEventRepository>,
    pub protocols: Arc<ProtocolRegistry>,
    pub providers: Arc<SettlementProviderRegistry>,
    pub material_protector: Arc<dyn SensitiveMaterialProtector>,
    pub authorizations: Arc<dyn EvmAuthorizationSource>,
}

struct PreparedInboundChallenge {
    access: AccessContext,
    price: PaymentPrice,
    policy: PaymentPolicy,
    wallet: PaymentWallet,
    challenge: PaymentChallenge,
}

enum SettlementRequestRecord {
    Inserted(PaymentIntent),
    Replay(PaymentIntent),
}

impl SettlementCoordinator {
    /// Creates a payee settlement orchestrator.
    ///
    /// # Errors
    ///
    /// Returns an error when the reconciliation delay is not positive.
    pub fn new(dependencies: SettlementDependencies, reconciliation_delay_seconds: i64) -> Result<Self> {
        if reconciliation_delay_seconds <= 0 {
            return Err(Error::Config("reconciliation delay must be positive".to_owned()));
        }
        Ok(Self {
            scenarios: dependencies.scenarios,
            prices: dependencies.prices,
            policies: dependencies.policies,
            wallets: dependencies.wallets,
            payments: dependencies.payments,
            events: dependencies.events,
            protocols: dependencies.protocols,
            providers: dependencies.providers,
            material_protector: dependencies.material_protector,
            authorizations: dependencies.authorizations,
            reconciliation_delay_seconds,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/handler/settlement/mod_test.rs"]
pub(crate) mod tests;

mod command {
    use crate::model::{PaymentCredential, PaymentIntent, PaymentReceipt};
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct SettledIncomingPayment {
        pub intent: PaymentIntent,
        pub receipt: PaymentReceipt,
    }
    #[derive(Clone, Debug)]
    pub struct VerifyIncomingPayment {
        pub scenario_id: String,
        pub run_id: String,
        pub challenge_hash: String,
        pub request_hash: String,
        pub credential: PaymentCredential,
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct IncomingVerification {
        pub intent: PaymentIntent,
        pub valid: bool,
        pub verification_ref: Option<String>,
    }
    #[derive(Clone, Debug)]
    pub struct SettleIncomingPayment {
        pub scenario_id: String,
        pub run_id: String,
        pub intent_id: String,
        pub verification_ref: String,
    }
    #[derive(Clone, Debug)]
    pub struct IssuePaymentChallenge {
        pub scenario_id: String,
        pub run_id: String,
        pub price_id: String,
        pub resource: String,
        pub origin: String,
        pub protocol: String,
        pub request_hash: String,
    }
}
pub use command::*;
