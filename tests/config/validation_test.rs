use super::*;
#[test]
fn insecure_operator_endpoints_are_restricted_to_explicit_loopback() {
    assert!(ConfigValidation::validate_operator_endpoint("rpc", "http://rpc.example", true).is_err());
    assert!(ConfigValidation::validate_operator_endpoint("rpc", "http://127.0.0.1:8545", false).is_err());
    ConfigValidation::validate_operator_endpoint("rpc", "http://127.0.0.1:8545", true).unwrap();
}

#[test]
fn listener_overlap_handles_wildcards_and_mapped_addresses() {
    for (left, right, expected) in [
        ("0.0.0.0", "127.0.0.1", true),
        ("::", "::1", true),
        ("::", "127.0.0.1", true),
        ("::ffff:127.0.0.1", "127.0.0.1", true),
        ("0.0.0.0", "::ffff:127.0.0.1", true),
        ("127.0.0.1", "127.0.0.2", false),
        ("127.0.0.1", "::1", false),
        ("0.0.0.0", "::1", false),
    ] {
        let left = left.parse().unwrap();
        let right = right.parse().unwrap();
        assert_eq!(
            ConfigValidation::listener_addresses_overlap(left, 8080, right, 8080),
            expected
        );
        assert_eq!(
            ConfigValidation::listener_addresses_overlap(right, 8080, left, 8080),
            expected
        );
        assert!(!ConfigValidation::listener_addresses_overlap(left, 8080, right, 9091));
    }
}
