use super::*;
#[test]
fn payer_configuration_does_not_accept_payee_provider_authority() {
    assert!(serde_yaml::from_str::<PayerConfig>("facilitator: https://untrusted.example").is_err());
}
