use super::*;

#[test]
fn provision_command_requires_a_manifest_and_accepts_global_config() {
    let cli = Cli::try_parse_from([
        "agentvault",
        "--config",
        "test-agentvault.yaml",
        "provision",
        "--manifest",
        "test-manifest.json",
    ])
    .unwrap();

    assert_eq!(cli.config, PathBuf::from("test-agentvault.yaml"));
    assert!(matches!(
        cli.command,
        Command::Provision { manifest }
            if manifest.as_path() == std::path::Path::new("test-manifest.json")
    ));
    assert!(Cli::try_parse_from(["agentvault", "provision"]).is_err());
}
