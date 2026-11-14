use super::*;

#[test]
fn unrestricted_marker_cannot_be_registered_as_a_client_principal() {
    let client = PaymentClient {
        id: "client".to_owned(),
        name: "Client".to_owned(),
        auth_principal: UNRESTRICTED_PRINCIPAL.to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    assert_eq!(client.validate(), Err("auth_principal"));
}
