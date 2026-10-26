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
        ChallengeIssuanceGuard, ChallengeWrite, EventStorage, IdempotentWrite, OutboxMessage, OutboxStatus,
        PaymentProgressWrite, PaymentStorage, PaymentWallet, PolicyStorage, PriceStorage, SettlementWrite,
        VerificationWrite, WalletStorage,
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
    offers: Arc<dyn PriceStorage>,
    policies: Arc<dyn PolicyStorage>,
    wallets: Arc<dyn WalletStorage>,
    payments: Arc<dyn PaymentStorage>,
    events: Arc<dyn EventStorage>,
    protocols: Arc<ProtocolRegistry>,
    providers: Arc<SettlementProviderRegistry>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
    authorizations: Arc<dyn EvmAuthorizationSource>,
    reconciliation_delay_seconds: i64,
}

/// Collaborators used by the payee settlement application service.
pub struct SettlementDependencies {
    pub scenarios: Arc<ScenarioHandler>,
    pub offers: Arc<dyn PriceStorage>,
    pub policies: Arc<dyn PolicyStorage>,
    pub wallets: Arc<dyn WalletStorage>,
    pub payments: Arc<dyn PaymentStorage>,
    pub events: Arc<dyn EventStorage>,
    pub protocols: Arc<ProtocolRegistry>,
    pub providers: Arc<SettlementProviderRegistry>,
    pub material_protector: Arc<dyn SensitiveMaterialProtector>,
    pub authorizations: Arc<dyn EvmAuthorizationSource>,
}

struct PreparedInboundChallenge {
    access: AccessContext,
    offer: PaymentPrice,
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
            offers: dependencies.offers,
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
