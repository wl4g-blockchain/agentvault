use super::*;

#[test]
fn result_alias_preserves_agentvault_errors() {
    let result: Result<()> = Err(Error::Forbidden);
    assert!(matches!(result, Err(Error::Forbidden)));
}
