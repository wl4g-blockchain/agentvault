use super::*;
#[test]
fn mqtt_listener_implements_the_runtime_component_contract() {
    fn component<T: RuntimeComponent>() {}
    component::<MqttController>();
}
