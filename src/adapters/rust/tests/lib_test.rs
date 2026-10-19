use agentvault_sdk::{
    AgentVault, AxumPayeeMiddleware, MqttVaultTransport, PayeeMiddleware, PayerMiddleware, ReqwestPayerMiddleware,
};

#[test]
fn public_surface_is_role_oriented() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<AgentVault<MqttVaultTransport>>();
    assert_send_sync::<PayerMiddleware<MqttVaultTransport>>();
    assert_send_sync::<ReqwestPayerMiddleware<MqttVaultTransport>>();
    assert_send_sync::<PayeeMiddleware<MqttVaultTransport>>();
    assert_send_sync::<AxumPayeeMiddleware<MqttVaultTransport>>();
}
