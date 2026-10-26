use axum::http::{HeaderName, Method};

use super::*;

#[test]
fn accepts_only_sdk_protocol_headers_from_grpc_envelopes() {
    for name in ["accept", "content-type", "x-request-id"] {
        assert!(GrpcEnvelopeService::is_allowed_request_header(
            &HeaderName::from_static(name)
        ));
    }
    for name in [
        "authorization",
        "host",
        "traceparent",
        "tracestate",
        "x-authguard-context",
        "x-authguard-scope-token",
        "cookie",
        "content-length",
    ] {
        assert!(!GrpcEnvelopeService::is_allowed_request_header(
            &HeaderName::from_static(name)
        ));
    }
}

#[test]
fn requires_a_canonical_nonzero_w3c_traceparent() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "traceparent",
        "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
            .parse()
            .expect("traceparent"),
    );
    assert_eq!(
        GrpcEnvelopeService::trace_identity(&headers),
        Some(("0123456789abcdef0123456789abcdef", "0123456789abcdef"))
    );
    for invalid in [
        "00-00000000000000000000000000000000-0123456789abcdef-01",
        "00-0123456789abcdef0123456789abcdef-0000000000000000-01",
        "00-0123456789ABCDEF0123456789ABCDEF-0123456789abcdef-01",
        "ff-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
    ] {
        headers.insert("traceparent", invalid.parse().expect("ASCII traceparent"));
        assert_eq!(
            GrpcEnvelopeService::trace_identity(&headers),
            None,
            "accepted {invalid}"
        );
    }
}

#[test]
fn exposes_only_domain_authorized_sdk_routes() {
    for (method, path) in [
        (Method::POST, "/payments/authorize"),
        (Method::GET, "/payments/intent-1"),
        (Method::GET, "/payments/intent-1/receipt"),
        (Method::POST, "/payments/intent-1/outbound-response"),
        (Method::GET, "/payments/balance"),
        (Method::GET, "/approvals"),
        (Method::GET, "/approvals/approval-1"),
        (Method::POST, "/approvals/approval-1/decision"),
        (Method::POST, "/offers/resolve"),
        (Method::POST, "/prices/resolve"),
        (Method::POST, "/payee/challenges"),
        (Method::POST, "/payee/verify"),
        (Method::POST, "/payee/settle"),
    ] {
        assert!(
            GrpcEnvelopeService::is_allowed_payment_route(&method, path),
            "route should be allowed: {method} {path}"
        );
    }

    for (method, path) in [
        (Method::GET, "/admin/clients"),
        (Method::POST, "/admin/prices"),
        (Method::PUT, "/admin/prices/price-1"),
        (Method::DELETE, "/admin/prices/price-1"),
        (Method::GET, "/admin/wallets"),
        (Method::POST, "/payments/intent-1"),
        (Method::GET, "/payments//receipt"),
        (Method::GET, "/payments/../admin/clients"),
        (Method::GET, "/unknown"),
    ] {
        assert!(
            !GrpcEnvelopeService::is_allowed_payment_route(&method, path),
            "route should be denied: {method} {path}"
        );
    }
}
