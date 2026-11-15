use super::*;
#[derive(Clone)]
struct LiveTempoBalanceSource {
    provider: mpp::server::TempoProvider,
    token: Address,
}

#[async_trait::async_trait]
impl agentvault_payer::handler::balance::WalletBalanceSource for LiveTempoBalanceSource {
    async fn load(
        &self,
        wallet: &crate::model::Wallet,
        asset: &AssetIdentity,
    ) -> crate::Result<crate::model::WalletBalanceSnapshot> {
        if asset.network != format!("eip155:{LIVE_TEMPO_CHAIN_ID}")
            || asset.token_contract.as_deref() != Some(LIVE_TEMPO_PATH_USD)
        {
            return Err(crate::Error::Settlement(
                "Tempo balance source received an unconfigured asset".to_owned(),
            ));
        }
        let account = wallet
            .address
            .parse::<Address>()
            .map_err(|_| crate::Error::Settlement("Tempo balance wallet is invalid".to_owned()))?;
        let available = live_tempo_balance(&self.provider, self.token, account).await;
        let available = u128::try_from(available)
            .map_err(|_| crate::Error::Settlement("Tempo balance exceeds Amount".to_owned()))?;
        Ok(crate::model::WalletBalanceSnapshot {
            asset_available: Amount::new(available),
            // Tempo charges gas in the configured fee token. The native-gas
            // threshold is therefore intentionally zero for this source.
            native_available: Amount::ZERO,
            minimum_native_required: Amount::ZERO,
        })
    }
}

struct LiveTempoClock(i64);

impl crate::util::clock::UnixClock for LiveTempoClock {
    fn now(&self) -> crate::Result<i64> {
        Ok(self.0)
    }
}

fn live_tempo_storage_config(database: &std::path::Path, kek_env: &str) -> crate::config::AppConfig {
    let mut config = crate::config::AppConfig::default();
    config.storage.provider = crate::config::StorageProvider::Sqlite;
    config.storage.sqlite.database_file = database.to_string_lossy().into_owned();
    config.agentvault.key_management.active_key_id = "live-tempo-kek-v1".to_owned();
    config.agentvault.key_management.keys = BTreeMap::from([(
        "live-tempo-kek-v1".to_owned(),
        crate::config::SecretRefConfig {
            file: String::new(),
            env: kek_env.to_owned(),
        },
    )]);
    config
}

async fn import_live_tempo_storage(
    config: &crate::config::AppConfig,
    manifest_path: &std::path::Path,
    manifest: &Value,
) -> std::sync::Arc<crate::storage::base_sqlite::SqliteRepository> {
    std::fs::write(
        manifest_path,
        agentvault_common::resource::ResourceImporter::encode_test_resource_documents(manifest),
    )
    .expect("write live Tempo resource documents");
    agentvault_common::resource::ResourceImporter::import_from_paths(config, &[manifest_path.to_path_buf()])
        .await
        .expect("import live Tempo Vault resources");
    std::sync::Arc::new(
        crate::storage::base_sqlite::SqliteRepository::connect(&config.storage.sqlite)
            .await
            .expect("open live Tempo Vault storage"),
    )
}

#[allow(clippy::similar_names, clippy::too_many_lines)]
#[tokio::test]
#[ignore = "requires the pinned Tempo localnet E2E profile"]
async fn live_tempo_vault_handlers_recover_once_and_reconcile_both_ledgers() {
    use crate::apm::AgentVaultMetrics;
    use crate::model::wallet::WalletSelectionStrategy;
    use agentvault_payee::handler::settlement::IssuePaymentChallenge;
    use crate::model::PaymentAuthorizationResult;
    use crate::model::PaymentEventType;
    use crate::model::PaymentStatus;
    use crate::model::RecordOutboundPaymentResponse;
    use agentvault_payee::handler::settlement::SettleIncomingPayment;
    use crate::model::SignAuthorizedPayment;
    use agentvault_payee::handler::settlement::VerifyIncomingPayment;
    use crate::storage::wallet::sqlite::SqliteWalletKeyRepository;
    use crate::storage::IEventRepository;
    use crate::storage::IIntentRepository;
    use crate::storage::ILedgerRepository;
    use crate::storage::LedgerQuery;
    use crate::storage::PageRequest;
    use crate::util::secure_material::Aes256GcmMaterialProtector;
    use crate::util::secure_material::SensitiveMaterialProtector;
    use agentvault_common::handler::scenario::ScenarioHandler;
    use agentvault_common::storage::reconciliation::PaymentEvidenceRouter;
    use agentvault_common::storage::reconciliation::ReconciliationConfig;
    use agentvault_common::storage::reconciliation::ReconciliationEvidenceSource;
    use agentvault_common::storage::reconciliation::ReconciliationJob;
    use agentvault_payee::handler::provider::ExternalSettlementProvider;
    use agentvault_payee::handler::provider::SettlementProviderRegistry;
    use agentvault_payee::handler::settlement::SettlementCoordinator;
    use agentvault_payee::handler::settlement::SettlementDependencies;
    use agentvault_payee::protocol::PaymentProtocolFactory;
    use agentvault_payer::handler::authorization::PaymentAuthorizer;
    use agentvault_payer::handler::evidence::MerchantEvidenceRecorder;
    use agentvault_payer::handler::evidence::OutboundEvidenceDependencies;
    use agentvault_payer::handler::signing::OutboundSigner;
    use agentvault_payer::handler::signing::OutboundSigningDependencies;
    use agentvault_payee::protocol::receipt::EvmEvidenceSource;
    use agentvault_payee::protocol::receipt::EvmRpcReceiptConfig;
    use agentvault_payee::protocol::receipt::EvmRpcReceiptSource;
    use agentvault_payee::protocol::receipt::ReceiptRegistry;
    use alloy::providers::Provider as _;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use std::{sync::Arc, time::Duration};

    let rpc_url = std::env::var("TEMPO_RPC_URL").unwrap_or_else(|_| LIVE_TEMPO_RPC_URL.to_owned());
    let provider = mpp::server::tempo_provider(&rpc_url).expect("create Tempo provider");
    assert_eq!(
        provider.get_chain_id().await.expect("query Tempo chain ID"),
        LIVE_TEMPO_CHAIN_ID
    );

    let token = LIVE_TEMPO_PATH_USD.parse::<Address>().expect("pathUSD address");
    let payer_key = crate::model::wallet::WalletCrypto::generate_private_key();
    let payer = crate::model::wallet::WalletCrypto::address(&payer_key).expect("derive isolated payer");
    let payer_address = payer.parse::<Address>().expect("payer address");
    fund_live_tempo_payer(&provider, token, payer_address).await;
    let recipient_key = crate::model::wallet::WalletCrypto::generate_private_key();
    let recipient = crate::model::wallet::WalletCrypto::address(&recipient_key).expect("derive recipient");
    let recipient_address = recipient.parse::<Address>().expect("recipient address");
    let asset = AssetIdentity {
        network: format!("eip155:{LIVE_TEMPO_CHAIN_ID}"),
        asset: "pathUSD".to_owned(),
        token_contract: Some(LIVE_TEMPO_PATH_USD.to_owned()),
        decimals: 6,
    };
    let configured_asset = MppAsset {
        network: asset.network.clone(),
        currency: LIVE_TEMPO_PATH_USD.to_owned(),
        symbol: asset.asset.clone(),
        token_contract: asset.token_contract.clone(),
        decimals: asset.decimals,
    };

    let temporary = tempfile::TempDir::new().expect("create live Tempo Vault directory");
    let suffix = uuid::Uuid::new_v4().simple().to_string().to_ascii_uppercase();
    let payer_key_env = format!("AGENTVAULT_LIVE_TEMPO_PAYER_KEY_{suffix}");
    let kek_env = format!("AGENTVAULT_LIVE_TEMPO_KEK_{suffix}");
    std::env::set_var(&payer_key_env, hex::encode(&payer_key));
    std::env::set_var(&kek_env, URL_SAFE_NO_PAD.encode([7_u8; 32]));
    let payer_config = live_tempo_storage_config(&temporary.path().join("payer.sqlite3"), &kek_env);
    let payee_config = live_tempo_storage_config(&temporary.path().join("payee.sqlite3"), &kek_env);
    let budget = json!({"asset": "pathUSD", "decimals": 6});
    let policy_asset = serde_json::to_value(&asset).expect("asset JSON");
    let resource = "https://marketpulse.local/mcp/realtime";
    let scenario_id = "scenario-mpp-tempo-handler-live";
    let payer_manifest = json!({
        "version": 1,
        "clients": [{"id": "payer-client", "name": "Tempo payer", "auth_principal": "principal:tempo-payer"}],
        "wallets": [{
            "id": "payer-wallet", "custody": "local", "private_key_env": payer_key_env,
            "budget_unit": budget, "max_per_payment": "10000000", "max_per_day": "100000000"
        }],
        "scenarios": [{"id": scenario_id, "client_id": "payer-client", "name": "Tempo handler E2E"}],
        "policies": [{
            "id": "payer-policy", "scenario_id": scenario_id, "wallet_ids": ["payer-wallet"],
            "protocols": ["mpp"], "networks": [asset.network], "assets": [policy_asset],
            "budget_unit": budget, "allowed_recipients": [recipient],
            "allowed_origins": ["https://marketpulse.local"],
            "outbound_limits": {"max_per_payment": "10000000", "max_per_day": "100000000", "approval_enabled": false}
        }]
    });
    let payee_manifest = json!({
        "version": 1,
        "clients": [{"id": "payee-client", "name": "Tempo payee", "auth_principal": "principal:tempo-payee"}],
        "wallets": [{
            "id": "payee-wallet", "custody": "external", "algorithm": "secp256k1", "address": recipient,
            "budget_unit": budget, "max_per_payment": "10000000", "max_per_day": "100000000"
        }],
        "scenarios": [{"id": scenario_id, "client_id": "payee-client", "name": "Tempo handler E2E"}],
        "policies": [{
            "id": "payee-policy", "scenario_id": scenario_id, "wallet_ids": ["payee-wallet"],
            "protocols": ["mpp"], "networks": [asset.network], "assets": [policy_asset],
            "budget_unit": budget, "allowed_recipients": [recipient],
            "allowed_origins": ["https://marketpulse.local"],
            "inbound_limits": {"max_per_payment": "10000000", "max_per_day": "100000000", "approval_enabled": false}
        }],
        "prices": [{
            "id": "price-mpp-tempo-handler-live", "scenario_id": scenario_id, "policy_id": "payee-policy",
            "resource": resource, "protocols": ["mpp"], "asset": policy_asset,
            "amount": "1000000", "wallet_id": "payee-wallet"
        }]
    });
    let payer_storage = import_live_tempo_storage(
        &payer_config,
        &temporary.path().join("payer-manifest.json"),
        &payer_manifest,
    )
    .await;
    let payee_storage = import_live_tempo_storage(
        &payee_config,
        &temporary.path().join("payee-manifest.json"),
        &payee_manifest,
    )
    .await;

    let payer_protocols = Arc::new(
        PaymentProtocolFactory::new()
            .register(Arc::new(
                MppAdapter::new_payer(vec![configured_asset.clone()], 60).expect("payer MPP adapter"),
            ))
            .build()
            .expect("payer protocol registry"),
    );
    let payee_protocols = Arc::new(
        PaymentProtocolFactory::new()
            .register(Arc::new(
                MppAdapter::new_payee(
                    vec![configured_asset],
                    "marketpulse.local",
                    "tempo-handler-e2e-challenge-secret-32-bytes",
                    60,
                )
                .expect("payee MPP adapter"),
            ))
            .build()
            .expect("payee protocol registry"),
    );
    let payer_ports = crate::storage::IRepository::ports(payer_storage.clone());
    let payee_ports = crate::storage::IRepository::ports(payee_storage.clone());
    let payer_scenarios = Arc::new(ScenarioHandler::new(
        payer_ports.clients.clone(),
        payer_ports.scenarios.clone(),
    ));
    let payee_scenarios = Arc::new(ScenarioHandler::new(
        payee_ports.clients.clone(),
        payee_ports.scenarios.clone(),
    ));
    let protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
        Aes256GcmMaterialProtector::new(
            "live-tempo-recovery-v1",
            [("live-tempo-recovery-v1".to_owned(), [9_u8; 32])],
        )
        .expect("recovery protector"),
    );

    let payer_handler = PaymentAuthorizer::new(
        payer_scenarios.clone(),
        payer_ports.policies.clone(),
        payer_ports.payments.clone(),
        payer_protocols.clone(),
        30,
        WalletSelectionStrategy::LeastSpent,
        AgentVaultMetrics::default(),
    )
    .expect("payer handler");
    let signer: Arc<dyn crate::storage::WalletPaymentSigner> = Arc::new(
        SqliteWalletKeyRepository::from_config((*payer_storage).clone(), &payer_config.agentvault.key_management)
            .expect("payer custody signer"),
    );
    let signing_handler = OutboundSigner::new(OutboundSigningDependencies {
        storage: payer_ports.signing.clone(),
        protocols: payer_protocols.clone(),
        balances: Arc::new(LiveTempoBalanceSource {
            provider: provider.clone(),
            token,
        }),
        signer,
        material_protector: protector.clone(),
    });

    let (external, broadcasts, server) = start_live_tempo_facilitator(&provider, &asset).await;
    let external: Arc<dyn ExternalSettlementProvider> = Arc::new(external);
    let providers = Arc::new(
        SettlementProviderRegistry::new([(
            MPP_PROTOCOL.to_owned(),
            "mpp-tempo-localnet".to_owned(),
            BTreeMap::from([(asset.network.clone(), 1)]),
            external,
        )])
        .expect("MPP settlement provider registry"),
    );
    let empty_sources: Vec<(String, Arc<dyn EvmEvidenceSource>)> = Vec::new();
    let payee_handler = Arc::new(
        SettlementCoordinator::new(
            SettlementDependencies {
                scenarios: payee_scenarios,
                prices: payee_ports.prices.clone(),
                policies: payee_ports.policies.clone(),
                wallets: payee_ports.wallets.clone(),
                payments: payee_ports.payments.clone(),
                events: payee_ports.events.clone(),
                protocols: payee_protocols,
                providers,
                material_protector: protector.clone(),
                authorizations: Arc::new(ReceiptRegistry::new(empty_sources).expect("empty receipt registry")),
            },
            1,
        )
        .expect("payee settlement handler"),
    );

    // Import timestamps are the key activation boundary. Advance the
    // request clock by one second so the test exercises an active key without
    // relying on wall-clock scheduling between the two operations.
    let now = time::OffsetDateTime::now_utc().unix_timestamp().saturating_add(1);
    let run_id = "run-mpp-tempo-handler-live";
    let request_hash = "ab".repeat(32);
    let issued = payee_handler
        .issue_challenge(
            "principal:tempo-payee",
            &IssuePaymentChallenge {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                price_id: "price-mpp-tempo-handler-live".to_owned(),
                resource: resource.to_owned(),
                origin: "https://marketpulse.local".to_owned(),
                protocol: MPP_PROTOCOL.to_owned(),
                request_hash: request_hash.clone(),
            },
            now,
        )
        .await
        .expect("payee issues durable MPP challenge");
    let payer_protocol = payer_protocols.get(MPP_PROTOCOL).expect("payer MPP protocol");
    let challenge = payer_protocol
        .parse_challenge(scenario_id, resource, &issued.payload, now, Some(issued.expires_at))
        .await
        .expect("payer parses MPP challenge");
    let authorization = payer_handler
        .authorize(
            "principal:tempo-payer",
            &crate::model::AuthorizePayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                challenge: challenge.clone(),
            },
            now.saturating_add(1),
        )
        .await
        .expect("payer authorizes MPP intent");
    let intent_id = match authorization {
        PaymentAuthorizationResult::Authorized { intent_id, .. } => intent_id,
        other => panic!("MPP handler E2E was not authorized: {other:?}"),
    };
    let signed = signing_handler
        .sign(
            "principal:tempo-payer",
            &SignAuthorizedPayment {
                intent_id: intent_id.clone(),
                challenge,
            },
            now.saturating_add(2),
        )
        .await
        .expect("Vault custody signs MPP charge");
    assert_eq!(signed.intent.run_id, run_id);
    assert_eq!(signed.intent.status, PaymentStatus::Signed);

    let verification = payee_handler
        .verify_payment(
            "principal:tempo-payee",
            &VerifyIncomingPayment {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                challenge_hash: issued.challenge_hash.clone(),
                request_hash,
                credential: signed.credential,
            },
            now.saturating_add(3),
        )
        .await
        .expect("payee verifies MPP credential through facilitator");
    assert!(verification.valid);
    let verification_ref = verification.verification_ref.expect("verification reference");
    let settle = SettleIncomingPayment {
        scenario_id: scenario_id.to_owned(),
        run_id: run_id.to_owned(),
        intent_id: verification.intent.id.clone(),
        verification_ref,
    };
    let payer_before = live_tempo_balance(&provider, token, payer_address).await;
    let recipient_before = live_tempo_balance(&provider, token, recipient_address).await;
    assert!(
        payee_handler
            .settle_payment("principal:tempo-payee", &settle, now.saturating_add(4))
            .await
            .is_err(),
        "the first handler settle must observe the injected lost response"
    );
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let settled = payee_handler
        .settle_payment("principal:tempo-payee", &settle, now.saturating_add(5))
        .await
        .expect("payee recovers settlement through read-only verify");
    let replay = payee_handler
        .settle_payment("principal:tempo-payee", &settle, now.saturating_add(6))
        .await
        .expect("settled payee retry is idempotent");
    assert_eq!(replay.receipt, settled.receipt);
    assert_eq!(settled.intent.run_id, run_id);
    assert_eq!(broadcasts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let transaction = settled.receipt.tx_hash.clone().expect("settled Tempo transaction");
    let (payer_after, recipient_after, gas_fee, gas_limit, gas_used) = assert_live_tempo_accounting(
        &provider,
        &LiveTempoAccounts {
            token,
            payer: payer_address,
            recipient: recipient_address,
        },
        &transaction,
        settled.intent.amount,
        payer_before,
        recipient_before,
    )
    .await;

    let receipt_header = mpp::Receipt::success(TEMPO_METHOD, transaction.clone())
        .to_header()
        .expect("MPP receipt header");
    let receipt_source: Arc<dyn EvmEvidenceSource> = Arc::new(
        EvmRpcReceiptSource::new(
            &asset.network,
            EvmRpcReceiptConfig {
                endpoint: rpc_url,
                secret_headers: BTreeMap::new(),
                timeout: Duration::from_secs(10),
                max_response_bytes: 1_048_576,
                minimum_confirmations: 1,
                allow_insecure_loopback: true,
            },
        )
        .expect("Tempo receipt source"),
    );
    let receipt_registry =
        Arc::new(ReceiptRegistry::new([(asset.network.clone(), receipt_source)]).expect("Tempo receipt registry"));
    let outbound = Arc::new(MerchantEvidenceRecorder::new(OutboundEvidenceDependencies {
        scenarios: payer_scenarios,
        payments: payer_ports.payments.clone(),
        events: payer_ports.events.clone(),
        protocols: payer_protocols,
        material_protector: protector,
        receipts: receipt_registry,
    }));
    let reported = outbound
        .record_merchant_response(
            "principal:tempo-payer",
            &RecordOutboundPaymentResponse {
                scenario_id: scenario_id.to_owned(),
                run_id: run_id.to_owned(),
                intent_id: intent_id.clone(),
                protocol: MPP_PROTOCOL.to_owned(),
                header_name: "payment-receipt".to_owned(),
                header_value: receipt_header,
            },
            now.saturating_add(7),
        )
        .await
        .expect("payer records untrusted MPP merchant receipt");
    assert!(reported.accepted);
    assert_eq!(reported.intent.status, PaymentStatus::PendingSettlement);

    let provider_evidence: Arc<dyn ReconciliationEvidenceSource> = payee_handler;
    let router: Arc<dyn ReconciliationEvidenceSource> =
        Arc::new(PaymentEvidenceRouter::new(provider_evidence, outbound));
    let job = ReconciliationJob::with_clock(
        payer_ports.payments.clone(),
        router,
        Arc::new(LiveTempoClock(now.saturating_add(20))),
        ReconciliationConfig {
            worker_id: "mpp-tempo-handler-live".to_owned(),
            batch_size: 10,
            lease_duration: Duration::from_secs(10),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(8),
        },
    )
    .expect("payer reconciliation job");
    let reconciliation = job.run_once().await.expect("reconcile payer MPP receipt");
    assert_eq!(reconciliation.settled, 1);
    let payer_intent = IIntentRepository::get(payer_ports.payments.as_ref(), &intent_id)
        .await
        .expect("load payer intent")
        .expect("payer intent exists");
    assert_eq!(payer_intent.status, PaymentStatus::Settled);
    assert_eq!(payer_intent.run_id, run_id);
    assert_eq!(payer_intent.tx_hash.as_deref(), Some(transaction.as_str()));

    let payer_ledger = ILedgerRepository::list(
        payer_ports.ledger.as_ref(),
        &LedgerQuery {
            intent_id: Some(intent_id.clone()),
            ..LedgerQuery::default()
        },
    )
    .await
    .expect("payer ledger");
    let payee_ledger = ILedgerRepository::list(
        payee_ports.ledger.as_ref(),
        &LedgerQuery {
            intent_id: Some(settled.intent.id.clone()),
            ..LedgerQuery::default()
        },
    )
    .await
    .expect("payee ledger");
    assert_eq!(payer_ledger.total, 1);
    assert_eq!(payee_ledger.total, 1);
    assert_eq!(payer_ledger.items[0].tx_hash.as_deref(), Some(transaction.as_str()));
    assert_eq!(payee_ledger.items[0].tx_hash.as_deref(), Some(transaction.as_str()));
    let payee_events = IEventRepository::list_for_intent(
        payee_ports.events.as_ref(),
        &settled.intent.id,
        PageRequest { offset: 0, limit: 100 },
    )
    .await
    .expect("payee events");
    assert_eq!(
        payee_events
            .items
            .iter()
            .filter(|event| event.event_type == PaymentEventType::SettlementRequested)
            .count(),
        1
    );

    println!(
        "MPP_TEMPO_EVIDENCE={}",
        json!({
            "run_id": run_id,
            "protocol": MPP_PROTOCOL,
            "method": TEMPO_METHOD,
            "chain_id": LIVE_TEMPO_CHAIN_ID,
            "transaction": transaction,
            "amount": settled.intent.amount.to_string(),
            "gas_fee": gas_fee.to_string(),
            "gas_limit": gas_limit,
            "gas_used": gas_used,
            "payer_before": payer_before.to_string(),
            "payer_after": payer_after.to_string(),
            "recipient_before": recipient_before.to_string(),
            "recipient_after": recipient_after.to_string(),
            "broadcasts": broadcasts.load(std::sync::atomic::Ordering::SeqCst),
            "recovered_after_lost_response": true,
            "vault_handler_roundtrip": true,
            "encrypted_custody_signing": true,
            "outbound_receipt_reconciled": true,
            "payer_ledger_entries": payer_ledger.total,
            "payee_ledger_entries": payee_ledger.total,
            "settlement_requested_events": 1,
        })
    );
    server.abort();
    let _ = server.await;
    std::env::remove_var(payer_key_env);
    std::env::remove_var(kek_env);
}
