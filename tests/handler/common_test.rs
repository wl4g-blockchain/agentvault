use super::*;

#[test]
fn origin_is_canonical_and_rejects_ambiguous_urls() {
    assert_eq!(
        canonical_http_origin("https://merchant.example:8443/paid?item=1").unwrap(),
        "https://merchant.example:8443"
    );
    assert!(canonical_http_origin("https://user:secret@merchant.example/paid").is_err());
    assert!(canonical_http_origin("https://merchant.example/paid#credential").is_err());
    assert!(canonical_http_origin("file:///etc/passwd").is_err());
}

#[test]
fn trusted_effective_time_allows_bounded_chain_clock_skew() {
    assert!(validate_trusted_effective_time(1_000, 400, 1_000).is_ok());
    assert!(validate_trusted_effective_time(1_000, 1_060, 1_000).is_ok());
    assert!(validate_trusted_effective_time(1_000, 399, 1_000).is_err());
    assert!(validate_trusted_effective_time(1_000, 1_061, 1_000).is_err());
    assert!(validate_trusted_effective_time(1_000, -1, 1_000).is_err());
}
