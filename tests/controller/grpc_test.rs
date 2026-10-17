use axum::http::{HeaderName, Method};

use super::{is_allowed_payment_route, is_allowed_request_header};

#[test]
fn accepts_only_sdk_protocol_headers_from_grpc_envelopes() {
    for name in ["accept", "content-type", "x-request-id"] {
        assert!(is_allowed_request_header(&HeaderName::from_static(name)));
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
        assert!(!is_allowed_request_header(&HeaderName::from_static(name)));
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
        (Method::POST, "/payee/challenges"),
        (Method::POST, "/payee/verify"),
        (Method::POST, "/payee/settle"),
    ] {
        assert!(
            is_allowed_payment_route(&method, path),
            "route should be allowed: {method} {path}"
        );
    }

    for (method, path) in [
        (Method::GET, "/admin/clients"),
        (Method::GET, "/admin/wallets"),
        (Method::POST, "/payments/intent-1"),
        (Method::GET, "/payments//receipt"),
        (Method::GET, "/payments/../admin/clients"),
        (Method::GET, "/unknown"),
    ] {
        assert!(
            !is_allowed_payment_route(&method, path),
            "route should be denied: {method} {path}"
        );
    }
}
