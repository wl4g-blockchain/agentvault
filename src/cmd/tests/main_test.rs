#[test]
fn daemon_binary_exposes_the_cli_help_contract() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agentvault"))
        .arg("--help")
        .output()
        .expect("run AgentVault CLI help");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("AgentVault"));
}
