use super::parse_bind;

#[test]
fn parses_explicit_listener_address() {
    assert_eq!(parse_bind("127.0.0.1:8081").unwrap().port(), 8081);
    assert!(parse_bind("invalid").is_err());
}
