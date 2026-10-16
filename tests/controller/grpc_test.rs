use axum::http::HeaderName;

use super::is_reserved_request_header;

#[test]
fn rejects_identity_and_authority_headers_from_grpc_envelopes() {
    for name in ["authorization", "host", "x-authguard-context"] {
        assert!(is_reserved_request_header(&HeaderName::from_static(name)));
    }
    assert!(!is_reserved_request_header(&HeaderName::from_static("idempotency-key")));
}
