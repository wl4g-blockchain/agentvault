//! Production payment composition root.
//!
//! Domain handlers depend only on ports. This module is the single place that
//! resolves operator configuration into concrete SQL, protocol, provider,
//! signer, RPC, HTTP, and MQTT adapters.

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
    handler::job::{
        ApprovalExpirationJob, PaymentEvidenceRouter, ReconciliationConfig as JobReconciliationConfig,
        ReconciliationEvidenceSource, ReconciliationJob,
    },
    handler::{
        ApprovalHandler, ApprovalPrincipalAuthorizer, EvmEvidenceSource, EvmReceiptRegistry, EvmRpcBalanceSource,
        EvmRpcReceiptConfig, EvmRpcReceiptSource, JwtApprovalGrantTokenService, OfferHandler,
        OutboundEvidenceDependencies, OutboundEvidenceHandler, OutboundSigningDependencies, OutboundSigningHandler,
        PaymentHandler, ProtocolRegistry, ScenarioHandler, SettlementDependencies, SettlementHandler,
        SettlementProviderConfig, SettlementProviderRegistry, StoredApprovalPrincipalAuthorizer, WalletBalanceRegistry,
        WalletBalanceSource, X402FacilitatorProvider,
    },
    protocol::{
        mpp::{MppAdapter, MppAsset},
        x402::{X402Adapter, X402Asset},
        PaymentProtocol,
    },
    storage::{
        ApprovalStorage, ApproverStorage, ClientStorage, EventStorage, IStorage, LedgerStorage, OfferStorage,
        OutboxStorage, PaymentStorage, PolicyStorage, ScenarioStorage, SigningStorage, StorageFactory, StoragePorts,
        WalletPaymentSigner, WalletStorage,
    },
    utils::secret::read_secret_ref,
    utils::secure_material::{Aes256GcmMaterialProtector, SensitiveMaterialProtector},
    Error,
};

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
    pub approval_expiration: ApprovalExpirationComponent,
    pub storage_readiness: Arc<dyn ReadinessProbe>,
    pub resources: PaymentRuntimeResources,
}

pub(super) async fn assemble(
    config: &AppConfig,
    metrics: AgentVaultMetrics,
) -> anyhow::Result<AssembledPaymentRuntime> {
    let cache_factory = Arc::new(CacheFactory::new());
    let storage_factory = Arc::new(StorageFactory::new());
    let assembled = assemble_with_factories(
        config,
        Arc::clone(&cache_factory),
        Arc::clone(&storage_factory),
        metrics,
    )
    .await;
    if assembled.is_err() {
        let _ = cache_factory.shutdown_all().await;
        let _ = storage_factory.shutdown_all().await;
    }
    assembled
}

async fn assemble_with_factories(
    config: &AppConfig,
    cache_factory: Arc<CacheFactory>,
    storage_factory: Arc<StorageFactory>,
    metrics: AgentVaultMetrics,
) -> anyhow::Result<AssembledPaymentRuntime> {
    let cache = cache_factory
        .create(&config.cache)
        .await
        .context("initialize non-authoritative cache")?;
    let storage = storage_factory
        .create(&config.storage)
        .await
        .context("initialize authoritative storage")?;
    let wallet_signer = Arc::clone(&storage)
        .wallet_payment_signer(&config.agentvault.key_management)
        .context("load wallet KEK ring")?;
    let ports = PaymentPorts::from_storage(Arc::clone(&storage).ports(), wallet_signer);
    let clients = Arc::clone(&ports.clients);
    let scenarios = Arc::clone(&ports.scenarios);
    let outbox = Arc::clone(&ports.outbox);
    let application = build_application(config, ports, metrics.clone())?;
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
            cache,
            interval: std::time::Duration::from_secs(30),
        },
        reconciliation: application.reconciliation,
        approval_expiration: application.approval_expiration,
        storage_readiness: Arc::new(StorageReadinessProbe { storage }),
        resources: PaymentRuntimeResources {
            cache_factory,
            storage_factory,
        },
    })
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
    cache_factory: Arc<CacheFactory>,
    storage_factory: Arc<StorageFactory>,
}

#[async_trait]
impl RuntimeResource for PaymentRuntimeResources {
    fn name(&self) -> &'static str {
        "payment provider registries"
    }

    async fn close(self: Box<Self>) -> anyhow::Result<()> {
        let cache_result = self.cache_factory.shutdown_all().await.context("close cache providers");
        let storage_result = self
            .storage_factory
            .shutdown_all()
            .await
            .context("close storage providers");
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
    payments: Arc<dyn PaymentStorage>,
    approvals: Arc<dyn ApprovalStorage>,
    events: Arc<dyn EventStorage>,
    ledger: Arc<dyn LedgerStorage>,
    outbox: Arc<dyn OutboxStorage>,
    signing: Arc<dyn SigningStorage>,
    wallet_signer: Arc<dyn WalletPaymentSigner>,
}

impl PaymentPorts {
    fn from_storage(ports: StoragePorts, wallet_signer: Arc<dyn WalletPaymentSigner>) -> Self {
        Self {
            clients: ports.clients,
            scenarios: ports.scenarios,
            approvers: ports.approvers,
            policies: ports.policies,
            offers: ports.offers,
            wallets: ports.wallets,
            payments: ports.payments,
            approvals: ports.approvals,
            events: ports.events,
            ledger: ports.ledger,
            outbox: ports.outbox,
            signing: ports.signing,
            wallet_signer,
        }
    }
}

struct BuiltPaymentApplication {
    api: Arc<PaymentApi>,
    reconciliation: Option<ReconciliationComponent>,
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
    metrics: AgentVaultMetrics,
) -> anyhow::Result<BuiltPaymentApplication> {
    let PaymentPorts {
        clients,
        scenarios: scenarios_store,
        approvers,
        policies,
        offers,
        wallets,
        payments,
        approvals,
        events,
        ledger,
        outbox: _,
        signing,
        wallet_signer,
    } = ports;
    let adapters = build_adapters(config)?;

    let scenarios = Arc::new(ScenarioHandler::new(Arc::clone(&clients), Arc::clone(&scenarios_store)));
    let payments_handler = build_payment_handler(
        config,
        Arc::clone(&scenarios),
        Arc::clone(&policies),
        Arc::clone(&payments),
        Arc::clone(&adapters.payer_protocols),
    )?;
    let (approval_authorizer, approval_handler) = build_approval_services(
        config,
        approvers,
        Arc::clone(&approvals),
        Arc::clone(&payments),
        Arc::clone(&adapters.material_protector),
    )?;
    let offer_handler = Arc::new(OfferHandler::new(
        Arc::clone(&scenarios),
        Arc::clone(&offers),
        Arc::clone(&policies),
        Arc::clone(&wallets),
        Arc::clone(&adapters.payee_protocols),
    ));
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
    let approval_expiration = build_approval_expiration(config, Arc::clone(&approvals))?;
    let api = build_payment_api(PaymentApiDependencies {
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
        events,
        ledger,
        balances: adapters.balances,
        metrics,
    });
    Ok(BuiltPaymentApplication {
        api,
        reconciliation,
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
) -> anyhow::Result<Arc<PaymentHandler>> {
    Ok(Arc::new(
        PaymentHandler::new(
            scenarios,
            policies,
            payments,
            protocols,
            seconds(config.agentvault.approval.request_ttl, "approval request TTL")?,
        )
        .context("build outbound payment handler")?,
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
    let authorizer: Arc<dyn ApprovalPrincipalAuthorizer> = Arc::new(StoredApprovalPrincipalAuthorizer::new(approvers));
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

fn reconciliation_worker_id(service_name: &str) -> String {
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
    let mut adapters: Vec<Arc<dyn PaymentProtocol>> = Vec::new();
    let x402 = &config.agentvault.payer.x402;
    if x402.enabled {
        adapters.push(Arc::new(X402Adapter::new(
            x402_assets(&x402.assets),
            x402.max_challenge_ttl.as_secs(),
        )?));
    }
    ProtocolRegistry::new(adapters)
}

fn build_payee_protocol_registry(config: &AppConfig) -> crate::Result<ProtocolRegistry> {
    let mut adapters: Vec<Arc<dyn PaymentProtocol>> = Vec::new();
    let x402 = &config.agentvault.payee.x402;
    if x402.enabled {
        adapters.push(Arc::new(X402Adapter::new(
            x402_assets(&x402.assets),
            x402.max_challenge_ttl.as_secs(),
        )?));
    }
    let mpp = &config.agentvault.payee.mpp;
    if mpp.enabled {
        let secret = read_secret_ref(&mpp.challenge_secret, "MPP challenge secret", MAX_OPERATOR_SECRET_BYTES)?;
        adapters.push(Arc::new(MppAdapter::new_payee(
            mpp_assets(&mpp.assets),
            mpp.realm.clone(),
            secret.to_string(),
            mpp.max_challenge_ttl.as_secs(),
        )?));
    }
    ProtocolRegistry::new(adapters)
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

fn mpp_assets(assets: &[MppAssetConfig]) -> Vec<MppAsset> {
    assets
        .iter()
        .map(|asset| MppAsset {
            network: asset.network.clone(),
            currency: asset.currency.clone(),
            symbol: asset.symbol.clone(),
            token_contract: asset.token_contract.clone(),
            decimals: asset.decimals,
        })
        .collect()
}

fn build_balance_registry(config: &AppConfig) -> crate::Result<WalletBalanceRegistry> {
    let mut sources = Vec::new();
    for (network, chain) in &config.agentvault.chains {
        if chain.family != ChainFamily::Evm {
            continue;
        }
        let rpc = &chain.rpc;
        let headers = resolved_header_values(&rpc.secret_headers, &format!("chain RPC {network}"))?;
        let source: Arc<dyn WalletBalanceSource> = Arc::new(EvmRpcBalanceSource::new(
            &rpc.endpoint,
            headers,
            rpc.timeout,
            rpc.max_response_bytes,
            chain.minimum_native_balance,
            rpc.allow_insecure_loopback,
        )?);
        sources.push((network.clone(), source));
    }
    WalletBalanceRegistry::new(sources)
}

fn build_receipt_registry(config: &AppConfig) -> crate::Result<EvmReceiptRegistry> {
    let mut sources = Vec::new();
    let mut configured = std::collections::BTreeSet::new();
    let outbound_assets = config
        .agentvault
        .payer
        .x402
        .enabled
        .then_some(config.agentvault.payer.x402.assets.as_slice())
        .into_iter()
        .flatten();
    let inbound_assets = config
        .agentvault
        .payee
        .x402
        .enabled
        .then_some(config.agentvault.payee.x402.assets.as_slice())
        .into_iter()
        .flatten();
    for asset in outbound_assets.chain(inbound_assets) {
        if !configured.insert(asset.network.clone()) {
            continue;
        }
        let chain = config
            .agentvault
            .chains
            .get(&asset.network)
            .ok_or_else(|| Error::Config(format!("no fixed chain RPC configured for {}", asset.network)))?;
        let rpc = &chain.rpc;
        let source: Arc<dyn EvmEvidenceSource> = Arc::new(EvmRpcReceiptSource::new(
            &asset.network,
            EvmRpcReceiptConfig {
                endpoint: rpc.endpoint.clone(),
                secret_headers: resolved_header_values(&rpc.secret_headers, &format!("EVM RPC {}", asset.network))?,
                timeout: rpc.timeout,
                max_response_bytes: rpc.max_response_bytes,
                minimum_confirmations: chain.confirmation_depth,
                allow_insecure_loopback: rpc.allow_insecure_loopback,
            },
        )?);
        sources.push((asset.network.clone(), source));
    }
    EvmReceiptRegistry::new(sources)
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

fn seconds(duration: std::time::Duration, field: &str) -> anyhow::Result<i64> {
    i64::try_from(duration.as_secs()).map_err(|_| anyhow!("{field} exceeds the supported timestamp range"))
}

#[cfg(test)]
#[path = "../../tests/cmd/payment_runtime_test.rs"]
mod tests;
