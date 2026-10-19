#[test]
fn binary_owns_the_axum_composition_root() {
    assert_eq!(env!("CARGO_PKG_NAME"), "mock-payee-marketpulse-rust-service");
    let _ = super::run;
}
