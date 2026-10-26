use super::{
    AgentVaultConfig, ApprovalJwtConfig, BTreeMap, ChainFamily, ChainId, Duration, Error, ExternalProviderConfig,
    IpAddr, MppAssetConfig, Path, Pubkey, RecoveryEncryptionConfig, Result, SecretHeaderConfig, SecretRefConfig,
    SolanaChainReference, WalletKeyRingConfig, X402AssetConfig, MAX_CHALLENGE_TTL, MAX_PROTOCOL_ASSETS,
    MAX_PROVIDER_RESPONSE_BYTES,
};

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

    pub(super) fn valid_topic_prefix(value: &str) -> bool {
        !value.trim().is_empty()
            && value.len() <= 256
            && !value.starts_with('/')
            && !value.ends_with('/')
            && !value.contains(['+', '#', '\0'])
            && value.split('/').all(|segment| !segment.is_empty())
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
