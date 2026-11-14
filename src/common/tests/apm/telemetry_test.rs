use super::*;

#[test]
fn telemetry_settings_extract_environment_without_leaking_other_attributes() {
    assert_eq!(
        find_resource_attribute(
            "service.name=vault,deployment.environment.name=staging",
            "deployment.environment.name"
        ),
        Some("staging".to_owned())
    );
    assert_eq!(find_resource_attribute("service.name=vault", "missing"), None);
}

#[test]
fn trace_identity_accepts_only_canonical_nonzero_w3c_context() {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "traceparent",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            .parse()
            .expect("valid header"),
    );
    assert_eq!(
        trace_identity(&headers),
        Some(("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7"))
    );

    for invalid in [
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
        "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
    ] {
        headers.insert("traceparent", invalid.parse().expect("ASCII test header"));
        assert_eq!(trace_identity(&headers), None, "accepted {invalid}");
    }
}
