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
