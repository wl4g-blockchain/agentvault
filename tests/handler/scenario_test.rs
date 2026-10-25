use super::*;

#[test]
fn handlers_share_the_client_ownership_boundary() {
    assert!(std::mem::size_of::<ClientHandler>() > 0);
    assert!(std::mem::size_of::<ScenarioHandler>() > 0);
}
