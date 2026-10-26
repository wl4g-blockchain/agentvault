use super::{
    payment_adapters::RuntimeAdapterFactory,
    payment_jobs::{
        ApprovalExpirationComponent, CacheHealthComponent, PaymentRuntimeResources, ReconciliationComponent,
        RuntimeJobFactory, StorageReadinessProbe, WalletBalanceSyncComponent,
    },
};
use crate::{
    apm::AgentVaultMetrics, apm::ReadinessProbe, cache::CacheFactory, cache::ICache, config::AppConfig,
    controller::api::ApiController, controller::principal_context::PrincipalContext, controller::GrpcController,
    controller::MqttController, handler::ScenarioHandler, payment::ApprovalPrincipalAuthorizer,
    payment::ApprovalWorkflow, payment::EvmReceiptRegistry, payment::JwtApprovalGrantTokenService,
    payment::MerchantEvidenceRecorder, payment::OutboundEvidenceDependencies, payment::OutboundSigner,
    payment::OutboundSigningDependencies, payment::PaymentApi, payment::PaymentApiDependencies,
    payment::PaymentAuthorizer, payment::PriceResolver, payment::ProtocolRegistry, payment::SettlementCoordinator,
    payment::SettlementDependencies, payment::SettlementProviderRegistry, payment::StoredApprovalPrincipalAuthorizer,
    payment::WalletBalanceSource, storage::ApprovalStorage, storage::ApproverStorage, storage::ClientStorage,
    storage::EventStorage, storage::LedgerStorage, storage::OutboxStorage, storage::PaymentStorage,
    storage::PolicyStorage, storage::PriceStorage, storage::ScenarioStorage, storage::SigningStorage,
    storage::StorageFactory, storage::StoragePorts, storage::WalletBalanceStorage, storage::WalletKeyManager,
    storage::WalletPaymentSigner, storage::WalletStorage, utils::secure_material::Aes256GcmMaterialProtector,
    utils::secure_material::SensitiveMaterialProtector,
};
use anyhow::{anyhow, Context as _};
use axum::Router;
use std::sync::Arc;

pub(crate) struct AssembledPaymentRuntime {
    pub router: Router,
    pub grpc: Option<GrpcController>,
    pub mqtt: Option<MqttController>,
    pub cache_health: CacheHealthComponent,
    pub reconciliation: Option<ReconciliationComponent>,
    pub wallet_balance_sync: Option<WalletBalanceSyncComponent>,
    pub approval_expiration: ApprovalExpirationComponent,
    pub storage_readiness: Arc<dyn ReadinessProbe>,
    pub resources: PaymentRuntimeResources,
}

pub(crate) struct PaymentPorts {
    pub(crate) clients: Arc<dyn ClientStorage>,
    pub(crate) scenarios: Arc<dyn ScenarioStorage>,
    pub(crate) approvers: Arc<dyn ApproverStorage>,
    pub(crate) policies: Arc<dyn PolicyStorage>,
    pub(crate) offers: Arc<dyn PriceStorage>,
    pub(crate) wallets: Arc<dyn WalletStorage>,
    pub(crate) wallet_balances: Arc<dyn WalletBalanceStorage>,
    pub(crate) payments: Arc<dyn PaymentStorage>,
    pub(crate) approvals: Arc<dyn ApprovalStorage>,
    pub(crate) events: Arc<dyn EventStorage>,
    pub(crate) ledger: Arc<dyn LedgerStorage>,
    pub(crate) outbox: Arc<dyn OutboxStorage>,
    pub(crate) signing: Arc<dyn SigningStorage>,
    pub(crate) wallet_signer: Arc<dyn WalletPaymentSigner>,
    pub(crate) wallet_keys: Arc<dyn WalletKeyManager>,
}

impl PaymentPorts {
    fn from_storage(
        ports: StoragePorts,
        wallet_keys: Arc<dyn WalletKeyManager>,
        wallet_signer: Arc<dyn WalletPaymentSigner>,
    ) -> Self {
        Self {
            clients: ports.clients,
            scenarios: ports.scenarios,
            approvers: ports.approvers,
            policies: ports.policies,
            offers: ports.offers,
            wallets: ports.wallets,
            wallet_balances: ports.wallet_balances,
            payments: ports.payments,
            approvals: ports.approvals,
            events: ports.events,
            ledger: ports.ledger,
            outbox: ports.outbox,
            signing: ports.signing,
            wallet_signer,
            wallet_keys,
        }
    }
}

pub(crate) struct BuiltPaymentApplication {
    pub(crate) api: Arc<PaymentApi>,
    pub(crate) reconciliation: Option<ReconciliationComponent>,
    pub(crate) wallet_balance_sync: Option<WalletBalanceSyncComponent>,
    pub(crate) approval_expiration: ApprovalExpirationComponent,
}

impl BuiltPaymentApplication {
    fn new(
        config: &AppConfig,
        dependencies: PaymentApiDependencies,
        wallet_balances: Arc<dyn WalletBalanceStorage>,
    ) -> anyhow::Result<Self> {
        let reconciliation = RuntimeJobFactory::build_reconciliation(
            config,
            Arc::clone(&dependencies.payments),
            Arc::clone(&dependencies.settlement_handler),
            Arc::clone(&dependencies.outbound_evidence_handler),
        )?;
        let wallet_balance_sync = RuntimeJobFactory::build_wallet_balance_sync(
            config,
            Arc::clone(&dependencies.wallets),
            wallet_balances,
            Arc::clone(&dependencies.balances),
            dependencies.metrics.clone(),
        )?;
        let approval_expiration =
            RuntimeJobFactory::build_approval_expiration(config, Arc::clone(&dependencies.approvals))?;
        Ok(Self {
            api: PaymentRuntimeFactory::build_payment_api(dependencies),
            reconciliation,
            wallet_balance_sync,
            approval_expiration,
        })
    }
}

pub(crate) struct ExecutionDependencies {
    pub(crate) scenarios: Arc<ScenarioHandler>,
    pub(crate) offers: Arc<dyn PriceStorage>,
    pub(crate) policies: Arc<dyn PolicyStorage>,
    pub(crate) wallets: Arc<dyn WalletStorage>,
    pub(crate) payments: Arc<dyn PaymentStorage>,
    pub(crate) events: Arc<dyn EventStorage>,
    pub(crate) signing: Arc<dyn SigningStorage>,
    pub(crate) payer_protocols: Arc<ProtocolRegistry>,
    pub(crate) payee_protocols: Arc<ProtocolRegistry>,
    pub(crate) providers: Arc<SettlementProviderRegistry>,
    pub(crate) balances: Arc<dyn WalletBalanceSource>,
    pub(crate) receipts: Arc<EvmReceiptRegistry>,
    pub(crate) material_protector: Arc<dyn SensitiveMaterialProtector>,
    pub(crate) wallet_signer: Arc<dyn WalletPaymentSigner>,
}

pub(crate) struct ExecutionHandlers {
    pub(crate) settlement: Arc<SettlementCoordinator>,
    pub(crate) signing: Arc<OutboundSigner>,
    pub(crate) outbound_evidence: Arc<MerchantEvidenceRecorder>,
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
            let wallet_keys = Arc::clone(&storage)
                .wallet_key_manager(&config.agentvault.key_management)
                .context("load wallet custody key ring")?;
            let wallet_signer: Arc<dyn WalletPaymentSigner> = wallet_keys.clone();
            let ports = PaymentPorts::from_storage(Arc::clone(&storage).ports(), wallet_keys, wallet_signer);
            let clients = Arc::clone(&ports.clients);
            let scenarios = Arc::clone(&ports.scenarios);
            let outbox = Arc::clone(&ports.outbox);
            let application =
                PaymentRuntimeFactory::build_application(config, ports, Arc::clone(&cache), metrics.clone())?;
            let api = application.api;
            let mqtt = config
                .agentvault
                .transport
                .mqtt
                .enabled
                .then(|| {
                    MqttController::new(
                        config.agentvault.transport.mqtt.clone(),
                        Arc::clone(&api),
                        clients,
                        scenarios,
                        outbox,
                        Arc::clone(&cache),
                    )
                })
                .transpose()
                .context("build MQTT payment controller")?;
            let router = ApiController::router(api);
            let grpc = config
                .agentvault
                .transport
                .grpc
                .enabled
                .then(|| {
                    PrincipalContext::from_config(&config.server.authguard).map(|principals| {
                        GrpcController::new(
                            config.agentvault.transport.grpc.clone(),
                            principals,
                            router.clone(),
                            metrics.clone(),
                        )
                    })
                })
                .transpose()
                .context("build gRPC payment controller")?;
            Ok(AssembledPaymentRuntime {
                router,
                grpc,
                mqtt,
                cache_health: CacheHealthComponent {
                    cache: Arc::clone(&cache),
                    interval: std::time::Duration::from_secs(30),
                },
                reconciliation: application.reconciliation,
                wallet_balance_sync: application.wallet_balance_sync,
                approval_expiration: application.approval_expiration,
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

    pub(crate) fn build_application(
        config: &AppConfig,
        ports: PaymentPorts,
        cache: Arc<dyn ICache>,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<BuiltPaymentApplication> {
        let PaymentPorts {
            clients,
            scenarios: scenarios_store,
            approvers,
            policies,
            offers,
            wallets,
            wallet_balances,
            payments,
            approvals,
            events,
            ledger,
            outbox: _,
            signing,
            wallet_signer,
            wallet_keys,
        } = ports;
        let adapters = RuntimeAdapterFactory::build_adapters(config)?;
        let wallet_handler = adapters.build_wallet_handler(config, cache, wallet_keys);
        let scenarios = Arc::new(ScenarioHandler::new(Arc::clone(&clients), Arc::clone(&scenarios_store)));
        let payments_handler = PaymentRuntimeFactory::build_payment_handler(
            config,
            Arc::clone(&scenarios),
            Arc::clone(&policies),
            Arc::clone(&payments),
            Arc::clone(&adapters.payer_protocols),
            metrics.clone(),
        )?;
        let (approval_authorizer, approval_handler) = PaymentRuntimeFactory::build_approval_services(
            config,
            approvers,
            Arc::clone(&approvals),
            Arc::clone(&payments),
            Arc::clone(&adapters.material_protector),
        )?;
        let offer_handler = PaymentRuntimeFactory::build_price_resolver(
            &scenarios,
            &offers,
            &policies,
            &wallets,
            &adapters.payee_protocols,
        );
        let handlers = PaymentRuntimeFactory::build_execution_handlers(
            config,
            ExecutionDependencies {
                scenarios: Arc::clone(&scenarios),
                offers: Arc::clone(&offers),
                policies: Arc::clone(&policies),
                wallets: Arc::clone(&wallets),
                payments: Arc::clone(&payments),
                events: Arc::clone(&events),
                signing,
                payer_protocols: Arc::clone(&adapters.payer_protocols),
                payee_protocols: adapters.payee_protocols,
                providers: adapters.providers,
                balances: Arc::clone(&adapters.balances),
                receipts: adapters.receipts,
                material_protector: adapters.material_protector,
                wallet_signer,
            },
        )?;
        let settlement_handler = handlers.settlement;
        let signing_handler = handlers.signing;
        let outbound_evidence_handler = handlers.outbound_evidence;
        BuiltPaymentApplication::new(
            config,
            PaymentApiDependencies {
                clients,

                scenarios,
                payments_handler,
                signing_handler,
                outbound_evidence_handler,
                settlement_handler,
                offer_handler,
                approval_handler,
                approval_authorizer,
                payer_protocols: adapters.payer_protocols,
                payments,
                approvals,
                offers,
                policies,
                wallets,
                wallet_handler,
                events,
                ledger,
                balances: adapters.balances,
                metrics,
            },
            wallet_balances,
        )
    }

    pub(crate) fn build_payment_api(dependencies: PaymentApiDependencies) -> Arc<PaymentApi> {
        Arc::new(PaymentApi::new(dependencies))
    }

    pub(crate) fn build_payment_handler(
        config: &AppConfig,
        scenarios: Arc<ScenarioHandler>,
        policies: Arc<dyn PolicyStorage>,
        payments: Arc<dyn PaymentStorage>,
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
        offers: &Arc<dyn PriceStorage>,
        policies: &Arc<dyn PolicyStorage>,
        wallets: &Arc<dyn WalletStorage>,
        protocols: &Arc<ProtocolRegistry>,
    ) -> Arc<PriceResolver> {
        Arc::new(PriceResolver::new(
            Arc::clone(scenarios),
            Arc::clone(offers),
            Arc::clone(policies),
            Arc::clone(wallets),
            Arc::clone(protocols),
        ))
    }

    pub(crate) fn build_execution_handlers(
        config: &AppConfig,
        dependencies: ExecutionDependencies,
    ) -> anyhow::Result<ExecutionHandlers> {
        let reconciliation_delay = PaymentRuntimeFactory::seconds(
            config.agentvault.reconciliation.initial_backoff,
            "reconciliation initial backoff",
        )?;
        let settlement = Arc::new(
            SettlementCoordinator::new(
                SettlementDependencies {
                    scenarios: Arc::clone(&dependencies.scenarios),
                    offers: dependencies.offers,
                    policies: dependencies.policies,
                    wallets: dependencies.wallets,
                    payments: Arc::clone(&dependencies.payments),
                    events: Arc::clone(&dependencies.events),
                    protocols: dependencies.payee_protocols,
                    providers: dependencies.providers,
                    material_protector: Arc::clone(&dependencies.material_protector),
                    authorizations: dependencies.receipts.clone(),
                },
                reconciliation_delay,
            )
            .context("build inbound settlement handler")?,
        );
        let signing = Arc::new(OutboundSigner::new(OutboundSigningDependencies {
            storage: dependencies.signing,
            protocols: Arc::clone(&dependencies.payer_protocols),
            balances: dependencies.balances,
            signer: dependencies.wallet_signer,
            material_protector: Arc::clone(&dependencies.material_protector),
        }));
        let outbound_evidence = Arc::new(MerchantEvidenceRecorder::new(OutboundEvidenceDependencies {
            scenarios: dependencies.scenarios,
            payments: dependencies.payments,
            events: dependencies.events,
            protocols: dependencies.payer_protocols,
            material_protector: dependencies.material_protector,
            receipts: dependencies.receipts,
        }));
        Ok(ExecutionHandlers {
            settlement,
            signing,
            outbound_evidence,
        })
    }

    pub(crate) fn build_approval_services(
        config: &AppConfig,
        approvers: Arc<dyn ApproverStorage>,
        approvals: Arc<dyn ApprovalStorage>,
        payments: Arc<dyn PaymentStorage>,
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
#[path = "../../tests/cmd/payment_runtime_test.rs"]
mod tests;
