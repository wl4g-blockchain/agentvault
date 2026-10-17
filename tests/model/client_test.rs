use super::*;

fn client() -> PaymentClient {
    PaymentClient {
        id: "client-1".to_owned(),
        name: "Flowgent".to_owned(),
        auth_principal: "principal-flowgent".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn client_authentication_requires_an_enabled_exact_binding() {
    let mut value = client();
    assert!(value.authenticates("principal-flowgent"));
    assert!(!value.authenticates(""));
    value.enabled = false;
    assert!(!value.authenticates("principal-flowgent"));
}
