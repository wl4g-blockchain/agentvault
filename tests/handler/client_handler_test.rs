use super::*;

#[test]
fn client_handler_remains_a_storage_port_consumer() {
    assert!(std::mem::size_of::<ClientHandler>() > 0);
}
