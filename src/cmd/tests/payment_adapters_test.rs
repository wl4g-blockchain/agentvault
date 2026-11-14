use super::*;
use crate::config::SecretRefConfig;
use std::os::unix::fs::PermissionsExt as _;
#[test]
fn operator_secret_headers_are_resolved_with_prefix_and_marked_sensitive() {
    let directory = tempfile::tempdir().unwrap();
    let secret = directory.path().join("header.secret");
    std::fs::write(&secret, "operator-token").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let headers = BTreeMap::from([(
        "authorization".into(),
        SecretHeaderConfig {
            source: SecretRefConfig {
                file: secret.display().to_string(),
                env: String::new(),
            },
            prefix: "Bearer ".into(),
        },
    )]);
    let resolved = RuntimeAdapterFactory::secret_headers(&headers, "provider").unwrap();
    let authorization = resolved.get("authorization").unwrap();
    assert_eq!(authorization.to_str().unwrap(), "Bearer operator-token");
    assert!(authorization.is_sensitive());
}
