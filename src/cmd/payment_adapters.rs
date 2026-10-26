use crate::cache::ICache;
use crate::config::AppConfig;
use crate::config::ChainFamily;
use crate::config::ExternalProviderConfig;
use crate::config::MppAssetConfig;
use crate::config::SecretHeaderConfig;
use crate::config::X402AssetConfig;
use crate::payment::EvmAuthorizationSource;
use crate::payment::EvmReceiptRegistry;
use crate::payment::EvmRpcBalanceSource;
use crate::payment::EvmRpcReceiptConfig;
use crate::payment::EvmRpcReceiptSource;
use crate::payment::ProtocolRegistry;
use crate::payment::ReceiptSource;
use crate::payment::SettlementProviderConfig;
use crate::payment::SettlementProviderRegistry;
use crate::payment::SolanaRpcBalanceSource;
use crate::payment::SolanaRpcReceiptConfig;
use crate::payment::SolanaRpcReceiptSource;
use crate::payment::WalletBalanceRegistry;
use crate::payment::WalletBalanceSource;
use crate::payment::WalletImportService;
use crate::payment::X402FacilitatorProvider;
use crate::protocol::mpp::MppAdapter;
use crate::protocol::mpp::MppAsset;
use crate::protocol::x402::X402Adapter;
use crate::protocol::x402::X402Asset;
use crate::protocol::x402::X402SolanaConfig;
use crate::protocol::PaymentProtocolFactory;
use crate::storage::WalletKeyManager;
use crate::utils::secret::SecretReader;
use crate::utils::secure_material::Aes256GcmMaterialProtector;
use crate::utils::secure_material::SensitiveMaterialProtector;
use crate::Error;
use anyhow::Context as _;
use reqwest::{header::HeaderMap, header::HeaderName, header::HeaderValue};
use std::{collections::BTreeMap, sync::Arc};

const MAX_OPERATOR_SECRET_BYTES: usize = 16 * 1024;
pub(crate) struct PaymentAdapters {
    pub(crate) material_protector: Arc<dyn SensitiveMaterialProtector>,
    pub(crate) payer_protocols: Arc<ProtocolRegistry>,
    pub(crate) payee_protocols: Arc<ProtocolRegistry>,
    pub(crate) providers: Arc<SettlementProviderRegistry>,
    pub(crate) balances: Arc<dyn WalletBalanceSource>,
    pub(crate) receipts: Arc<EvmReceiptRegistry>,
}

impl PaymentAdapters {
    pub(crate) fn build_wallet_handler(
        &self,
        config: &AppConfig,
        cache: Arc<dyn ICache>,
        wallet_keys: Arc<dyn WalletKeyManager>,
    ) -> Arc<WalletImportService> {
        Arc::new(WalletImportService::new(
            cache,
            wallet_keys,
            Arc::clone(&self.material_protector),
            config.agentvault.key_management.import_session_ttl,
        ))
    }
}

pub(crate) struct RuntimeAdapterFactory;
impl RuntimeAdapterFactory {
    pub(crate) fn build_adapters(config: &AppConfig) -> anyhow::Result<PaymentAdapters> {
        let material_protector: Arc<dyn SensitiveMaterialProtector> = Arc::new(
            Aes256GcmMaterialProtector::from_config(&config.agentvault.recovery_encryption)
                .context("load recovery-encryption key ring")?,
        );
        Ok(PaymentAdapters {
            material_protector,
            payer_protocols: Arc::new(
                RuntimeAdapterFactory::build_payer_protocol_registry(config)
                    .context("build payer protocol registry")?,
            ),
            payee_protocols: Arc::new(
                RuntimeAdapterFactory::build_payee_protocol_registry(config)
                    .context("build payee protocol registry")?,
            ),
            providers: Arc::new(
                RuntimeAdapterFactory::build_provider_registry(config).context("build settlement provider registry")?,
            ),
            balances: Arc::new(
                RuntimeAdapterFactory::build_balance_registry(config).context("build chain balance-source registry")?,
            ),
            receipts: Arc::new(
                RuntimeAdapterFactory::build_receipt_registry(config).context("build chain receipt-source registry")?,
            ),
        })
    }

    pub(crate) fn build_payer_protocol_registry(config: &AppConfig) -> crate::Result<ProtocolRegistry> {
        let mut factory = PaymentProtocolFactory::new();
        let x402 = &config.agentvault.payer.x402;
        if x402.enabled {
            factory = factory.register(Arc::new(RuntimeAdapterFactory::x402_adapter(
                config,
                &x402.assets,
                x402.max_challenge_ttl.as_secs(),
            )?));
        }
        let mpp = &config.agentvault.payer.mpp;
        if mpp.enabled {
            factory = factory.register(Arc::new(MppAdapter::new_payer(
                RuntimeAdapterFactory::mpp_assets(&mpp.assets),
                mpp.max_challenge_ttl.as_secs(),
            )?));
        }
        factory.build()
    }

    pub(crate) fn build_payee_protocol_registry(config: &AppConfig) -> crate::Result<ProtocolRegistry> {
        let mut factory = PaymentProtocolFactory::new();
        let x402 = &config.agentvault.payee.x402;
        if x402.enabled {
            factory = factory.register(Arc::new(RuntimeAdapterFactory::x402_adapter(
                config,
                &x402.assets,
                x402.max_challenge_ttl.as_secs(),
            )?));
        }
        let mpp = &config.agentvault.payee.mpp;
        if mpp.enabled {
            let secret =
                SecretReader::read_ref(&mpp.challenge_secret, "MPP challenge secret", MAX_OPERATOR_SECRET_BYTES)?;
            factory = factory.register(Arc::new(MppAdapter::new_payee(
                RuntimeAdapterFactory::mpp_assets(&mpp.assets),
                mpp.realm.clone(),
                secret.to_string(),
                mpp.max_challenge_ttl.as_secs(),
            )?));
        }
        factory.build()
    }

    pub(crate) fn build_provider_registry(config: &AppConfig) -> crate::Result<SettlementProviderRegistry> {
        let mut bindings = Vec::new();
        let x402 = &config.agentvault.payee.x402;
        if x402.enabled {
            let settings = &x402.facilitator;
            let provider_id = settings.id.as_str();
            let headers = RuntimeAdapterFactory::secret_headers(
                &settings.secret_headers,
                &format!("settlement provider {provider_id}"),
            )?;
            let provider: Arc<dyn crate::payment::ExternalSettlementProvider> = Arc::new(X402FacilitatorProvider::new(
                &RuntimeAdapterFactory::settlement_provider_config(settings),
                headers,
                RuntimeAdapterFactory::x402_assets(&x402.assets),
            )?);
            bindings.push((
                "x402".to_owned(),
                provider_id.to_owned(),
                RuntimeAdapterFactory::confirmation_depths(
                    config,
                    x402.assets.iter().map(|asset| asset.network.as_str()),
                )?,
                provider,
            ));
        }
        let mpp = &config.agentvault.payee.mpp;
        if mpp.enabled {
            let settings = &mpp.provider;
            let provider_id = settings.id.as_str();
            let headers = RuntimeAdapterFactory::secret_headers(
                &settings.secret_headers,
                &format!("settlement provider {provider_id}"),
            )?;
            let provider: Arc<dyn crate::payment::ExternalSettlementProvider> =
                Arc::new(crate::payment::HttpSettlementProvider::new(
                    &RuntimeAdapterFactory::settlement_provider_config(settings),
                    headers,
                )?);
            bindings.push((
                "mpp".to_owned(),
                provider_id.to_owned(),
                RuntimeAdapterFactory::confirmation_depths(
                    config,
                    mpp.assets.iter().map(|asset| asset.network.as_str()),
                )?,
                provider,
            ));
        }
        SettlementProviderRegistry::new(bindings)
    }

    pub(crate) fn confirmation_depths<'a>(
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

    pub(crate) fn settlement_provider_config(provider: &ExternalProviderConfig) -> SettlementProviderConfig {
        SettlementProviderConfig {
            base_url: provider.base_url.clone(),
            verify_path: provider.verify_path.clone(),
            settle_path: provider.settle_path.clone(),
            timeout: provider.timeout,
            max_response_bytes: provider.max_response_bytes,
            allow_insecure_loopback: provider.allow_insecure_loopback,
        }
    }

    pub(crate) fn x402_assets(assets: &[X402AssetConfig]) -> Vec<X402Asset> {
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

    pub(crate) fn x402_adapter(
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
                rpc_headers: RuntimeAdapterFactory::resolved_header_values(
                    &chain.rpc.secret_headers,
                    &format!("Solana RPC {}", asset.network),
                )?,
                allow_insecure_loopback: chain.rpc.allow_insecure_loopback,
            });
        }
        X402Adapter::new_with_solana(RuntimeAdapterFactory::x402_assets(assets), max_timeout_seconds, solana)
    }

    pub(crate) fn mpp_assets(assets: &[MppAssetConfig]) -> Vec<MppAsset> {
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

    pub(crate) fn build_balance_registry(config: &AppConfig) -> crate::Result<WalletBalanceRegistry> {
        let mut sources = Vec::new();
        for (network, chain) in &config.agentvault.chains {
            let rpc = &chain.rpc;
            let headers =
                RuntimeAdapterFactory::resolved_header_values(&rpc.secret_headers, &format!("chain RPC {network}"))?;
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

    pub(crate) fn build_receipt_registry(config: &AppConfig) -> crate::Result<EvmReceiptRegistry> {
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
            let headers =
                RuntimeAdapterFactory::resolved_header_values(&rpc.secret_headers, &format!("chain RPC {network}"))?;
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

    pub(crate) fn secret_headers(
        headers: &BTreeMap<String, SecretHeaderConfig>,
        description: &str,
    ) -> crate::Result<HeaderMap> {
        let values = RuntimeAdapterFactory::resolved_header_values(headers, description)?;
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

    pub(crate) fn resolved_header_values(
        headers: &BTreeMap<String, SecretHeaderConfig>,
        description: &str,
    ) -> crate::Result<BTreeMap<String, String>> {
        headers
            .iter()
            .map(|(name, header)| {
                let secret = SecretReader::read_ref(
                    &header.source,
                    &format!("{description} header {name}"),
                    MAX_OPERATOR_SECRET_BYTES,
                )?;
                Ok((name.clone(), format!("{}{}", header.prefix, secret.as_str())))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/payment_adapters_test.rs"]
mod tests;
