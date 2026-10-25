//! `AgentVault` server lifecycle and production payment composition root.
//!
//! Domain handlers depend only on ports. This module is the single place that
//! resolves operator configuration into concrete SQL, protocol, provider,
//! signer, RPC, HTTP, and MQTT adapters.

mod payment_runtime {
    use std::{collections::BTreeMap, sync::Arc};

    use anyhow::{anyhow, Context as _};
    use async_trait::async_trait;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    use crate::{
        apm::AgentVaultMetrics,
        cache::{CacheFactory, ICache},
        config::{AppConfig, ChainFamily, ExternalProviderConfig, MppAssetConfig, SecretHeaderConfig, X402AssetConfig},
        controller::http::{payment_router, PaymentApi, PaymentApiDependencies, PrincipalContext},
        controller::{GrpcController, LocalController, MqttController, ReadinessProbe},
        handler::{
            ApprovalExpirationJob, ApprovalHandler, ApprovalPrincipalAuthorizer, EvmAuthorizationSource,
            EvmReceiptRegistry, EvmRpcBalanceSource, EvmRpcReceiptConfig, EvmRpcReceiptSource,
            JwtApprovalGrantTokenService, OfferHandler, OutboundEvidenceDependencies, OutboundEvidenceHandler,
            OutboundSigningDependencies, OutboundSigningHandler, PaymentEvidenceRouter, PaymentHandler,
            ProtocolRegistry, ReceiptSource, ReconciliationConfig as JobReconciliationConfig,
            ReconciliationEvidenceSource, ReconciliationJob, ScenarioHandler, SettlementDependencies,
            SettlementHandler, SettlementProviderConfig, SettlementProviderRegistry, SolanaRpcBalanceSource,
            SolanaRpcReceiptConfig, SolanaRpcReceiptSource, StoredApprovalPrincipalAuthorizer, WalletBalanceRegistry,
            WalletBalanceSource, WalletBalanceSyncJob, WalletBalanceSynchronizer, WalletHandler,
            X402FacilitatorProvider,
        },
        protocol::{
            mpp::{MppAdapter, MppAsset},
            x402::{X402Adapter, X402Asset, X402SolanaConfig},
            PaymentProtocolFactory,
        },
        storage::{
            ApprovalStorage, ApproverStorage, ClientStorage, EventStorage, IStorage, LedgerStorage, OfferStorage,
            OutboxStorage, PaymentStorage, PolicyStorage, ScenarioStorage, SigningStorage, StorageFactory,
            StoragePorts, WalletBalanceStorage, WalletKeyManager, WalletPaymentSigner, WalletStorage,
        },
        utils::secret::read_secret_ref,
        utils::secure_material::{Aes256GcmMaterialProtector, SensitiveMaterialProtector},
        Error,
    };

    use crate::model::AssetIdentity;

    use axum::Router;

    use super::{RuntimeComponent, RuntimeResource, Shutdown};

    const MAX_OPERATOR_SECRET_BYTES: usize = 16 * 1024;

    pub(super) struct AssembledPaymentRuntime {
        pub router: Router,
        pub grpc: Option<GrpcController>,
        pub local: Option<LocalController>,
        pub mqtt: Option<MqttController>,
        pub cache_health: CacheHealthComponent,
        pub reconciliation: Option<ReconciliationComponent>,
        pub wallet_balance_sync: Option<WalletBalanceSyncComponent>,
        pub approval_expiration: ApprovalExpirationComponent,
        pub storage_readiness: Arc<dyn ReadinessProbe>,
        pub resources: PaymentRuntimeResources,
    }

    pub(super) async fn assemble(
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
            let application = build_application(config, ports, Arc::clone(&cache), metrics.clone())?;
            let api = application.api;
            let local = config
                .agentvault
                .transport
                .local
                .enabled
                .then(|| LocalController::new(config.agentvault.transport.local.clone(), Arc::clone(&api)))
                .transpose()
                .context("build Unix-socket payment controller")?;
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
            let router = payment_router(api);
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
                local,
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

    struct StorageReadinessProbe {
        storage: Arc<dyn IStorage>,
    }

    #[async_trait]
    impl ReadinessProbe for StorageReadinessProbe {
        fn name(&self) -> &'static str {
            "authoritative SQL storage"
        }

        async fn check(&self) -> anyhow::Result<()> {
            self.storage.ping().await.context("ping authoritative SQL storage")
        }
    }

    pub(super) struct PaymentRuntimeResources {
        cache: Arc<dyn ICache>,
        storage: Arc<dyn IStorage>,
    }

    #[async_trait]
    impl RuntimeResource for PaymentRuntimeResources {
        fn name(&self) -> &'static str {
            "payment providers"
        }

        async fn close(self: Box<Self>) -> anyhow::Result<()> {
            let cache_result = self.cache.close().await.context("close cache provider");
            let storage_result = self.storage.close().await.context("close storage provider");
            cache_result.and(storage_result)
        }
    }

    /// Keeps the selected best-effort cache adapter live without making payment
    /// correctness depend on it. SQL remains authoritative if a later ping fails.
    pub(super) struct CacheHealthComponent {
        cache: Arc<dyn ICache>,
        interval: std::time::Duration,
    }

    #[async_trait]
    impl RuntimeComponent for CacheHealthComponent {
        fn name(&self) -> &'static str {
            "non-authoritative cache"
        }

        async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
            let mut interval = tokio::time::interval(self.interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        if let Err(error) = self.cache.ping().await {
                            tracing::warn!(%error, "cache is unavailable; continuing with authoritative SQL");
                        }
                    }
                }
            }
        }
    }

    pub(super) struct ApprovalExpirationComponent {
        job: ApprovalExpirationJob,
        interval: std::time::Duration,
    }

    #[async_trait]
    impl RuntimeComponent for ApprovalExpirationComponent {
        fn name(&self) -> &'static str {
            "payment approval expiration"
        }

        async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
            let mut interval = tokio::time::interval(self.interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        let run = self.job.run_once().await.context("run approval expiration batch")?;
                        tracing::debug!(expired = run.expired, "approval expiration batch completed");
                    }
                }
            }
        }
    }

    pub(super) struct ReconciliationComponent {
        job: ReconciliationJob,
        interval: std::time::Duration,
    }

    pub(super) struct WalletBalanceSyncComponent {
        job: WalletBalanceSyncJob,
        interval: std::time::Duration,
    }

    #[async_trait]
    impl RuntimeComponent for WalletBalanceSyncComponent {
        fn name(&self) -> &'static str {
            "managed-wallet balance synchronization"
        }

        async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
            let mut interval = tokio::time::interval(self.interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        let run = self.job.run_once().await.context("run managed-wallet balance synchronization")?;
                        tracing::info!(
                            event.name = "agentvault.wallet.balance_sync_completed",
                            scanned = run.scanned,
                            observed = run.observed,
                            skipped = run.skipped,
                            failed = run.failed,
                            "managed-wallet balance synchronization completed"
                        );
                    }
                }
            }
        }
    }

    #[async_trait]
    impl RuntimeComponent for ReconciliationComponent {
        fn name(&self) -> &'static str {
            "payment reconciliation"
        }

        async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
            let mut interval = tokio::time::interval(self.interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        let run = self.job.run_once().await.context("run payment reconciliation batch")?;
                        tracing::debug!(
                            claimed = run.claimed,
                            settled = run.settled,
                            verified = run.verified,
                            expired = run.expired,
                            pending = run.pending,
                            unknown = run.unknown,
                            unavailable = run.unavailable,
                            provider_errors = run.provider_errors,
                            skipped = run.skipped,
                            "payment reconciliation batch completed"
                        );
                    }
                }
            }
        }
    }

    struct PaymentPorts {
        clients: Arc<dyn ClientStorage>,
        scenarios: Arc<dyn ScenarioStorage>,
        approvers: Arc<dyn ApproverStorage>,
        policies: Arc<dyn PolicyStorage>,
        offers: Arc<dyn OfferStorage>,
        wallets: Arc<dyn WalletStorage>,
        wallet_balances: Arc<dyn WalletBalanceStorage>,
        payments: Arc<dyn PaymentStorage>,
        approvals: Arc<dyn ApprovalStorage>,
        events: Arc<dyn EventStorage>,
        ledger: Arc<dyn LedgerStorage>,
        outbox: Arc<dyn OutboxStorage>,
        signing: Arc<dyn SigningStorage>,
        wallet_signer: Arc<dyn WalletPaymentSigner>,
        wallet_keys: Arc<dyn WalletKeyManager>,
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

    struct BuiltPaymentApplication {
        api: Arc<PaymentApi>,
        reconciliation: Option<ReconciliationComponent>,
        wallet_balance_sync: Option<WalletBalanceSyncComponent>,
        approval_expiration: ApprovalExpirationComponent,
    }

    struct PaymentAdapters {
        material_protector: Arc<dyn SensitiveMaterialProtector>,
        payer_protocols: Arc<ProtocolRegistry>,
        payee_protocols: Arc<ProtocolRegistry>,
        providers: Arc<SettlementProviderRegistry>,
        balances: Arc<dyn WalletBalanceSource>,
        receipts: Arc<EvmReceiptRegistry>,
    }

    impl PaymentAdapters {
        fn build_wallet_handler(
            &self,
            config: &AppConfig,
            cache: Arc<dyn ICache>,
            wallet_keys: Arc<dyn WalletKeyManager>,
        ) -> Arc<WalletHandler> {
            Arc::new(WalletHandler::new(
                cache,
                wallet_keys,
                Arc::clone(&self.material_protector),
                config.agentvault.key_management.import_session_ttl,
            ))
        }
    }

    struct ExecutionDependencies {
        scenarios: Arc<ScenarioHandler>,
        offers: Arc<dyn OfferStorage>,
        policies: Arc<dyn PolicyStorage>,
        wallets: Arc<dyn WalletStorage>,
        payments: Arc<dyn PaymentStorage>,
        events: Arc<dyn EventStorage>,
        signing: Arc<dyn SigningStorage>,
        payer_protocols: Arc<ProtocolRegistry>,
        payee_protocols: Arc<ProtocolRegistry>,
        providers: Arc<SettlementProviderRegistry>,
        balances: Arc<dyn WalletBalanceSource>,
        receipts: Arc<EvmReceiptRegistry>,
        material_protector: Arc<dyn SensitiveMaterialProtector>,
        wallet_signer: Arc<dyn WalletPaymentSigner>,
    }

    struct ExecutionHandlers {
        settlement: Arc<SettlementHandler>,
        signing: Arc<OutboundSigningHandler>,
        outbound_evidence: Arc<OutboundEvidenceHandler>,
    }

    fn build_application(
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
        let adapters = build_adapters(config)?;
        let wallet_handler = adapters.build_wallet_handler(config, cache, wallet_keys);
        let scenarios = Arc::new(ScenarioHandler::new(Arc::clone(&clients), Arc::clone(&scenarios_store)));
        let payments_handler = build_payment_handler(
            config,
            Arc::clone(&scenarios),
            Arc::clone(&policies),
            Arc::clone(&payments),
            Arc::clone(&adapters.payer_protocols),
            metrics.clone(),
        )?;
        let (approval_authorizer, approval_handler) = build_approval_services(
            config,
            approvers,
            Arc::clone(&approvals),
            Arc::clone(&payments),
            Arc::clone(&adapters.material_protector),
        )?;
        let offer_handler = build_offer_handler(&scenarios, &offers, &policies, &wallets, &adapters.payee_protocols);
        let handlers = build_execution_handlers(
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
        let reconciliation = build_reconciliation(
            config,
            Arc::clone(&payments),
            settlement_handler.clone(),
            Arc::clone(&outbound_evidence_handler),
        )?;
        let wallet_balance_sync = build_wallet_balance_sync(
            config,
            Arc::clone(&wallets),
            wallet_balances,
            Arc::clone(&adapters.balances),
            metrics.clone(),
        )?;
        let approval_expiration = build_approval_expiration(config, Arc::clone(&approvals))?;
        Ok(BuiltPaymentApplication {
            api: build_payment_api(PaymentApiDependencies {
                clients,
                scenario_store: scenarios_store,
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
            }),
            reconciliation,
            wallet_balance_sync,
            approval_expiration,
        })
    }

    fn build_payment_api(dependencies: PaymentApiDependencies) -> Arc<PaymentApi> {
        Arc::new(PaymentApi::new(dependencies))
    }

    fn build_payment_handler(
        config: &AppConfig,
        scenarios: Arc<ScenarioHandler>,
        policies: Arc<dyn PolicyStorage>,
        payments: Arc<dyn PaymentStorage>,
        protocols: Arc<ProtocolRegistry>,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<Arc<PaymentHandler>> {
        Ok(Arc::new(
            PaymentHandler::new(
                scenarios,
                policies,
                payments,
                protocols,
                seconds(config.agentvault.approval.request_ttl, "approval request TTL")?,
                config.agentvault.payer.wallet_selection.strategy,
                metrics,
            )
            .context("build outbound payment handler")?,
        ))
    }

    fn build_offer_handler(
        scenarios: &Arc<ScenarioHandler>,
        offers: &Arc<dyn OfferStorage>,
        policies: &Arc<dyn PolicyStorage>,
        wallets: &Arc<dyn WalletStorage>,
        protocols: &Arc<ProtocolRegistry>,
    ) -> Arc<OfferHandler> {
        Arc::new(OfferHandler::new(
            Arc::clone(scenarios),
            Arc::clone(offers),
            Arc::clone(policies),
            Arc::clone(wallets),
            Arc::clone(protocols),
        ))
    }

    fn build_execution_handlers(
        config: &AppConfig,
        dependencies: ExecutionDependencies,
    ) -> anyhow::Result<ExecutionHandlers> {
        let reconciliation_delay = seconds(
            config.agentvault.reconciliation.initial_backoff,
            "reconciliation initial backoff",
        )?;
        let settlement = Arc::new(
            SettlementHandler::new(
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
        let signing = Arc::new(OutboundSigningHandler::new(OutboundSigningDependencies {
            storage: dependencies.signing,
            protocols: Arc::clone(&dependencies.payer_protocols),
            balances: dependencies.balances,
            signer: dependencies.wallet_signer,
            material_protector: Arc::clone(&dependencies.material_protector),
        }));
        let outbound_evidence = Arc::new(OutboundEvidenceHandler::new(OutboundEvidenceDependencies {
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

    fn build_adapters(config: &AppConfig) -> anyhow::Result<PaymentAdapters> {
        let material_protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
            Aes256GcmMaterialProtector::from_config(&config.agentvault.recovery_encryption)
                .context("load recovery-encryption key ring")?,
        );
        Ok(PaymentAdapters {
            material_protector,
            payer_protocols: Arc::new(build_payer_protocol_registry(config).context("build payer protocol registry")?),
            payee_protocols: Arc::new(build_payee_protocol_registry(config).context("build payee protocol registry")?),
            providers: Arc::new(build_provider_registry(config).context("build settlement provider registry")?),
            balances: Arc::new(build_balance_registry(config).context("build chain balance-source registry")?),
            receipts: Arc::new(build_receipt_registry(config).context("build chain receipt-source registry")?),
        })
    }

    fn build_approval_services(
        config: &AppConfig,
        approvers: Arc<dyn ApproverStorage>,
        approvals: Arc<dyn ApprovalStorage>,
        payments: Arc<dyn PaymentStorage>,
        material_protector: Arc<dyn SensitiveMaterialProtector>,
    ) -> anyhow::Result<(Arc<dyn ApprovalPrincipalAuthorizer>, Arc<ApprovalHandler>)> {
        let authorizer: Arc<dyn ApprovalPrincipalAuthorizer> =
            Arc::new(StoredApprovalPrincipalAuthorizer::new(approvers));
        let grant_tokens = Arc::new(
            JwtApprovalGrantTokenService::from_config(&config.agentvault.approval.jwt, material_protector)
                .context("build approval grant service")?,
        );
        let handler = ApprovalHandler::new(
            approvals,
            payments,
            Arc::clone(&authorizer),
            grant_tokens,
            seconds(config.agentvault.approval.grant_ttl, "approval grant TTL")?,
        )
        .context("build approval handler")?;
        Ok((authorizer, Arc::new(handler)))
    }

    /// Builds the same approval state machine used by the HTTP API for the local
    /// administrative console. The console receives no wallet signer or custody
    /// key port through this boundary.
    pub(super) fn console_approval_handler(
        config: &AppConfig,
        ports: &StoragePorts,
    ) -> anyhow::Result<Arc<ApprovalHandler>> {
        let material_protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
            Aes256GcmMaterialProtector::from_config(&config.agentvault.recovery_encryption)
                .context("load recovery-encryption key ring")?,
        );
        let (_, handler) = build_approval_services(
            config,
            Arc::clone(&ports.approvers),
            Arc::clone(&ports.approvals),
            Arc::clone(&ports.payments),
            material_protector,
        )?;
        Ok(handler)
    }

    fn build_reconciliation(
        config: &AppConfig,
        payments: Arc<dyn PaymentStorage>,
        settlement: Arc<SettlementHandler>,
        outbound_evidence: Arc<OutboundEvidenceHandler>,
    ) -> anyhow::Result<Option<ReconciliationComponent>> {
        let settings = &config.agentvault.reconciliation;
        if !settings.enabled {
            return Ok(None);
        }
        let provider: Arc<dyn ReconciliationEvidenceSource> = settlement;
        let evidence: Arc<dyn ReconciliationEvidenceSource> =
            Arc::new(PaymentEvidenceRouter::new(provider, outbound_evidence));
        Ok(Some(ReconciliationComponent {
            job: ReconciliationJob::new(
                payments,
                evidence,
                JobReconciliationConfig {
                    worker_id: reconciliation_worker_id(&config.server.service_name),
                    batch_size: u32::try_from(settings.batch_size).context("reconciliation batch size exceeds u32")?,
                    lease_duration: settings.lease_ttl,
                    initial_backoff: settings.initial_backoff,
                    max_backoff: settings.max_backoff,
                },
            )
            .context("build reconciliation job")?,
            interval: settings.interval,
        }))
    }

    fn build_wallet_balance_sync(
        config: &AppConfig,
        wallets: Arc<dyn WalletStorage>,
        wallet_balances: Arc<dyn WalletBalanceStorage>,
        sources: Arc<dyn WalletBalanceSource>,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<Option<WalletBalanceSyncComponent>> {
        let settings = &config.agentvault.wallet_balance_sync;
        if !settings.enabled {
            return Ok(None);
        }
        let synchronizer = Arc::new(
            WalletBalanceSynchronizer::new(
                wallets,
                wallet_balances,
                sources,
                payer_balance_assets(config),
                metrics,
                u32::try_from(settings.batch_size).context("wallet balance sync batch size exceeds u32")?,
            )
            .context("build managed-wallet balance synchronizer")?,
        );
        Ok(Some(WalletBalanceSyncComponent {
            job: WalletBalanceSyncJob::new(synchronizer),
            interval: settings.interval,
        }))
    }

    fn payer_balance_assets(config: &AppConfig) -> Vec<AssetIdentity> {
        let mut assets = Vec::new();
        if config.agentvault.payer.x402.enabled {
            assets.extend(config.agentvault.payer.x402.assets.iter().map(|asset| AssetIdentity {
                network: asset.network.clone(),
                asset: asset.symbol.clone(),
                token_contract: Some(asset.contract.clone()),
                decimals: asset.decimals,
            }));
        }
        if config.agentvault.payer.mpp.enabled {
            assets.extend(config.agentvault.payer.mpp.assets.iter().map(|asset| AssetIdentity {
                network: asset.network.clone(),
                asset: asset.symbol.clone(),
                token_contract: Some(asset.currency.clone()),
                decimals: asset.decimals,
            }));
        }
        assets
    }

    pub(super) fn reconciliation_worker_id(service_name: &str) -> String {
        crate::utils::runtime_identity::unique_runtime_id(&format!("{service_name}-reconciliation"))
    }

    fn build_approval_expiration(
        config: &AppConfig,
        approvals: Arc<dyn ApprovalStorage>,
    ) -> anyhow::Result<ApprovalExpirationComponent> {
        let settings = &config.agentvault.approval;
        Ok(ApprovalExpirationComponent {
            job: ApprovalExpirationJob::new(
                approvals,
                u32::try_from(settings.expiry_batch_size).context("approval expiry batch size exceeds u32")?,
            )
            .context("build approval expiration job")?,
            interval: settings.expiry_interval,
        })
    }

    fn build_payer_protocol_registry(config: &AppConfig) -> crate::Result<ProtocolRegistry> {
        let mut factory = PaymentProtocolFactory::new();
        let x402 = &config.agentvault.payer.x402;
        if x402.enabled {
            factory = factory.register(Arc::new(x402_adapter(
                config,
                &x402.assets,
                x402.max_challenge_ttl.as_secs(),
            )?));
        }
        let mpp = &config.agentvault.payer.mpp;
        if mpp.enabled {
            factory = factory.register(Arc::new(MppAdapter::new_payer(
                mpp_assets(&mpp.assets),
                mpp.max_challenge_ttl.as_secs(),
            )?));
        }
        factory.build()
    }

    fn build_payee_protocol_registry(config: &AppConfig) -> crate::Result<ProtocolRegistry> {
        let mut factory = PaymentProtocolFactory::new();
        let x402 = &config.agentvault.payee.x402;
        if x402.enabled {
            factory = factory.register(Arc::new(x402_adapter(
                config,
                &x402.assets,
                x402.max_challenge_ttl.as_secs(),
            )?));
        }
        let mpp = &config.agentvault.payee.mpp;
        if mpp.enabled {
            let secret = read_secret_ref(&mpp.challenge_secret, "MPP challenge secret", MAX_OPERATOR_SECRET_BYTES)?;
            factory = factory.register(Arc::new(MppAdapter::new_payee(
                mpp_assets(&mpp.assets),
                mpp.realm.clone(),
                secret.to_string(),
                mpp.max_challenge_ttl.as_secs(),
            )?));
        }
        factory.build()
    }

    fn build_provider_registry(config: &AppConfig) -> crate::Result<SettlementProviderRegistry> {
        let mut bindings = Vec::new();
        let x402 = &config.agentvault.payee.x402;
        if x402.enabled {
            let settings = &x402.facilitator;
            let provider_id = settings.id.as_str();
            let headers = secret_headers(&settings.secret_headers, &format!("settlement provider {provider_id}"))?;
            let provider: Arc<dyn crate::handler::ExternalSettlementProvider> = Arc::new(X402FacilitatorProvider::new(
                &settlement_provider_config(settings),
                headers,
                x402_assets(&x402.assets),
            )?);
            bindings.push((
                "x402".to_owned(),
                provider_id.to_owned(),
                confirmation_depths(config, x402.assets.iter().map(|asset| asset.network.as_str()))?,
                provider,
            ));
        }
        let mpp = &config.agentvault.payee.mpp;
        if mpp.enabled {
            let settings = &mpp.provider;
            let provider_id = settings.id.as_str();
            let headers = secret_headers(&settings.secret_headers, &format!("settlement provider {provider_id}"))?;
            let provider: Arc<dyn crate::handler::ExternalSettlementProvider> = Arc::new(
                crate::handler::HttpSettlementProvider::new(&settlement_provider_config(settings), headers)?,
            );
            bindings.push((
                "mpp".to_owned(),
                provider_id.to_owned(),
                confirmation_depths(config, mpp.assets.iter().map(|asset| asset.network.as_str()))?,
                provider,
            ));
        }
        SettlementProviderRegistry::new(bindings)
    }

    fn confirmation_depths<'a>(
        config: &AppConfig,
        networks: impl IntoIterator<Item = &'a str>,
    ) -> crate::Result<BTreeMap<String, u64>> {
        networks
            .into_iter()
            .map(|network| {
                config
                    .agentvault
                    .chains
                    .get(network)
                    .map(|chain| (network.to_owned(), chain.confirmation_depth))
                    .ok_or_else(|| Error::Config(format!("no trusted chain configured for {network}")))
            })
            .collect()
    }

    fn settlement_provider_config(provider: &ExternalProviderConfig) -> SettlementProviderConfig {
        SettlementProviderConfig {
            base_url: provider.base_url.clone(),
            verify_path: provider.verify_path.clone(),
            settle_path: provider.settle_path.clone(),
            timeout: provider.timeout,
            max_response_bytes: provider.max_response_bytes,
            allow_insecure_loopback: provider.allow_insecure_loopback,
        }
    }

    fn x402_assets(assets: &[X402AssetConfig]) -> Vec<X402Asset> {
        assets
            .iter()
            .map(|asset| X402Asset {
                network: asset.network.clone(),
                contract: asset.contract.clone(),
                symbol: asset.symbol.clone(),
                decimals: asset.decimals,
            })
            .collect()
    }

    fn x402_adapter(
        config: &AppConfig,
        assets: &[X402AssetConfig],
        max_timeout_seconds: u64,
    ) -> crate::Result<X402Adapter> {
        let mut solana = Vec::new();
        for asset in assets {
            let Some(chain) = config.agentvault.chains.get(&asset.network) else {
                return Err(Error::Config(format!(
                    "no fixed chain RPC configured for {}",
                    asset.network
                )));
            };
            if chain.family != ChainFamily::Solana {
                continue;
            }
            let fee_payer = asset
                .fee_payer
                .clone()
                .ok_or_else(|| Error::Config(format!("x402 Solana asset {} has no fee payer", asset.network)))?;
            solana.push(X402SolanaConfig {
                network: asset.network.clone(),
                fee_payer,
                rpc_url: chain.rpc.endpoint.clone(),
                rpc_timeout: chain.rpc.timeout,
                rpc_max_response_bytes: chain.rpc.max_response_bytes,
                rpc_headers: resolved_header_values(
                    &chain.rpc.secret_headers,
                    &format!("Solana RPC {}", asset.network),
                )?,
                allow_insecure_loopback: chain.rpc.allow_insecure_loopback,
            });
        }
        X402Adapter::new_with_solana(x402_assets(assets), max_timeout_seconds, solana)
    }

    fn mpp_assets(assets: &[MppAssetConfig]) -> Vec<MppAsset> {
        assets
            .iter()
            .map(|asset| MppAsset {
                network: asset.network.clone(),
                currency: asset.currency.clone(),
                symbol: asset.symbol.clone(),
                token_contract: Some(asset.currency.clone()),
                decimals: asset.decimals,
            })
            .collect()
    }

    fn build_balance_registry(config: &AppConfig) -> crate::Result<WalletBalanceRegistry> {
        let mut sources = Vec::new();
        for (network, chain) in &config.agentvault.chains {
            let rpc = &chain.rpc;
            let headers = resolved_header_values(&rpc.secret_headers, &format!("chain RPC {network}"))?;
            let source: Arc<dyn WalletBalanceSource> = match chain.family {
                ChainFamily::Evm => Arc::new(EvmRpcBalanceSource::new(
                    &rpc.endpoint,
                    headers,
                    rpc.timeout,
                    rpc.max_response_bytes,
                    chain.minimum_native_balance,
                    rpc.allow_insecure_loopback,
                )?),
                ChainFamily::Solana => Arc::new(SolanaRpcBalanceSource::new(
                    network,
                    &rpc.endpoint,
                    &headers,
                    rpc.timeout,
                    rpc.max_response_bytes,
                    chain.minimum_native_balance,
                    rpc.allow_insecure_loopback,
                )?),
            };
            sources.push((network.clone(), source));
        }
        WalletBalanceRegistry::new(sources)
    }

    fn build_receipt_registry(config: &AppConfig) -> crate::Result<EvmReceiptRegistry> {
        let mut sources: Vec<(String, Arc<dyn ReceiptSource>)> = Vec::new();
        let mut authorizations: Vec<(String, Arc<dyn EvmAuthorizationSource>)> = Vec::new();
        let mut configured = std::collections::BTreeSet::new();
        let outbound_x402 = config
            .agentvault
            .payer
            .x402
            .enabled
            .then_some(config.agentvault.payer.x402.assets.as_slice())
            .into_iter()
            .flatten()
            .map(|asset| asset.network.as_str());
        let outbound_mpp = config
            .agentvault
            .payer
            .mpp
            .enabled
            .then_some(config.agentvault.payer.mpp.assets.as_slice())
            .into_iter()
            .flatten()
            .map(|asset| asset.network.as_str());
        let inbound_x402 = config
            .agentvault
            .payee
            .x402
            .enabled
            .then_some(config.agentvault.payee.x402.assets.as_slice())
            .into_iter()
            .flatten()
            .map(|asset| asset.network.as_str());
        let inbound_mpp = config
            .agentvault
            .payee
            .mpp
            .enabled
            .then_some(config.agentvault.payee.mpp.assets.as_slice())
            .into_iter()
            .flatten()
            .map(|asset| asset.network.as_str());
        for network in outbound_x402.chain(outbound_mpp).chain(inbound_x402).chain(inbound_mpp) {
            if !configured.insert(network.to_owned()) {
                continue;
            }
            let chain = config
                .agentvault
                .chains
                .get(network)
                .ok_or_else(|| Error::Config(format!("no fixed chain RPC configured for {network}")))?;
            let rpc = &chain.rpc;
            let headers = resolved_header_values(&rpc.secret_headers, &format!("chain RPC {network}"))?;
            match chain.family {
                ChainFamily::Evm => {
                    let source = Arc::new(EvmRpcReceiptSource::new(
                        network,
                        EvmRpcReceiptConfig {
                            endpoint: rpc.endpoint.clone(),
                            secret_headers: headers,
                            timeout: rpc.timeout,
                            max_response_bytes: rpc.max_response_bytes,
                            minimum_confirmations: chain.confirmation_depth,
                            allow_insecure_loopback: rpc.allow_insecure_loopback,
                        },
                    )?);
                    let receipt: Arc<dyn ReceiptSource> = source.clone();
                    let authorization: Arc<dyn EvmAuthorizationSource> = source;
                    sources.push((network.to_owned(), receipt));
                    authorizations.push((network.to_owned(), authorization));
                }
                ChainFamily::Solana => {
                    let source: Arc<dyn ReceiptSource> = Arc::new(SolanaRpcReceiptSource::new(
                        network,
                        &SolanaRpcReceiptConfig {
                            endpoint: rpc.endpoint.clone(),
                            secret_headers: headers,
                            timeout: rpc.timeout,
                            max_response_bytes: rpc.max_response_bytes,
                            minimum_confirmations: chain.confirmation_depth,
                            allow_insecure_loopback: rpc.allow_insecure_loopback,
                        },
                    )?);
                    sources.push((network.to_owned(), source));
                }
            }
        }
        EvmReceiptRegistry::from_sources(sources, authorizations)
    }

    fn secret_headers(headers: &BTreeMap<String, SecretHeaderConfig>, description: &str) -> crate::Result<HeaderMap> {
        let values = resolved_header_values(headers, description)?;
        let mut resolved = HeaderMap::new();
        for (name, value) in values {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::Config(format!("{description} header name is invalid")))?;
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| Error::Config(format!("{description} header value is invalid")))?;
            value.set_sensitive(true);
            resolved.insert(name, value);
        }
        Ok(resolved)
    }

    fn resolved_header_values(
        headers: &BTreeMap<String, SecretHeaderConfig>,
        description: &str,
    ) -> crate::Result<BTreeMap<String, String>> {
        headers
            .iter()
            .map(|(name, header)| {
                let secret = read_secret_ref(
                    &header.source,
                    &format!("{description} header {name}"),
                    MAX_OPERATOR_SECRET_BYTES,
                )?;
                Ok((name.clone(), format!("{}{}", header.prefix, secret.as_str())))
            })
            .collect()
    }

    pub(super) fn seconds(duration: std::time::Duration, field: &str) -> anyhow::Result<i64> {
        i64::try_from(duration.as_secs()).map_err(|_| anyhow!("{field} exceeds the supported timestamp range"))
    }
}

mod lifecycle {
    use std::time::Duration;

    use anyhow::{anyhow, Context as _};
    use async_trait::async_trait;
    use tokio::{sync::watch, task::JoinSet};

    #[async_trait]
    pub trait RuntimeComponent: Send {
        fn name(&self) -> &'static str;

        /// Runs until shutdown or a component failure.
        async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()>;
    }

    /// Long-lived provider registry closed only after all request handlers and
    /// background components have completed graceful shutdown.
    #[async_trait]
    pub trait RuntimeResource: Send {
        fn name(&self) -> &'static str;

        async fn close(self: Box<Self>) -> anyhow::Result<()>;
    }

    #[derive(Clone)]
    pub struct Shutdown {
        receiver: watch::Receiver<bool>,
    }

    impl Shutdown {
        pub async fn cancelled(&mut self) {
            while !*self.receiver.borrow() {
                if self.receiver.changed().await.is_err() {
                    break;
                }
            }
        }
    }

    pub struct ServerSupervisor {
        components: Vec<Box<dyn RuntimeComponent>>,
        shutdown_timeout: Duration,
    }

    impl ServerSupervisor {
        #[must_use]
        pub fn new(shutdown_timeout: Duration) -> Self {
            Self {
                components: Vec::new(),
                shutdown_timeout,
            }
        }

        pub fn add<C>(&mut self, component: C)
        where
            C: RuntimeComponent + 'static,
        {
            self.components.push(Box::new(component));
        }

        pub fn add_boxed(&mut self, component: Box<dyn RuntimeComponent>) {
            self.components.push(component);
        }

        /// Starts every registered component and coordinates one graceful shutdown.
        ///
        /// # Errors
        ///
        /// Returns the first component, join, signal, or graceful-timeout error.
        pub async fn run(self) -> anyhow::Result<()> {
            if self.components.is_empty() {
                return Err(anyhow!("no runtime component was enabled"));
            }
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let mut tasks = JoinSet::new();
            for component in self.components {
                let name = component.name();
                let shutdown = Shutdown {
                    receiver: shutdown_rx.clone(),
                };
                tasks.spawn(async move { component.run(shutdown).await.with_context(|| format!("{name} stopped")) });
            }

            let first_error = tokio::select! {
                result = process_shutdown_signal() => {
                    result?;
                    tracing::info!("shutdown signal received");
                    None
                }
                result = tasks.join_next() => {
                    Some(match result {
                        Some(Ok(Ok(()))) => anyhow!("runtime component exited before shutdown"),
                        Some(Ok(Err(error))) => error,
                        Some(Err(error)) => anyhow!("runtime component task failed: {error}"),
                        None => anyhow!("no runtime component was started"),
                    })
                }
            };
            let _ = shutdown_tx.send(true);
            let graceful = async {
                let mut error = first_error;
                while let Some(result) = tasks.join_next().await {
                    let result =
                        result.unwrap_or_else(|join_error| Err(anyhow!("runtime component task failed: {join_error}")));
                    if error.is_none() {
                        error = result.err();
                    }
                }
                error
            };
            let Ok(error) = tokio::time::timeout(self.shutdown_timeout, graceful).await else {
                tasks.abort_all();
                return Err(anyhow!(
                    "graceful shutdown exceeded {} seconds",
                    self.shutdown_timeout.as_secs_f64()
                ));
            };
            error.map_or(Ok(()), Err)
        }
    }

    async fn process_shutdown_signal() -> anyhow::Result<()> {
        let ctrl_c = async { tokio::signal::ctrl_c().await.context("install Ctrl+C handler") };
        #[cfg(unix)]
        let terminate = async {
            let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
            let _ = signal.recv().await;
            Ok(())
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<anyhow::Result<()>>();

        tokio::select! {
            result = ctrl_c => result,
            result = terminate => result,
        }
    }
}

pub use lifecycle::{RuntimeComponent, RuntimeResource, ServerSupervisor, Shutdown};

pub(super) fn console_approval_handler(
    config: &AppConfig,
    ports: &crate::storage::StoragePorts,
) -> anyhow::Result<std::sync::Arc<crate::handler::ApprovalHandler>> {
    payment_runtime::console_approval_handler(config, ports)
}

use anyhow::Context as _;
use axum::Router;

use crate::{
    apm::{init_telemetry, AgentVaultMetrics, TelemetryConfig},
    config::AppConfig,
    controller::http::{HttpApiServer, PrincipalContext},
    controller::{ManagementServer, ReadinessProbe},
};

/// Process runtime assembled from enterprise payment components.
///
/// The core owns lifecycle and observability only. HTTP handlers, SQL-backed
/// payment services, and messaging consumers are injected at the composition
/// root, which prevents the daemon from silently selecting another storage
/// implementation.
pub struct AgentVaultServer {
    config: AppConfig,
    api_router: Option<Router>,
    components: Vec<Box<dyn RuntimeComponent>>,
    resources: Vec<Box<dyn RuntimeResource>>,
    readiness_probes: Vec<std::sync::Arc<dyn ReadinessProbe>>,
    metrics: AgentVaultMetrics,
}

impl AgentVaultServer {
    #[must_use]
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            api_router: None,
            components: Vec::new(),
            resources: Vec::new(),
            readiness_probes: Vec::new(),
            metrics: AgentVaultMetrics::default(),
        }
    }

    /// Builds the production payment runtime from validated operator
    /// configuration. No filesystem authority or implicit adapter fallback is
    /// selected when a requested backend or transport is unavailable.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, secrets, SQL, protocols,
    /// providers, signing, RPC, or transport construction fails.
    pub async fn assemble(config: AppConfig) -> anyhow::Result<Self> {
        config.validate_for_serve().context("validate service configuration")?;
        let mut server = Self::new(config.clone());
        if config.agentvault.enabled() {
            let payment = payment_runtime::assemble(&config, server.metrics.clone()).await?;
            server = server.with_readiness_probe(payment.storage_readiness);
            server = server.with_api_router(payment.router);
            if let Some(grpc) = payment.grpc {
                server = server.with_component(grpc);
            }
            if let Some(local) = payment.local {
                server = server.with_component(local);
            }
            if let Some(mqtt) = payment.mqtt {
                server = server.with_component(mqtt);
            }
            server = server.with_component(payment.cache_health);
            if let Some(reconciliation) = payment.reconciliation {
                server = server.with_component(reconciliation);
            }
            if let Some(wallet_balance_sync) = payment.wallet_balance_sync {
                server = server.with_component(wallet_balance_sync);
            }
            server = server.with_component(payment.approval_expiration);
            server = server.with_resource(payment.resources);
        } else if config.server.enabled
            || config.agentvault.transport.grpc.enabled
            || config.agentvault.transport.local.enabled
            || config.agentvault.transport.mqtt.enabled
        {
            anyhow::bail!("an AgentVault data-plane transport is enabled while payer and payee are disabled");
        }
        Ok(server)
    }

    /// Installs the payment API router assembled from the shared handlers.
    #[must_use]
    pub fn with_api_router(mut self, router: Router) -> Self {
        self.api_router = Some(router);
        self
    }

    /// Installs a transport or background component that shares the same
    /// handler/storage composition root.
    #[must_use]
    pub fn with_component<C>(mut self, component: C) -> Self
    where
        C: RuntimeComponent + 'static,
    {
        self.components.push(Box::new(component));
        self
    }

    /// Installs a provider registry whose resources close after every runtime
    /// component has stopped using them.
    #[must_use]
    pub fn with_resource<R>(mut self, resource: R) -> Self
    where
        R: RuntimeResource + 'static,
    {
        self.resources.push(Box::new(resource));
        self
    }

    fn with_readiness_probe(mut self, probe: std::sync::Arc<dyn ReadinessProbe>) -> Self {
        self.readiness_probes.push(probe);
        self
    }

    /// Opens configured runtime components and serves until process shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error for configuration, telemetry, secret, listener, or
    /// component failures.
    pub async fn run(self) -> anyhow::Result<()> {
        self.config
            .validate_for_serve()
            .context("validate service configuration")?;
        let telemetry = init_telemetry(&TelemetryConfig::from_settings(
            &self.config.server.service_name,
            &self.config.logging,
            &self.config.mgmt.otel,
        ))
        .context("configure AgentVault telemetry")?;
        let metrics = self.metrics.clone();
        let mut supervisor = ServerSupervisor::new(self.config.server.shutdown_timeout);

        if self.config.server.enabled {
            let router = self
                .api_router
                .context("payment HTTP API is enabled but no router was installed")?;
            let principal_context = PrincipalContext::from_config(&self.config.server.authguard)?;
            supervisor.add(HttpApiServer::new(
                self.config.server.clone(),
                principal_context,
                router,
                metrics.clone(),
            ));
        }
        if self.config.mgmt.enabled {
            supervisor.add(ManagementServer::with_readiness_probes(
                self.config.mgmt.clone(),
                metrics,
                self.readiness_probes.clone(),
            ));
        }
        for component in self.components {
            supervisor.add_boxed(component);
        }

        tracing::info!(
            service.name = %self.config.server.service_name,
            api.enabled = self.config.server.enabled,
            management.enabled = self.config.mgmt.enabled,
            "AgentVault runtime started"
        );
        let runtime_result = supervisor.run().await;
        let mut resource_error = None;
        for resource in self.resources.into_iter().rev() {
            let name = resource.name();
            if let Err(error) = resource.close().await.with_context(|| format!("close {name}")) {
                tracing::error!(resource = name, %error, "runtime resource shutdown failed");
                if resource_error.is_none() {
                    resource_error = Some(error);
                }
            }
        }
        telemetry.shutdown();
        runtime_result?;
        resource_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/agentvault_test.rs"]
mod tests;
