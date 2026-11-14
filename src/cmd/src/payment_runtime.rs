use super::{
    payment_adapters::PaymentAdapters, payment_adapters::RuntimeAdapterFactory,
    payment_jobs::ApprovalExpirationComponent, payment_jobs::CacheHealthComponent,
    payment_jobs::PaymentRuntimeResources, payment_jobs::ReconciliationComponent, payment_jobs::RuntimeJobFactory,
    payment_jobs::StorageReadinessProbe, payment_jobs::WalletBalanceSyncComponent,
};
use crate::{
    apm::AgentVaultMetrics,
    apm::ReadinessProbe,
    cache::CacheFactory,
    cache::ICache,
    storage::{
        IApprovalRepository, IApproverRepository, IIntentRepository, IPolicyRepository, IPriceRepository,
        IWalletRepository, StorageFactory, StoragePorts, WalletKeyManager, WalletPaymentSigner,
    },
    util::scenario::ScenarioHandler,
    util::secure_material::Aes256GcmMaterialProtector,
    util::secure_material::SensitiveMaterialProtector,
    AppConfig,
};
use agentvault_common::{
    storage::reconciliation::PaymentEvidenceRouter, storage::reconciliation::ReconciliationEvidenceSource,
};
use agentvault_payee::{
    handler::api::PayeeApi, handler::price::PriceResolver, handler::settlement::SettlementCoordinator,
    handler::settlement::SettlementDependencies,
};
use agentvault_payer::{
    handler::api::PayerApi, handler::api::PayerApiDependencies, handler::approval::ApprovalPrincipalAuthorizer,
    handler::approval::ApprovalWorkflow, handler::approval::StoredApprovalPrincipalAuthorizer,
    handler::approval_grant::JwtApprovalGrantTokenService, handler::authorization::PaymentAuthorizer,
    handler::evidence::MerchantEvidenceRecorder, handler::evidence::OutboundEvidenceDependencies,
    handler::signing::OutboundSigner, handler::signing::OutboundSigningDependencies,
};
use agentvault_protocol::ProtocolRegistry;
use anyhow::{anyhow, Context as _};
use axum::Router;
use std::sync::Arc;

pub(crate) struct AssembledPaymentRuntime {
    pub router: Router,
    pub cache_health: CacheHealthComponent,
    pub reconciliation: Option<ReconciliationComponent>,
    pub wallet_balance_sync: Option<WalletBalanceSyncComponent>,
    pub approval_expiration: Option<ApprovalExpirationComponent>,
    pub storage_readiness: Arc<dyn ReadinessProbe>,
    pub resources: PaymentRuntimeResources,
}
pub(crate) struct PaymentRuntimeFactory;
impl PaymentRuntimeFactory {
    pub(crate) async fn assemble(
        config: &AppConfig,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<AssembledPaymentRuntime> {
        let cache = CacheFactory::new()
            .create(&config.cache)
            .await
            .context("initialize non-authoritative cache")?;
        let storage = match StorageFactory::new().create(&config.storage).await {
            Ok(storage) => storage,
            Err(error) => {
                let _ = cache.close().await;
                return Err(error).context("initialize authoritative storage");
            }
        };
        let assembled = async {
            let ports = Arc::clone(&storage).ports();
            let scenarios = Arc::new(ScenarioHandler::new(
                Arc::clone(&ports.clients),
                Arc::clone(&ports.scenarios),
            ));
            let adapters = RuntimeAdapterFactory::build_adapters(config)?;
            let mut router = Router::new();
            let mut outbound: Option<Arc<dyn ReconciliationEvidenceSource>> = None;
            let mut incoming: Option<Arc<dyn ReconciliationEvidenceSource>> = None;
            if config.agentvault.payer.enabled {
                // Only the payer process opens wallet custody. The payee receives SQL public metadata.
                let wallet_keys = Arc::clone(&storage)
                    .wallet_key_manager(&config.agentvault.key_management)
                    .context("load payer wallet custody key ring")?;
                let (api, evidence) = Self::build_payer(
                    config,
                    &ports,
                    &scenarios,
                    &adapters,
                    &cache,
                    wallet_keys,
                    metrics.clone(),
                )?;
                router = router.merge(agentvault_payer::routes::PayerRoutes::router(api));
                outbound = Some(evidence);
            }
            if config.agentvault.payee.enabled {
                let api = Self::build_payee(config, &ports, scenarios, &adapters, metrics.clone())?;
                incoming = Some(api.settlement_handler.clone());
                router = router.merge(agentvault_payee::routes::PayeeRoutes::router(api));
            }
            let evidence: Arc<dyn ReconciliationEvidenceSource> =
                Arc::new(PaymentEvidenceRouter::with_roles(incoming, outbound));
            Ok(AssembledPaymentRuntime {
                router,
                cache_health: CacheHealthComponent {
                    cache: Arc::clone(&cache),
                    interval: std::time::Duration::from_secs(30),
                },
                reconciliation: RuntimeJobFactory::build_reconciliation(config, Arc::clone(&ports.payments), evidence)?,
                wallet_balance_sync: if config.agentvault.payer.enabled {
                    RuntimeJobFactory::build_wallet_balance_sync(
                        config,
                        Arc::clone(&ports.wallets),
                        Arc::clone(&ports.wallet_balances),
                        Arc::clone(&adapters.balances),
                        metrics,
                    )?
                } else {
                    None
                },
                approval_expiration: if config.agentvault.payer.enabled {
                    Some(RuntimeJobFactory::build_approval_expiration(
                        config,
                        Arc::clone(&ports.approvals),
                    )?)
                } else {
                    None
                },
                storage_readiness: Arc::new(StorageReadinessProbe {
                    storage: Arc::clone(&storage),
                }),
                resources: PaymentRuntimeResources {
                    cache: Arc::clone(&cache),
                    storage: Arc::clone(&storage),
                },
            })
        }
        .await;
        if assembled.is_err() {
            let _ = cache.close().await;
            let _ = storage.close().await;
        }
        assembled
    }
    fn build_payer(
        config: &AppConfig,
        ports: &StoragePorts,
        scenarios: &Arc<ScenarioHandler>,
        adapters: &PaymentAdapters,
        cache: &Arc<dyn ICache>,
        wallet_keys: Arc<dyn WalletKeyManager>,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<(Arc<PayerApi>, Arc<MerchantEvidenceRecorder>)> {
        let payments_handler = Self::build_payment_handler(
            config,
            Arc::clone(scenarios),
            Arc::clone(&ports.policies),
            Arc::clone(&ports.payments),
            Arc::clone(&adapters.payer_protocols),
            metrics.clone(),
        )?;
        let signer: Arc<dyn WalletPaymentSigner> = wallet_keys.clone();
        let signing_handler = Arc::new(OutboundSigner::new(OutboundSigningDependencies {
            storage: Arc::clone(&ports.signing),
            protocols: Arc::clone(&adapters.payer_protocols),
            balances: Arc::clone(&adapters.balances),
            signer,
            material_protector: Arc::clone(&adapters.material_protector),
        }));
        let outbound_evidence_handler = Arc::new(MerchantEvidenceRecorder::new(OutboundEvidenceDependencies {
            scenarios: Arc::clone(scenarios),
            payments: Arc::clone(&ports.payments),
            events: Arc::clone(&ports.events),
            protocols: Arc::clone(&adapters.payer_protocols),
            material_protector: Arc::clone(&adapters.material_protector),
            receipts: adapters.receipts.clone(),
        }));
        let (approval_authorizer, approval_handler) = Self::build_approval_services(
            config,
            Arc::clone(&ports.approvers),
            Arc::clone(&ports.approvals),
            Arc::clone(&ports.payments),
            Arc::clone(&adapters.material_protector),
        )?;
        let wallet_handler = adapters.build_wallet_handler(config, Arc::clone(cache), wallet_keys);
        let api = Self::build_payment_api(PayerApiDependencies {
            clients: Arc::clone(&ports.clients),
            scenarios: Arc::clone(scenarios),
            payments_handler,
            signing_handler,
            outbound_evidence_handler: Arc::clone(&outbound_evidence_handler),
            approval_handler,
            approval_authorizer,
            payer_protocols: Arc::clone(&adapters.payer_protocols),
            payments: Arc::clone(&ports.payments),
            approvals: Arc::clone(&ports.approvals),
            prices: Arc::clone(&ports.prices),
            policies: Arc::clone(&ports.policies),
            wallets: Arc::clone(&ports.wallets),
            wallet_handler,
            events: Arc::clone(&ports.events),
            ledger: Arc::clone(&ports.ledger),
            balances: Arc::clone(&adapters.balances),
            metrics,
        });
        Ok((api, outbound_evidence_handler))
    }
    fn build_payee(
        config: &AppConfig,
        ports: &StoragePorts,
        scenarios: Arc<ScenarioHandler>,
        adapters: &PaymentAdapters,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<Arc<PayeeApi>> {
        let settlement = Arc::new(SettlementCoordinator::new(
            SettlementDependencies {
                scenarios: Arc::clone(&scenarios),
                prices: Arc::clone(&ports.prices),
                policies: Arc::clone(&ports.policies),
                wallets: Arc::clone(&ports.wallets),
                payments: Arc::clone(&ports.payments),
                events: Arc::clone(&ports.events),
                protocols: Arc::clone(&adapters.payee_protocols),
                providers: Arc::clone(&adapters.providers),
                material_protector: Arc::clone(&adapters.material_protector),
                authorizations: adapters.receipts.clone(),
            },
            Self::seconds(
                config.agentvault.reconciliation.initial_backoff,
                "reconciliation initial backoff",
            )?,
        )?);
        let price_resolver = Self::build_price_resolver(
            &scenarios,
            &ports.prices,
            &ports.policies,
            &ports.wallets,
            &adapters.payee_protocols,
        );
        Ok(Arc::new(PayeeApi {
            scenarios,
            prices: Arc::clone(&ports.prices),
            settlement_handler: settlement,
            price_resolver,
            metrics,
        }))
    }
    pub(crate) fn build_payment_api(dependencies: PayerApiDependencies) -> Arc<PayerApi> {
        Arc::new(PayerApi::new(dependencies))
    }

    pub(crate) fn build_payment_handler(
        config: &AppConfig,
        scenarios: Arc<ScenarioHandler>,
        policies: Arc<dyn IPolicyRepository>,
        payments: Arc<dyn IIntentRepository>,
        protocols: Arc<ProtocolRegistry>,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<Arc<PaymentAuthorizer>> {
        Ok(Arc::new(
            PaymentAuthorizer::new(
                scenarios,
                policies,
                payments,
                protocols,
                PaymentRuntimeFactory::seconds(config.agentvault.approval.request_ttl, "approval request TTL")?,
                config.agentvault.payer.wallet_selection.strategy,
                metrics,
            )
            .context("build outbound payment handler")?,
        ))
    }

    pub(crate) fn build_price_resolver(
        scenarios: &Arc<ScenarioHandler>,
        prices: &Arc<dyn IPriceRepository>,
        policies: &Arc<dyn IPolicyRepository>,
        wallets: &Arc<dyn IWalletRepository>,
        protocols: &Arc<ProtocolRegistry>,
    ) -> Arc<PriceResolver> {
        Arc::new(PriceResolver::new(
            Arc::clone(scenarios),
            Arc::clone(prices),
            Arc::clone(policies),
            Arc::clone(wallets),
            Arc::clone(protocols),
        ))
    }

    pub(crate) fn build_approval_services(
        config: &AppConfig,
        approvers: Arc<dyn IApproverRepository>,
        approvals: Arc<dyn IApprovalRepository>,
        payments: Arc<dyn IIntentRepository>,
        material_protector: Arc<dyn SensitiveMaterialProtector>,
    ) -> anyhow::Result<(Arc<dyn ApprovalPrincipalAuthorizer>, Arc<ApprovalWorkflow>)> {
        let authorizer: Arc<dyn ApprovalPrincipalAuthorizer> =
            Arc::new(StoredApprovalPrincipalAuthorizer::new(approvers));
        let grant_tokens = Arc::new(
            JwtApprovalGrantTokenService::from_config(&config.agentvault.approval.jwt, material_protector)
                .context("build approval grant service")?,
        );
        let handler = ApprovalWorkflow::new(
            approvals,
            payments,
            Arc::clone(&authorizer),
            grant_tokens,
            PaymentRuntimeFactory::seconds(config.agentvault.approval.grant_ttl, "approval grant TTL")?,
        )
        .context("build approval handler")?;
        Ok((authorizer, Arc::new(handler)))
    }

    /// Builds the same approval state machine used by the HTTP API for the local
    /// administrative console. The console receives no wallet signer or custody
    /// key port through this boundary.
    pub(crate) fn console_approval_handler(
        config: &AppConfig,
        ports: &StoragePorts,
    ) -> anyhow::Result<Arc<ApprovalWorkflow>> {
        let material_protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
            Aes256GcmMaterialProtector::from_config(&config.agentvault.recovery_encryption)
                .context("load recovery-encryption key ring")?,
        );
        let (_, handler) = PaymentRuntimeFactory::build_approval_services(
            config,
            Arc::clone(&ports.approvers),
            Arc::clone(&ports.approvals),
            Arc::clone(&ports.payments),
            material_protector,
        )?;
        Ok(handler)
    }

    pub(crate) fn seconds(duration: std::time::Duration, field: &str) -> anyhow::Result<i64> {
        i64::try_from(duration.as_secs()).map_err(|_| anyhow!("{field} exceeds the supported timestamp range"))
    }
}

#[cfg(test)]
#[path = "../tests/payment_runtime_test.rs"]
mod tests;
