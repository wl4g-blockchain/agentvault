#[test]
fn wire_components_remain_separate_from_controller_listener_types() {
    fn runtime<T: crate::cmd::RuntimeComponent>() {}
    runtime::<crate::controller::GrpcController>();
    runtime::<crate::controller::MqttController>();
    runtime::<crate::transport::http::HttpApiServer>();
}
