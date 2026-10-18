use super::*;

#[test]
fn request_debug_redacts_headers_and_body() {
    let request = TransportRequest {
        method: HttpMethod::Post,
        path: "/api/v1/test".to_owned(),
        headers: BTreeMap::from([("authorization".to_owned(), "secret".to_owned())]),
        body: b"secret".to_vec(),
    };
    let rendered = format!("{request:?}");
    assert!(rendered.contains("<redacted>"));
    assert!(!rendered.contains("secret"));
}

#[test]
fn percent_encoding_is_stable() {
    assert_eq!(percent_encode("run id/1"), "run%20id%2F1");
}
