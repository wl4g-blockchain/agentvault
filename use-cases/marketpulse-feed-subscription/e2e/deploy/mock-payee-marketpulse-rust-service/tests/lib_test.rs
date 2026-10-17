use super::required_env;

#[test]
fn missing_runtime_configuration_fails_closed() {
    assert!(required_env("AGENTVAULT_E2E_TEST_UNSET_VALUE").is_err());
}
