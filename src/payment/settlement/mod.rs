use super::{
    AccessContext, EvmAuthorizationObservation, EvmAuthorizationQuery, EvmAuthorizationSource, ProtocolRegistry,
    ScenarioHandler,
};
use crate::payment::SettlementProviderRegistry;
use crate::{
    model::{
        wallet::is_evm_payment_network, Amount, IncomingVerification, IssuePaymentChallenge, LedgerEntryType,
        PaymentChallenge, PaymentCredential, PaymentDirection, PaymentEvent, PaymentEventType, PaymentIntent,
        PaymentLedgerEntry, PaymentPolicy, PaymentPrice, PaymentReceipt, PaymentStatus, PolicyDecision, PolicyEngine,
        PolicyEvaluation, ReconciliationProviderObservation, ReservationStatus, SettleIncomingPayment,
        SettledIncomingPayment, SettlementOperation, SettlementProviderRequest, SettlementProviderResult,
        VerifyIncomingPayment, WalletBudget,
    },
    protocol::x402::X402Evm,
    storage::{
        ChallengeIssuanceGuard, ChallengeWrite, IEventRepository, IIntentRepository, IPolicyRepository,
        IPriceRepository, IWalletRepository, IdempotentWrite, OutboxMessage, OutboxStatus, PaymentProgressWrite,
        PaymentWallet, SettlementWrite, VerificationWrite,
    },
    utils::secure_material::{SealedSensitiveMaterial, SensitiveMaterialProtector},
    Error, Result,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
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
#[path = "../../../tests/payment/settlement/mod_test.rs"]
pub(crate) mod tests;
