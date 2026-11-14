use super::*;

#[test]
fn command_facade_exports_the_server_type() {
    fn accepts_server(_: Option<AgentVaultServer>) {}
    accepts_server(None);
}
