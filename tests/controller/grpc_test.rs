use super::*;
#[test]
fn grpc_listener_implements_the_runtime_component_contract() {
    fn component<T: RuntimeComponent>() {}
    component::<GrpcController>();
}
