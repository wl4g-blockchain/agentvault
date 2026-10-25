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
            operation: ConsoleCommand::Import { paths }
        } if paths == [PathBuf::from("resources/"), PathBuf::from("client.yaml")]
    ));
    assert!(Cli::try_parse_from(["agentvault", "console", "import"]).is_err());
}
