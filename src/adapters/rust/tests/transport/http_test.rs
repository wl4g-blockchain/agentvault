use super::*;

#[test]
fn rejects_authority_override_and_redacts_token() {
    assert!(HttpVaultTransport::new("https://vault.example/api").is_ok());
    assert!(HttpVaultTransport::new("https://user@vault.example").is_err());
    assert!(HttpVaultTransport::new("https://vault.example?next=evil").is_err());
    let transport = HttpVaultTransport::new("https://vault.example")
        .and_then(|transport| transport.with_authguard_bearer("super-secret"))
        .expect("transport");
    assert!(transport.endpoint("https://evil.example/pay").is_err());
    assert!(!format!("{transport:?}").contains("super-secret"));
}
