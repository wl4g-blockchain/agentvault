use super::*;

#[test]
fn console_import_requires_paths_and_accepts_global_config() {
    let cli = Cli::try_parse_from([
        "agentvault",
        "--config",
        "test-agentvault.yaml",
        "console",
        "import",
        "resources/",
        "client.yaml",
    ])
    .unwrap();

    assert_eq!(cli.config, PathBuf::from("test-agentvault.yaml"));
    assert!(matches!(
        cli.command,
        Command::Console {
            operation: Some(ConsoleCommand::Import { paths })
        } if paths == [PathBuf::from("resources/"), PathBuf::from("client.yaml")]
    ));
    assert!(Cli::try_parse_from(["agentvault", "console", "import"]).is_err());
}

#[test]
fn console_without_an_operation_opens_the_shell() {
    let cli = Cli::try_parse_from(["agentvault", "console"]).unwrap();
    assert!(matches!(cli.command, Command::Console { operation: None }));
}

#[test]
fn wallet_console_surface_is_create_and_metadata_only() {
    for command in [
        ["agentvault", "console", "wallet", "reveal"].as_slice(),
        ["agentvault", "console", "wallet", "export"].as_slice(),
        ["agentvault", "console", "wallet", "update"].as_slice(),
        ["agentvault", "console", "wallet", "rotate"].as_slice(),
        ["agentvault", "console", "wallet", "delete"].as_slice(),
    ] {
        assert!(Cli::try_parse_from(command).is_err());
    }

    assert!(Cli::try_parse_from(["agentvault", "console", "wallet", "create", "wallet.yaml",]).is_ok());
    assert!(Cli::try_parse_from(["agentvault", "console", "wallet", "list"]).is_ok());
    assert!(Cli::try_parse_from(["agentvault", "console", "wallet", "get", "wallet-1"]).is_ok());
    assert!(Cli::try_parse_from([
        "agentvault",
        "console",
        "set-enabled",
        "wallet",
        "wallet-1",
        "--enabled",
        "false",
    ])
    .is_err());
}

#[test]
fn console_never_calls_wallet_key_read_or_mutation_ports() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cmd/console.rs"));
    for forbidden in [
        ".load_active_key(",
        ".list_key_versions(",
        ".rotate(",
        "wallet_payment_signer(",
    ] {
        assert!(
            !source.contains(forbidden),
            "console contains forbidden wallet operation {forbidden}"
        );
    }
}

#[test]
fn approval_decision_requires_an_explicit_principal() {
    assert!(Cli::try_parse_from([
        "agentvault",
        "console",
        "approval",
        "decide",
        "approval-1",
        "intent-1",
        "--decision",
        "approve",
        "--approved-amount",
        "100",
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "agentvault",
        "console",
        "approval",
        "decide",
        "approval-1",
        "intent-1",
        "--principal",
        "operator-1",
        "--decision",
        "approve",
        "--approved-amount",
        "100",
    ])
    .is_ok());
}

#[test]
fn catalog_lifecycle_updates_require_an_explicit_boolean() {
    assert!(Cli::try_parse_from([
        "agentvault",
        "console",
        "set-enabled",
        "policy",
        "policy-1",
        "--enabled",
        "false",
    ])
    .is_ok());
    assert!(Cli::try_parse_from(["agentvault", "console", "set-enabled", "policy", "policy-1",]).is_err());
}

#[test]
fn resource_timestamps_advance_monotonically() {
    assert_eq!(ConsoleStore::next_timestamp(10, 12).unwrap(), 12);
    assert_eq!(ConsoleStore::next_timestamp(10, 10).unwrap(), 11);
    assert!(ConsoleStore::next_timestamp(i64::MAX, i64::MAX).is_err());
}

#[test]
fn price_commands_keep_offer_spelling_as_a_compatibility_alias() {
    for resource in ["price", "offer"] {
        let cli = Cli::try_parse_from(["agentvault", "console", "get", resource, "price-1"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Console {
                operation: Some(ConsoleCommand::Get(GetArgs {
                    resource: ConsoleResource::Price,
                    ..
                }))
            }
        ));
    }
}
