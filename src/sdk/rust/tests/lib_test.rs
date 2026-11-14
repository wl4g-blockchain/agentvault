use agentvault_sdk::{
    AgentVault, AxumPayeeMiddleware, HttpVaultTransport, PayeeMiddleware, PayerMiddleware, ReqwestPayerMiddleware,
};

#[test]
fn public_surface_is_role_oriented() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<AgentVault<HttpVaultTransport>>();
    assert_send_sync::<PayerMiddleware<HttpVaultTransport>>();
    assert_send_sync::<ReqwestPayerMiddleware<HttpVaultTransport>>();
    assert_send_sync::<PayeeMiddleware<HttpVaultTransport>>();
    assert_send_sync::<AxumPayeeMiddleware<HttpVaultTransport>>();
}
