use super::*;

#[test]
fn rejects_config_without_a_data_plane() {
    let mut config = AppConfig::default();
    config.server.enabled = false;
    config.mgmt.enabled = false;
    assert!(config.validate_for_serve().is_err());
}

#[test]
fn validates_optional_authguard_context_key() {
    let mut config = AppConfig::default();
    config.server.enabled = true;
    assert!(config.validate_for_serve().is_ok());

    config.server.authguard.direct_context_hmac_key = SecretRefConfig::default();
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("direct_context_hmac_key")
    ));

    config.server.authguard.enabled = false;
    assert!(config.validate_for_serve().is_ok());
}

#[test]
fn validates_authguard_scope_resolver_configuration() {
    let mut config = AppConfig::default();
    config.server.authguard.scope_resolver.target = "authguard:8081".to_owned();
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("scope_resolver")
    ));

    config.server.authguard.scope_resolver.target = "http://127.0.0.1:8081".to_owned();
    config.server.authguard.scope_resolver.timeout = Duration::ZERO;
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("scope_resolver")
    ));
}

#[test]
fn validates_authguard_when_grpc_is_the_only_edge_transport() {
    let mut config = AppConfig::default();
    config.server.enabled = false;
    config.mgmt.enabled = false;
    config.agentvault.transport.grpc.enabled = true;
    config.server.authguard.direct_context_hmac_key = SecretRefConfig::default();
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("direct_context_hmac_key")
    ));
}

#[test]
fn rejects_listener_address_collisions() {
    let mut config = AppConfig::default();
    config.agentvault.transport.grpc.enabled = true;
    config.agentvault.transport.grpc.host = config.server.host;
    config.agentvault.transport.grpc.port = config.server.port;
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("listeners")
    ));
}

#[test]
fn rejects_management_route_collisions() {
    let mut config = AppConfig::default();
    config.mgmt.health.readiness_path = config.mgmt.health.liveness_path.clone();
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("routes must be distinct")
    ));

    config.mgmt.health.readiness_path = "/readyz".to_owned();
    config.mgmt.metrics.path = "/debug/runtime".to_owned();
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("routes must be distinct")
    ));
}

#[test]
fn requires_a_bounded_management_readiness_timeout() {
    let mut config = AppConfig::default();
    config.mgmt.health.probe_timeout = Duration::ZERO;
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("probe_timeout")
    ));

    config.mgmt.health.probe_timeout = Duration::from_secs(31);
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("probe_timeout")
    ));
}

#[test]
fn requires_an_absolute_sqlite_database_path() {
    let mut config = AppConfig::default();
    config.storage.sqlite.database_file = "agentvault.sqlite3".to_owned();
    assert!(matches!(
        config.validate(),
        Err(Error::Config(message)) if message.contains("sqlite")
    ));
}

#[test]
fn returns_the_absolute_sqlite_database_path() {
    let config = AppConfig::default();
    assert_eq!(
        config.sqlite_database_path(),
        std::path::PathBuf::from("/var/lib/agentvault/agentvault.sqlite3")
    );
}

#[test]
fn canonical_yaml_is_valid() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("etc/agentvault.yaml");
    AppConfig::load(path).unwrap().validate_for_serve().unwrap();
}

#[test]
fn docker_yaml_uses_the_same_typed_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/docker/config/agentvault.yaml");
    AppConfig::load(path).unwrap().validate_for_serve().unwrap();
}

#[test]
fn grpc_transport_requires_bounded_messages_and_timeout() {
    let mut config = AppConfig::default();
    config.agentvault.transport.grpc.enabled = true;
    config.agentvault.transport.grpc.max_message_bytes = 1;
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("gRPC")
    ));

    config.agentvault.transport.grpc.max_message_bytes = 2 * 1024 * 1024;
    assert!(config.validate_for_serve().is_ok());

    config.agentvault.transport.grpc.max_message_bytes = 16 * 1024 * 1024 + 1;
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("gRPC")
    ));

    config.agentvault.transport.grpc.max_message_bytes = 2 * 1024 * 1024;
    config.agentvault.transport.grpc.request_timeout = Duration::ZERO;
    assert!(matches!(
        config.validate_for_serve(),
        Err(Error::Config(message)) if message.contains("gRPC")
    ));
}

#[test]
fn validates_otel_even_when_management_listener_is_disabled() {
    let mut config = AppConfig::default();
    config.mgmt.enabled = false;
    config.mgmt.otel.enabled = true;
    config.mgmt.otel.protocol = "http".to_owned();
    assert!(matches!(config.validate_for_serve(), Err(Error::Config(message)) if message.contains("mgmt.otel")));
}

#[test]
fn rejects_removed_file_store_configuration() {
    let file = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
    std::fs::write(
        file.path(),
        r"
store:
  directory: /tmp/agentvault
",
    )
    .unwrap();

    assert!(AppConfig::load(file.path()).is_err());
}

#[test]
fn top_level_storage_and_cache_support_external_provider_refs() {
    let mut config = AppConfig::default();
    config.agentvault.payee.enabled = true;
    config.storage.provider = StorageProvider::Postgres;
    config.storage.postgres.dsn.env = "AGENTVAULT_STORAGE_POSTGRES_URL".to_owned();
    config.cache.provider = CacheProvider::Redis;
    config.cache.redis.urls = vec![SecretRefConfig {
        file: String::new(),
        env: "AGENTVAULT_CACHE_REDIS_URL".to_owned(),
    }];
    config.agentvault.payee.x402.enabled = true;
    config.agentvault.payee.x402.assets = vec![X402AssetConfig {
        network: "eip155:8453".to_owned(),
        contract: "0x0000000000000000000000000000000000001001".to_owned(),
        symbol: "USDC".to_owned(),
        decimals: 6,
    }];
    config.agentvault.chains.insert(
        "eip155:8453".to_owned(),
        ChainConfig {
            rpc: ChainRpcConfig {
                endpoint: "https://rpc.example/".to_owned(),
                ..ChainRpcConfig::default()
            },
            ..ChainConfig::default()
        },
    );
    config.agentvault.payee.x402.facilitator = ExternalProviderConfig {
        id: "primary".to_owned(),
        base_url: "https://facilitator.example/api/".to_owned(),
        ..ExternalProviderConfig::default()
    };

    config.validate().unwrap();
}

#[test]
fn component_instance_keys_and_provider_names_fail_closed() {
    let mut config = AppConfig::default();
    config.storage.instance_key = "unsafe/key".to_owned();
    assert!(matches!(config.validate(), Err(Error::Config(message)) if message.contains("instance_key")));

    let mut config = AppConfig::default();
    config.cache.instance_key.clear();
    assert!(matches!(config.validate(), Err(Error::Config(message)) if message.contains("instance_key")));

    for document in ["storage:\n  provider: oracle\n", "cache:\n  provider: memcached\n"] {
        let file = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
        std::fs::write(file.path(), document).unwrap();
        assert!(AppConfig::load(file.path()).is_err());
    }
}

#[test]
fn rejects_storage_or_cache_nested_under_agentvault() {
    for nested_key in ["storage", "cache"] {
        let file = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
        std::fs::write(
            file.path(),
            format!("agentvault:\n  {nested_key}:\n    provider: memory\nmgmt:\n  enabled: true\n"),
        )
        .unwrap();

        assert!(AppConfig::load(file.path()).is_err());
    }
}

#[test]
fn approval_jwt_requires_one_indirect_source_for_the_active_versioned_key() {
    let mut missing_active = AppConfig::default();
    missing_active.agentvault.approval.jwt.active_key_id = "approval-hs256-v2".to_owned();
    assert!(matches!(
        missing_active.validate_agentvault(),
        Err(Error::Config(message)) if message.contains("approval.jwt")
    ));

    let mut ambiguous_source = AppConfig::default();
    let active_key = ambiguous_source
        .agentvault
        .approval
        .jwt
        .signing_keys
        .get_mut("approval-hs256-v1")
        .expect("default active key");
    active_key.file = "/run/secrets/approval-jwt".to_owned();
    assert!(matches!(
        ambiguous_source.validate_agentvault(),
        Err(Error::Config(message)) if message.contains("exactly one")
    ));
}

#[test]
fn rejects_literal_provider_secret_and_removed_config_roots() {
    let file = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
    std::fs::write(
        file.path(),
        r"
agentvault:
  payee:
    x402:
      facilitator:
        id: primary
        base_url: https://facilitator.example
        secret_headers:
          Authorization:
            value: plaintext-is-not-supported
",
    )
    .unwrap();

    assert!(AppConfig::load(file.path()).is_err());

    for removed in [
        "payment:\n  enabled: true\n",
        "transport:\n  grpc:\n    enabled: true\n",
        "agentvault:\n  protocols: {}\n",
        "agentvault:\n  evm_rpc: {}\n",
        "agentvault:\n  payer:\n    provider: facilitator\n",
    ] {
        let file = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
        std::fs::write(file.path(), removed).unwrap();
        assert!(AppConfig::load(file.path()).is_err());
    }
}

#[test]
fn payer_and_payee_adapters_are_enabled_independently() {
    let mut config = AppConfig::default();
    config.agentvault.payer.x402.enabled = true;
    config.agentvault.payer.x402.assets = vec![X402AssetConfig {
        network: "eip155:8453".to_owned(),
        contract: "0x0000000000000000000000000000000000001001".to_owned(),
        symbol: "USDC".to_owned(),
        decimals: 6,
    }];
    config.agentvault.chains.insert(
        "eip155:8453".to_owned(),
        ChainConfig {
            rpc: ChainRpcConfig {
                endpoint: "https://rpc.example/".to_owned(),
                ..ChainRpcConfig::default()
            },
            ..ChainConfig::default()
        },
    );
    assert!(matches!(
        config.validate_agentvault(),
        Err(Error::Config(message)) if message.contains("payer adapters must be disabled")
    ));

    config.agentvault.payer.enabled = true;
    config.validate_agentvault().unwrap();
    assert!(!config.agentvault.payee.enabled);
}

#[test]
fn rejects_unimplemented_mpp_payer_at_startup() {
    let mut config = AppConfig::default();
    config.agentvault.payer.enabled = true;
    config.agentvault.payer.mpp.enabled = true;

    assert!(matches!(
        config.validate_agentvault(),
        Err(Error::Config(message)) if message.contains("payer.mpp") && message.contains("signing")
    ));
}

#[test]
fn shared_chain_registry_accepts_non_evm_families_without_enabling_unsupported_protocols() {
    let mut config = AppConfig::default();
    config.agentvault.chains.insert(
        "solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ".to_owned(),
        ChainConfig {
            family: ChainFamily::Solana,
            rpc: ChainRpcConfig {
                endpoint: "https://api.mainnet-beta.solana.com/".to_owned(),
                ..ChainRpcConfig::default()
            },
            confirmation_depth: 32,
            ..ChainConfig::default()
        },
    );

    config.validate_agentvault().unwrap();
}

#[test]
fn rejects_remote_plain_http_provider() {
    let provider = ExternalProviderConfig {
        id: "primary".to_owned(),
        base_url: "http://provider.example/api/".to_owned(),
        allow_insecure_loopback: true,
        ..ExternalProviderConfig::default()
    };

    assert!(matches!(
        validate_external_provider("agentvault.payee.x402.facilitator", &provider),
        Err(Error::Config(message)) if message.contains("HTTPS")
    ));
}

#[test]
fn permits_explicit_loopback_development_provider() {
    let provider = ExternalProviderConfig {
        id: "local".to_owned(),
        base_url: "http://127.0.0.1:8088/api/".to_owned(),
        allow_insecure_loopback: true,
        ..ExternalProviderConfig::default()
    };

    validate_external_provider("agentvault.payee.x402.facilitator", &provider).unwrap();
}

#[test]
fn trace_context_headers_are_transport_owned() {
    for name in ["traceparent", "tracestate"] {
        let headers = BTreeMap::from([(name.to_owned(), SecretHeaderConfig::default())]);
        assert!(matches!(
            validate_secret_headers("agentvault.chains.test.rpc", &headers),
            Err(Error::Config(message)) if message.contains("unsafe secret header")
        ));
    }
}
