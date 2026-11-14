use super::*;

#[test]
fn public_errors_do_not_erase_the_failure_domain() {
    assert_eq!(
        Error::Forbidden.to_string(),
        "principal is not authorized for this resource"
    );
    assert!(Error::Storage("database unavailable".to_owned())
        .to_string()
        .contains("storage"));
}
