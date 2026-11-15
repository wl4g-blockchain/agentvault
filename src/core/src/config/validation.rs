//! Conditional and cross-field runtime configuration constraints.
use super::{
    AgentVaultConfig, AppConfig, ApprovalJwtConfig, BTreeMap, CacheProvider, ChainFamily, Duration, Error,
    ExternalProviderConfig, IpAddr, MppAssetConfig, Path, RecoveryEncryptionConfig, RedisCacheMode, Result,
    SecretHeaderConfig, SecretRefConfig, StorageProvider, WalletKeyRingConfig, X402AssetConfig, MAX_CACHE_ENTRIES,
    MAX_CACHE_REDIS_ENDPOINTS, MAX_CACHE_RETRIES, MAX_CACHE_TTL, MAX_CACHE_VALUE_BYTES, MAX_CHALLENGE_TTL,
    MAX_HEALTH_PROBE_TIMEOUT, MAX_PROTOCOL_ASSETS, MAX_PROVIDER_RESPONSE_BYTES, MAX_RECONCILIATION_BATCH_SIZE,
    MAX_REQUEST_BYTES, MAX_RPC_RESPONSE_BYTES, MAX_STORAGE_CONNECTIONS,
};
use solana_pubkey::Pubkey;
use x402_chain_solana::chain::SolanaChainReference;
use x402_types::chain::ChainId;

impl AppConfig {
    /// Validates all listeners, protocol limits, policies, and observability settings.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe or internally inconsistent runtime configuration.
    pub fn validate_for_serve(&self) -> Result<()> {
        self.validate_storage()?;
        self.validate_cache()?;
        self.validate_agentvault()?;
        if self.server.service_name.trim().is_empty() || self.server.shutdown_timeout.is_zero() {
            return Err(Error::Config(
                "server.service_name and server.shutdown_timeout must be set".to_owned(),
            ));
        }
        if !self.server.enabled && !self.mgmt.enabled {
            return Err(Error::Config(
                "at least one HTTP or management listener must be enabled".to_owned(),
            ));
        }
        if self.server.enabled {
            ConfigValidation::validate_path("server.context_path", &self.server.context_path)?;
            if self.server.port == 0 {
                return Err(Error::Config("enabled API server has an invalid port".to_owned()));
            }
            if !(256..=MAX_REQUEST_BYTES).contains(&self.server.request.max_body_bytes)
                || self.server.request.timeout.is_zero()
            {
                return Err(Error::Config("server.request limits are invalid".to_owned()));
            }
        }
        if self.server.enabled && self.server.authguard.enabled {
            ConfigValidation::validate_secret_ref(
                "server.authguard.direct_context_hmac_key",
                &self.server.authguard.direct_context_hmac_key,
            )?;
            let resolver = &self.server.authguard.scope_resolver;
            if resolver.enabled
                && (resolver.target.trim().is_empty()
                    || !(resolver.target.starts_with("http://") || resolver.target.starts_with("https://"))
                    || resolver.timeout.is_zero())
            {
                return Err(Error::Config(
                    "server.authguard.scope_resolver configuration is invalid".to_owned(),
                ));
            }
        }
        self.validate_management()?;
        if self.mgmt.enabled
            && self.server.enabled
            && ConfigValidation::listener_addresses_overlap(
                self.mgmt.host,
                self.mgmt.port,
                self.server.host,
                self.server.port,
            )
        {
            return Err(Error::Config(
                "API and management listeners must use different addresses".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_agentvault(&self) -> Result<()> {
        ConfigValidation::validate_wallet_key_ring(&self.agentvault.key_management)?;
        let cache_ttl = match self.cache.provider {
            CacheProvider::Memory => self.cache.memory.default_ttl,
            CacheProvider::Redis => self.cache.redis.default_ttl,
        };
        if self.agentvault.key_management.import_session_ttl < Duration::from_secs(30)
            || self.agentvault.key_management.import_session_ttl > Duration::from_mins(5)
            || self.agentvault.key_management.import_session_ttl > cache_ttl
        {
            return Err(Error::Config(
                "agentvault.key_management.import_session_ttl must be between 30s and 5m and not exceed the selected cache default_ttl"
                    .to_owned(),
            ));
        }
        ConfigValidation::validate_recovery_encryption(&self.agentvault.recovery_encryption)?;
        self.validate_payer()?;
        self.validate_payee()?;
        self.validate_chains()?;
        self.validate_agentvault_runtime()
    }

    pub(super) fn validate_storage(&self) -> Result<()> {
        match self.storage.provider {
            StorageProvider::Sqlite => {
                let sqlite = &self.storage.sqlite;
                if sqlite.database_file.trim().is_empty()
                    || sqlite.database_file == ":memory:"
                    || sqlite.database_file.contains('\0')
                    || !Path::new(&sqlite.database_file).is_absolute()
                    || sqlite.max_connections == 0
                    || sqlite.max_connections > MAX_STORAGE_CONNECTIONS
                    || sqlite.busy_timeout.is_zero()
                {
                    return Err(Error::Config("storage.sqlite configuration is invalid".to_owned()));
                }
            }
            StorageProvider::Postgres => {
                let postgres = &self.storage.postgres;
                ConfigValidation::validate_secret_ref("storage.postgres.url", &postgres.url)?;
                if postgres.max_connections == 0
                    || postgres.max_connections > MAX_STORAGE_CONNECTIONS
                    || postgres.connect_timeout.is_zero()
                {
                    return Err(Error::Config(
                        "storage.postgres connection limits are invalid".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_cache(&self) -> Result<()> {
        match self.cache.provider {
            CacheProvider::Memory => {
                let memory = &self.cache.memory;
                if memory.max_entries == 0
                    || memory.max_entries > MAX_CACHE_ENTRIES
                    || memory.max_value_bytes == 0
                    || memory.max_value_bytes > MAX_CACHE_VALUE_BYTES
                    || memory.default_ttl.is_zero()
                    || memory.default_ttl > MAX_CACHE_TTL
                {
                    return Err(Error::Config("cache.memory configuration is invalid".to_owned()));
                }
            }
            CacheProvider::Redis => {
                let redis = &self.cache.redis;
                if redis.urls.is_empty()
                    || redis.urls.len() > MAX_CACHE_REDIS_ENDPOINTS
                    || (redis.mode == RedisCacheMode::Standalone && redis.urls.len() != 1)
                {
                    return Err(Error::Config(
                        "cache.redis URL count is invalid for its mode".to_owned(),
                    ));
                }
                for (index, url) in redis.urls.iter().enumerate() {
                    ConfigValidation::validate_secret_ref(&format!("cache.redis.urls.{index}"), url)?;
                    if !url.file.is_empty() || url.env.is_empty() {
                        return Err(Error::Config("cache.redis URLs must use SecretRef.env".to_owned()));
                    }
                }
                if redis.key_prefix.trim().is_empty()
                    || redis.key_prefix.len() > 128
                    || redis.key_prefix.starts_with(':')
                    || redis.key_prefix.ends_with(':')
                    || !redis
                        .key_prefix
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-'))
                    || redis.max_value_bytes == 0
                    || redis.max_value_bytes > MAX_CACHE_VALUE_BYTES
                    || redis.connect_timeout.is_zero()
                    || redis.operation_timeout.is_zero()
                    || redis.default_ttl.is_zero()
                    || redis.default_ttl > MAX_CACHE_TTL
                    || redis.retry_attempts > MAX_CACHE_RETRIES
                    || redis.min_retry_wait.is_zero()
                    || redis.max_retry_wait < redis.min_retry_wait
                {
                    return Err(Error::Config("cache.redis configuration is invalid".to_owned()));
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_payer(&self) -> Result<()> {
        let payer = &self.agentvault.payer;
        let mut enabled_adapters = 0_u8;
        ConfigValidation::validate_challenge_ttl("agentvault.payer.x402", payer.x402.max_challenge_ttl)?;
        if payer.x402.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            ConfigValidation::validate_x402_assets("agentvault.payer.x402.assets", &payer.x402.assets)?;
        }
        ConfigValidation::validate_challenge_ttl("agentvault.payer.mpp", payer.mpp.max_challenge_ttl)?;
        if payer.mpp.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            ConfigValidation::validate_mpp_assets("agentvault.payer.mpp.assets", &payer.mpp.assets)?;
        }
        ConfigValidation::validate_direction_enabled("payer", payer.enabled, enabled_adapters)
    }

    pub(super) fn validate_payee(&self) -> Result<()> {
        let payee = &self.agentvault.payee;
        let mut enabled_adapters = 0_u8;
        ConfigValidation::validate_challenge_ttl("agentvault.payee.x402", payee.x402.max_challenge_ttl)?;
        if payee.x402.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            ConfigValidation::validate_x402_assets("agentvault.payee.x402.assets", &payee.x402.assets)?;
            ConfigValidation::validate_external_provider("agentvault.payee.x402.facilitator", &payee.x402.facilitator)?;
        }
        ConfigValidation::validate_challenge_ttl("agentvault.payee.mpp", payee.mpp.max_challenge_ttl)?;
        if payee.mpp.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            if payee.mpp.realm.trim().is_empty()
                || payee.mpp.realm.len() > 256
                || payee.mpp.realm.chars().any(char::is_control)
            {
                return Err(Error::Config("agentvault.payee.mpp.realm is invalid".to_owned()));
            }
            ConfigValidation::validate_secret_ref(
                "agentvault.payee.mpp.challenge_secret",
                &payee.mpp.challenge_secret,
            )?;
            ConfigValidation::validate_mpp_assets("agentvault.payee.mpp.assets", &payee.mpp.assets)?;
            ConfigValidation::validate_external_provider("agentvault.payee.mpp.provider", &payee.mpp.provider)?;
        }
        ConfigValidation::validate_direction_enabled("payee", payee.enabled, enabled_adapters)
    }

    pub(super) fn validate_chains(&self) -> Result<()> {
        for asset in ConfigValidation::enabled_x402_assets(&self.agentvault) {
            let chain = self.agentvault.chains.get(&asset.network).ok_or_else(|| {
                Error::Config(format!(
                    "agentvault.chains has no fixed endpoint for enabled network {}",
                    asset.network
                ))
            })?;
            let expected = if ConfigValidation::is_evm_network(&asset.network) {
                ChainFamily::Evm
            } else {
                ChainFamily::Solana
            };
            if chain.family != expected {
                return Err(Error::Config(format!(
                    "x402 network {} does not match its configured chain family",
                    asset.network
                )));
            }
        }
        for network in ConfigValidation::enabled_mpp_networks(&self.agentvault) {
            let chain = self.agentvault.chains.get(network).ok_or_else(|| {
                Error::Config(format!(
                    "agentvault.chains has no fixed endpoint for enabled network {network}"
                ))
            })?;
            if chain.family != ChainFamily::Evm {
                return Err(Error::Config(format!(
                    "enabled MPP Tempo network {network} must use an EVM chain"
                )));
            }
        }
        for (network, chain) in &self.agentvault.chains {
            ConfigValidation::validate_chain_id(network, chain.family)?;
            ConfigValidation::validate_operator_endpoint(
                &format!("agentvault.chains.{network}.rpc.endpoint"),
                &chain.rpc.endpoint,
                chain.rpc.allow_insecure_loopback,
            )?;
            if chain.rpc.timeout.is_zero()
                || chain.rpc.max_response_bytes == 0
                || chain.rpc.max_response_bytes > MAX_RPC_RESPONSE_BYTES
                || chain.confirmation_depth == 0
                || chain.confirmation_depth > 10_000
            {
                return Err(Error::Config(format!(
                    "agentvault.chains.{network} request or confirmation limits are invalid"
                )));
            }
            ConfigValidation::validate_secret_headers(
                &format!("agentvault.chains.{network}.rpc"),
                &chain.rpc.secret_headers,
            )?;
        }
        Ok(())
    }

    pub(super) fn validate_agentvault_runtime(&self) -> Result<()> {
        if self.agentvault.approval.request_ttl.is_zero()
            || self.agentvault.approval.grant_ttl.is_zero()
            || self.agentvault.approval.grant_ttl > self.agentvault.approval.request_ttl
            || self.agentvault.approval.expiry_interval.is_zero()
            || self.agentvault.approval.expiry_batch_size == 0
            || self.agentvault.approval.expiry_batch_size > MAX_RECONCILIATION_BATCH_SIZE
        {
            return Err(Error::Config(
                "agentvault.approval TTL or expiration scheduling limits are invalid".to_owned(),
            ));
        }
        ConfigValidation::validate_approval_jwt(&self.agentvault.approval.jwt)?;

        let reconciliation = &self.agentvault.reconciliation;
        if reconciliation.interval.is_zero()
            || reconciliation.lease_ttl < reconciliation.interval
            || reconciliation.batch_size == 0
            || reconciliation.batch_size > MAX_RECONCILIATION_BATCH_SIZE
            || reconciliation.initial_backoff.is_zero()
            || reconciliation.max_backoff < reconciliation.initial_backoff
        {
            return Err(Error::Config(
                "agentvault.reconciliation scheduling limits are invalid".to_owned(),
            ));
        }
        if self.agentvault.enabled() && !reconciliation.enabled {
            return Err(Error::Config(
                "agentvault.reconciliation must be enabled when payer or payee is enabled".to_owned(),
            ));
        }
        let balance_sync = &self.agentvault.wallet_balance_sync;
        if balance_sync.interval.is_zero()
            || balance_sync.batch_size == 0
            || balance_sync.batch_size > MAX_RECONCILIATION_BATCH_SIZE
        {
            return Err(Error::Config(
                "agentvault.wallet_balance_sync scheduling limits are invalid".to_owned(),
            ));
        }
        if self.agentvault.payer.enabled && !balance_sync.enabled {
            return Err(Error::Config(
                "agentvault.wallet_balance_sync must be enabled when payer is enabled".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_management(&self) -> Result<()> {
        if self.mgmt.enabled {
            if self.mgmt.port == 0 {
                return Err(Error::Config("mgmt.port must not be zero".to_owned()));
            }
            ConfigValidation::validate_path("mgmt.context_path", &self.mgmt.context_path)?;
            ConfigValidation::validate_path("mgmt.health.liveness_path", &self.mgmt.health.liveness_path)?;
            ConfigValidation::validate_path("mgmt.health.readiness_path", &self.mgmt.health.readiness_path)?;
            if self.mgmt.health.probe_timeout.is_zero() || self.mgmt.health.probe_timeout > MAX_HEALTH_PROBE_TIMEOUT {
                return Err(Error::Config(
                    "mgmt.health.probe_timeout must be between 1ns and 30s".to_owned(),
                ));
            }
            ConfigValidation::validate_path("mgmt.metrics.path", &self.mgmt.metrics.path)?;
            let mut routes = vec![
                self.mgmt.health.liveness_path.as_str(),
                self.mgmt.health.readiness_path.as_str(),
                "/debug/runtime",
            ];
            if self.mgmt.metrics.enabled {
                routes.push(self.mgmt.metrics.path.as_str());
            }
            routes.sort_unstable();
            if routes.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(Error::Config(
                    "management health, metrics, and runtime routes must be distinct".to_owned(),
                ));
            }
        }
        if self.mgmt.otel.enabled
            && (self.mgmt.otel.endpoint.trim().is_empty()
                || !self.mgmt.otel.protocol.eq_ignore_ascii_case("grpc")
                || self.mgmt.otel.timeout.is_zero()
                || !(0.0..=1.0).contains(&self.mgmt.otel.sample_rate))
        {
            return Err(Error::Config("mgmt.otel configuration is invalid".to_owned()));
        }
        Ok(())
    }
}

pub(super) struct ConfigValidation;
impl ConfigValidation {
    pub(super) fn listener_addresses_overlap(left: IpAddr, left_port: u16, right: IpAddr, right_port: u16) -> bool {
        if left_port != right_port {
            return false;
        }
        let left = left.to_canonical();
        let right = right.to_canonical();
        if left == right {
            return true;
        }
        match (left, right) {
            (IpAddr::V4(left), IpAddr::V4(right)) => left.is_unspecified() || right.is_unspecified(),
            (IpAddr::V6(left), IpAddr::V6(right)) => left.is_unspecified() || right.is_unspecified(),
            // The listener does not set IPV6_V6ONLY: an IPv6 wildcard can
            // also bind IPv4, so configuration must reject this overlap.
            (IpAddr::V6(ipv6), IpAddr::V4(_)) | (IpAddr::V4(_), IpAddr::V6(ipv6)) => ipv6.is_unspecified(),
        }
    }

    pub(super) fn validate_wallet_key_ring(key_ring: &WalletKeyRingConfig) -> Result<()> {
        Self::validate_config_identifier("agentvault.key_management.active_key_id", &key_ring.active_key_id)?;
        if key_ring.keys.is_empty() || key_ring.keys.len() > 16 || !key_ring.keys.contains_key(&key_ring.active_key_id)
        {
            return Err(Error::Config(
                "agentvault.key_management Wallet KEK ring is invalid".to_owned(),
            ));
        }
        for (key_id, reference) in &key_ring.keys {
            Self::validate_config_identifier("agentvault.key_management key id", key_id)?;
            if !reference.file.is_empty() || reference.env.trim().is_empty() {
                return Err(Error::Config(format!(
                    "agentvault.key_management.keys.{key_id} must use an environment reference"
                )));
            }
            Self::validate_secret_ref(&format!("agentvault.key_management.keys.{key_id}"), reference)?;
        }
        Ok(())
    }

    pub(super) fn validate_recovery_encryption(key_ring: &RecoveryEncryptionConfig) -> Result<()> {
        Self::validate_config_identifier("agentvault.recovery_encryption.active_key_id", &key_ring.active_key_id)?;
        if key_ring.keys.is_empty() || key_ring.keys.len() > 16 || !key_ring.keys.contains_key(&key_ring.active_key_id)
        {
            return Err(Error::Config(
                "agentvault.recovery_encryption key ring is invalid".to_owned(),
            ));
        }
        for (key_id, reference) in &key_ring.keys {
            Self::validate_config_identifier("agentvault.recovery_encryption key id", key_id)?;
            Self::validate_secret_ref(&format!("agentvault.recovery_encryption.keys.{key_id}"), reference)?;
        }
        Ok(())
    }

    pub(super) fn validate_challenge_ttl(field: &str, max_challenge_ttl: Duration) -> Result<()> {
        if max_challenge_ttl.is_zero() || max_challenge_ttl > MAX_CHALLENGE_TTL {
            return Err(Error::Config(format!("{field}.max_challenge_ttl is invalid")));
        }
        Ok(())
    }

    pub(super) fn validate_direction_enabled(direction: &str, enabled: bool, enabled_adapters: u8) -> Result<()> {
        if enabled && enabled_adapters == 0 {
            return Err(Error::Config(format!(
                "agentvault.{direction}.enabled requires at least one enabled adapter"
            )));
        }
        if !enabled && enabled_adapters != 0 {
            return Err(Error::Config(format!(
                "agentvault.{direction} adapters must be disabled when the direction is disabled"
            )));
        }
        Ok(())
    }

    pub(super) fn validate_x402_assets(field: &str, assets: &[X402AssetConfig]) -> Result<()> {
        if assets.is_empty() || assets.len() > MAX_PROTOCOL_ASSETS {
            return Err(Error::Config(format!(
                "{field} must contain a bounded trusted registry"
            )));
        }
        for (index, asset) in assets.iter().enumerate() {
            let valid_chain_fields = if Self::is_evm_network(&asset.network) {
                Self::valid_canonical_evm_address(&asset.contract) && asset.fee_payer.is_none()
            } else if Self::is_solana_network(&asset.network) {
                asset.contract.parse::<Pubkey>().is_ok()
                    && asset
                        .fee_payer
                        .as_deref()
                        .is_some_and(|fee_payer| fee_payer.parse::<Pubkey>().is_ok())
            } else {
                false
            };
            if !valid_chain_fields
                || asset.symbol != "USDC"
                || asset.decimals != 6
                || assets[..index]
                    .iter()
                    .any(|prior| prior.network == asset.network && prior.contract == asset.contract)
            {
                return Err(Error::Config(format!(
                    "{field} contains an invalid or duplicate x402 USDC asset"
                )));
            }
        }
        Ok(())
    }

    pub(super) fn validate_mpp_assets(field: &str, assets: &[MppAssetConfig]) -> Result<()> {
        if assets.is_empty() || assets.len() > MAX_PROTOCOL_ASSETS {
            return Err(Error::Config(format!(
                "{field} must contain a bounded trusted registry"
            )));
        }
        for (index, asset) in assets.iter().enumerate() {
            Self::validate_evm_network(&format!("{field}.network"), &asset.network)?;
            if !Self::valid_canonical_evm_address(&asset.currency)
                || asset.symbol.trim().is_empty()
                || asset
                    .token_contract
                    .as_deref()
                    .is_some_and(|value| value != asset.currency)
                || assets[..index]
                    .iter()
                    .any(|prior| prior.network == asset.network && prior.currency == asset.currency)
            {
                return Err(Error::Config(format!(
                    "{field} contains an invalid or duplicate Tempo asset"
                )));
            }
        }
        Ok(())
    }

    pub(super) fn enabled_x402_assets(config: &AgentVaultConfig) -> Vec<&X402AssetConfig> {
        let mut assets = Vec::new();
        if config.payer.enabled && config.payer.x402.enabled {
            assets.extend(&config.payer.x402.assets);
        }
        if config.payee.enabled && config.payee.x402.enabled {
            assets.extend(&config.payee.x402.assets);
        }
        assets
    }

    pub(super) fn enabled_mpp_networks(config: &AgentVaultConfig) -> Vec<&str> {
        let mut networks = Vec::new();
        if config.payer.enabled && config.payer.mpp.enabled {
            networks.extend(config.payer.mpp.assets.iter().map(|asset| asset.network.as_str()));
        }
        if config.payee.enabled && config.payee.mpp.enabled {
            networks.extend(config.payee.mpp.assets.iter().map(|asset| asset.network.as_str()));
        }
        networks
    }

    pub(super) fn validate_chain_id(network: &str, family: ChainFamily) -> Result<()> {
        match family {
            ChainFamily::Evm => Self::validate_evm_network("agentvault.chains network", network),
            ChainFamily::Solana => {
                let valid = Self::is_solana_network(network);
                if valid {
                    Ok(())
                } else {
                    Err(Error::Config(
                        "agentvault.chains Solana network must be a canonical CAIP-2 identifier".to_owned(),
                    ))
                }
            }
        }
    }

    pub(super) fn is_evm_network(network: &str) -> bool {
        network
            .strip_prefix("eip155:")
            .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
    }

    pub(super) fn is_solana_network(network: &str) -> bool {
        network
            .parse::<ChainId>()
            .ok()
            .and_then(|chain| SolanaChainReference::try_from(chain).ok())
            .is_some()
    }

    pub(super) fn validate_evm_network(field: &str, network: &str) -> Result<()> {
        if !Self::is_evm_network(network) {
            return Err(Error::Config(format!("{field} must be a canonical EIP-155 network")));
        }
        Ok(())
    }

    pub(super) fn valid_canonical_evm_address(value: &str) -> bool {
        value.len() == 42
            && value.starts_with("0x")
            && value == value.to_ascii_lowercase()
            && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    pub(super) fn validate_operator_endpoint(field: &str, value: &str, allow_insecure_loopback: bool) -> Result<()> {
        let url = reqwest::Url::parse(value).map_err(|_| Error::Config(format!("{field} is invalid")))?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "https" | "http")
            || (url.scheme() == "http"
                && (!allow_insecure_loopback || !url.host_str().is_some_and(Self::is_loopback_host)))
        {
            return Err(Error::Config(format!("{field} is unsafe")));
        }
        Ok(())
    }

    pub(super) fn validate_secret_headers(field: &str, headers: &BTreeMap<String, SecretHeaderConfig>) -> Result<()> {
        if headers.len() > 16 {
            return Err(Error::Config(format!("{field} has too many secret headers")));
        }
        for (header_name, header) in headers {
            let normalized = header_name.to_ascii_lowercase();
            if reqwest::header::HeaderName::from_bytes(header_name.as_bytes()).is_err()
                || matches!(
                    normalized.as_str(),
                    "connection"
                        | "content-length"
                        | "host"
                        | "idempotency-key"
                        | "payment-signature"
                        | "traceparent"
                        | "tracestate"
                        | "transfer-encoding"
                )
            {
                return Err(Error::Config(format!("{field} has an unsafe secret header name")));
            }
            Self::validate_secret_ref(&format!("{field}.secret_headers.{header_name}.source"), &header.source)?;
            if header.prefix.len() > 64
                || !header
                    .prefix
                    .bytes()
                    .all(|byte| byte == b' ' || byte.is_ascii_graphic())
            {
                return Err(Error::Config(format!("{field} has an unsafe secret header prefix")));
            }
        }
        Ok(())
    }

    pub(super) fn validate_approval_jwt(jwt: &ApprovalJwtConfig) -> Result<()> {
        if jwt.issuer.trim().is_empty()
            || jwt.issuer.len() > 256
            || jwt.issuer.chars().any(char::is_control)
            || jwt.audience.trim().is_empty()
            || jwt.audience.len() > 256
            || jwt.audience.chars().any(char::is_control)
            || jwt.signing_keys.is_empty()
            || jwt.signing_keys.len() > 16
            || !jwt.signing_keys.contains_key(&jwt.active_key_id)
        {
            return Err(Error::Config(
                "agentvault.approval.jwt identity or key ring is invalid".to_owned(),
            ));
        }
        Self::validate_config_identifier("agentvault.approval.jwt.active_key_id", &jwt.active_key_id)?;
        for (key_id, secret) in &jwt.signing_keys {
            Self::validate_config_identifier("agentvault.approval.jwt.signing_keys key id", key_id)?;
            Self::validate_secret_ref(&format!("agentvault.approval.jwt.signing_keys.{key_id}"), secret)?;
        }
        Ok(())
    }

    pub(super) fn validate_path(field: &str, value: &str) -> Result<()> {
        if !value.starts_with('/') || (value.len() > 1 && value.ends_with('/')) || value.contains("//") {
            return Err(Error::Config(format!("{field} must be an absolute normalized path")));
        }
        Ok(())
    }

    pub(super) fn validate_secret_ref(field: &str, secret: &SecretRefConfig) -> Result<()> {
        let has_file = !secret.file.trim().is_empty();
        let has_env = !secret.env.trim().is_empty();
        if has_file == has_env {
            return Err(Error::Config(format!("{field} must set exactly one of file or env")));
        }
        if has_file && (!Path::new(&secret.file).is_absolute() || secret.file.contains('\0')) {
            return Err(Error::Config(format!("{field}.file must be an absolute safe path")));
        }
        if has_env && !Self::is_environment_name(&secret.env) {
            return Err(Error::Config(format!(
                "{field}.env is not a valid environment variable name"
            )));
        }
        Ok(())
    }

    pub(super) fn is_environment_name(value: &str) -> bool {
        let mut bytes = value.bytes();
        bytes
            .next()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
            && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    }

    pub(super) fn validate_config_identifier(field: &str, value: &str) -> Result<()> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        {
            return Err(Error::Config(format!("{field} is invalid")));
        }
        Ok(())
    }

    pub(super) fn validate_external_provider(field: &str, provider: &ExternalProviderConfig) -> Result<()> {
        Self::validate_config_identifier(&format!("{field}.id"), &provider.id)?;
        let url = reqwest::Url::parse(&provider.base_url)
            .map_err(|error| Error::Config(format!("{field}.base_url is invalid: {error}")))?;
        if url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
        {
            return Err(Error::Config(format!(
                "{field}.base_url must not contain credentials, query, or fragment"
            )));
        }
        match url.scheme() {
            "https" => {}
            "http" if provider.allow_insecure_loopback && url.host_str().is_some_and(Self::is_loopback_host) => {}
            _ => {
                return Err(Error::Config(format!(
                    "{field} must use HTTPS; HTTP is limited to explicit loopback development"
                )));
            }
        }
        Self::validate_provider_operation_path(field, "verify_path", &provider.verify_path)?;
        Self::validate_provider_operation_path(field, "settle_path", &provider.settle_path)?;
        if provider.timeout.is_zero()
            || provider.max_response_bytes == 0
            || provider.max_response_bytes > MAX_PROVIDER_RESPONSE_BYTES
            || provider.secret_headers.len() > 16
        {
            return Err(Error::Config(format!("{field} request limits are invalid")));
        }
        Self::validate_secret_headers(field, &provider.secret_headers)?;
        Ok(())
    }

    pub(super) fn validate_provider_operation_path(provider_field: &str, field: &str, value: &str) -> Result<()> {
        if value.is_empty()
            || value.starts_with('/')
            || value.contains(['?', '#'])
            || value
                .split('/')
                .any(|part| part.is_empty() || part == ".." || part == ".")
        {
            return Err(Error::Config(format!("{provider_field}.{field} is unsafe")));
        }
        Ok(())
    }

    pub(super) fn is_loopback_host(host: &str) -> bool {
        host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|address| address.is_loopback())
    }
}

#[cfg(test)]
#[path = "../../tests/config/validation_test.rs"]
mod tests;
