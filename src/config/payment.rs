use super::{
    Amount, BTreeMap, Deserialize, Duration, IpAddr, SecretRefConfig, Serialize, WalletSelectionStrategy,
    DEFAULT_APPROVAL_JWT_AUDIENCE, DEFAULT_APPROVAL_JWT_ISSUER, DEFAULT_APPROVAL_JWT_KEY_ID, DEFAULT_MQTT_TOPIC_PREFIX,
};

/// Enterprise `AgentVault` runtime configuration.
///
/// Business authorization, budget, recipient, network, asset, and pricing
/// rules deliberately live in the persistent Policy and Price stores rather
/// than process configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentVaultConfig {
    pub payer: PayerConfig,
    pub payee: PayeeConfig,
    /// Shared operator-owned chain registry keyed by canonical CAIP-2 network.
    pub chains: BTreeMap<String, ChainConfig>,
    /// Versioned KEKs used only to envelope-encrypt persisted Wallet EOA keys.
    pub key_management: WalletKeyRingConfig,
    /// Independent application encryption for short-lived durable recovery
    /// material. This never contains or encrypts Wallet EOA private keys.
    pub recovery_encryption: RecoveryEncryptionConfig,
    pub approval: ApprovalRuntimeConfig,
    pub reconciliation: ReconciliationConfig,
    pub wallet_balance_sync: WalletBalanceSyncConfig,
    pub transport: TransportConfig,
}

impl AgentVaultConfig {
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.payer.enabled || self.payee.enabled
    }
}

/// Outbound payment adapters. A payer consumes remote challenges and signs
/// locally; it does not configure the merchant's facilitator/provider.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayerConfig {
    pub enabled: bool,
    pub wallet_selection: WalletSelectionConfig,
    pub x402: PayerX402Config,
    pub mpp: PayerMppConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletSelectionConfig {
    pub strategy: WalletSelectionStrategy,
}

/// Inbound payment adapters. Each provider is colocated with the protocol
/// whose credentials it verifies and settles.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayeeConfig {
    pub enabled: bool,
    pub x402: PayeeX402Config,
    pub mpp: PayeeMppConfig,
}

/// Indirect references for versioned Wallet key-encryption keys. EOA private
/// keys themselves are encrypted in the SQL Wallet aggregate.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletKeyRingConfig {
    pub active_key_id: String,
    pub keys: BTreeMap<String, SecretRefConfig>,
    /// Lifetime of a one-use browser-to-Vault wallet-key import session.
    #[serde(with = "humantime_serde")]
    pub import_session_ttl: Duration,
}

/// Versioned keys protecting durable credential and approval recovery
/// envelopes. These keys are deliberately distinct from Wallet KEKs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecoveryEncryptionConfig {
    pub active_key_id: String,
    pub keys: BTreeMap<String, SecretRefConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayerX402Config {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub assets: Vec<X402AssetConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayeeX402Config {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub assets: Vec<X402AssetConfig>,
    pub facilitator: ExternalProviderConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct X402AssetConfig {
    pub network: String,
    pub contract: String,
    pub symbol: String,
    pub decimals: u8,
    pub fee_payer: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayerMppConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub assets: Vec<MppAssetConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayeeMppConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub realm: String,
    pub challenge_secret: SecretRefConfig,
    pub assets: Vec<MppAssetConfig>,
    pub provider: ExternalProviderConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MppAssetConfig {
    pub network: String,
    pub currency: String,
    pub symbol: String,
    pub token_contract: Option<String>,
    pub decimals: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChainFamily {
    #[default]
    Evm,
    Solana,
}

/// One shared chain boundary used by payer balance/signing checks and payee
/// settlement reconciliation. Protocol challenges cannot override it.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChainConfig {
    pub family: ChainFamily,
    pub rpc: ChainRpcConfig,
    pub minimum_native_balance: Amount,
    pub confirmation_depth: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChainRpcConfig {
    pub endpoint: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub allow_insecure_loopback: bool,
    pub secret_headers: BTreeMap<String, SecretHeaderConfig>,
}

/// HTTP configuration for a hosted facilitator or payment provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExternalProviderConfig {
    pub id: String,
    pub base_url: String,
    pub verify_path: String,
    pub settle_path: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub max_response_bytes: usize,
    /// Plain HTTP is limited to an explicitly configured loopback endpoint.
    pub allow_insecure_loopback: bool,
    /// Header values are indirect secret references; literal values are not
    /// representable by this configuration model.
    pub secret_headers: BTreeMap<String, SecretHeaderConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretHeaderConfig {
    pub source: SecretRefConfig,
    /// Non-secret scheme prefix such as `Bearer `.
    pub prefix: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalRuntimeConfig {
    #[serde(with = "humantime_serde")]
    pub request_ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub grant_ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub expiry_interval: Duration,
    pub expiry_batch_size: usize,
    pub jwt: ApprovalJwtConfig,
}

/// Internal one-time approval JWT configuration.
///
/// Signing key bytes are never represented inline. Each key is loaded from an
/// indirect secret reference, and retired key identifiers remain configured
/// until every JWT they signed has expired or been consumed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalJwtConfig {
    pub issuer: String,
    pub audience: String,
    pub active_key_id: String,
    pub signing_keys: BTreeMap<String, SecretRefConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReconciliationConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub lease_ttl: Duration,
    pub batch_size: usize,
    #[serde(with = "humantime_serde")]
    pub initial_backoff: Duration,
    #[serde(with = "humantime_serde")]
    pub max_backoff: Duration,
}

/// Periodic, non-authoritative observations of managed wallet balances.
/// Authorization and signing never trust these snapshots; they remain an
/// operations, capacity, and alerting data set.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletBalanceSyncConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    pub batch_size: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransportConfig {
    pub grpc: GrpcTransportConfig,
    pub mqtt: MqttTransportConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrpcTransportConfig {
    pub enabled: bool,
    pub host: IpAddr,
    pub port: u16,
    pub max_message_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttTransportConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub consumer_group: String,
    pub topic_prefix: String,
    pub qos: u8,
    pub tls: bool,
    pub username: String,
    pub password: SecretRefConfig,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub reconnect_delay: Duration,
}

impl Default for WalletKeyRingConfig {
    fn default() -> Self {
        Self {
            active_key_id: "wallet-kek-v1".to_owned(),
            keys: BTreeMap::from([(
                "wallet-kek-v1".to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT__AGENTVAULT__KEY_MANAGEMENT__KEYS__WALLET_KEK_V1".to_owned(),
                },
            )]),
            import_session_ttl: Duration::from_mins(2),
        }
    }
}

impl Default for RecoveryEncryptionConfig {
    fn default() -> Self {
        Self {
            active_key_id: "recovery-v1".to_owned(),
            keys: BTreeMap::from([(
                "recovery-v1".to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT__AGENTVAULT__RECOVERY_ENCRYPTION__KEYS__RECOVERY_V1".to_owned(),
                },
            )]),
        }
    }
}

impl Default for PayerX402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            assets: Vec::new(),
        }
    }
}

impl Default for PayeeX402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            assets: Vec::new(),
            facilitator: ExternalProviderConfig::default(),
        }
    }
}

impl Default for X402AssetConfig {
    fn default() -> Self {
        Self {
            network: String::new(),
            contract: String::new(),
            symbol: "USDC".to_owned(),
            decimals: 6,
            fee_payer: None,
        }
    }
}

impl Default for PayerMppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            assets: Vec::new(),
        }
    }
}

impl Default for PayeeMppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            realm: String::new(),
            challenge_secret: SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT__AGENTVAULT__PAYEE__MPP__CHALLENGE_SECRET".to_owned(),
            },
            assets: Vec::new(),
            provider: ExternalProviderConfig::default(),
        }
    }
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            family: ChainFamily::default(),
            rpc: ChainRpcConfig::default(),
            minimum_native_balance: Amount::ZERO,
            confirmation_depth: 1,
        }
    }
}

impl Default for ChainRpcConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            timeout: Duration::from_secs(5),
            max_response_bytes: 256 * 1024,
            allow_insecure_loopback: false,
            secret_headers: BTreeMap::new(),
        }
    }
}

impl Default for ExternalProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            base_url: String::new(),
            verify_path: "verify".to_owned(),
            settle_path: "settle".to_owned(),
            timeout: Duration::from_secs(10),
            max_response_bytes: 256 * 1024,
            allow_insecure_loopback: false,
            secret_headers: BTreeMap::new(),
        }
    }
}

impl Default for ApprovalRuntimeConfig {
    fn default() -> Self {
        Self {
            request_ttl: Duration::from_mins(15),
            grant_ttl: Duration::from_mins(5),
            expiry_interval: Duration::from_secs(15),
            expiry_batch_size: 100,
            jwt: ApprovalJwtConfig::default(),
        }
    }
}

impl Default for ApprovalJwtConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_APPROVAL_JWT_ISSUER.to_owned(),
            audience: DEFAULT_APPROVAL_JWT_AUDIENCE.to_owned(),
            active_key_id: DEFAULT_APPROVAL_JWT_KEY_ID.to_owned(),
            signing_keys: BTreeMap::from([(
                DEFAULT_APPROVAL_JWT_KEY_ID.to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT__AGENTVAULT__APPROVAL__JWT__SIGNING_KEYS__APPROVAL_HS256_V1".to_owned(),
                },
            )]),
        }
    }
}

impl Default for ReconciliationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(15),
            lease_ttl: Duration::from_secs(45),
            batch_size: 100,
            initial_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_mins(5),
        }
    }
}

impl Default for WalletBalanceSyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_mins(3),
            batch_size: 1_000,
        }
    }
}

impl Default for GrpcTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: IpAddr::from([127, 0, 0, 1]),
            port: 50051,
            max_message_bytes: 2 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl Default for MqttTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: 1883,
            client_id: "agentvault".to_owned(),
            consumer_group: "agentvault-pool".to_owned(),
            topic_prefix: DEFAULT_MQTT_TOPIC_PREFIX.to_owned(),
            qos: 1,
            tls: false,
            username: String::new(),
            password: SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT__AGENTVAULT__TRANSPORT__MQTT__PASSWORD".to_owned(),
            },
            request_timeout: Duration::from_secs(10),
            reconnect_delay: Duration::from_secs(1),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/config/payment_test.rs"]
mod tests;
