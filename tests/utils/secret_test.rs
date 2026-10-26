use std::{fs, os::unix::fs::symlink};

use tempfile::TempDir;

use super::*;

#[test]
fn rejects_symlink_and_overly_permissive_secret() {
    let temp = TempDir::new().unwrap();
    let secret = temp.path().join("secret");
    fs::write(&secret, "value").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(SecretReader::read_private_text(&secret, "test secret", 32).is_err());

    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let link = temp.path().join("link");
    symlink(&secret, &link).unwrap();
    assert!(SecretReader::read_private_text(&link, "test secret", 32).is_err());
}

#[test]
fn reads_one_value_from_a_read_only_projected_env_file() {
    let temp = TempDir::new().unwrap();
    let projection = temp.path().join("secrets.env");
    fs::write(
        &projection,
        "# one provider payload\nFIRST=ignored\nAGENTVAULT_TEST_SECRET=projected-value\n",
    )
    .unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o444)).unwrap();

    let value = SecretReader::read_projected_env_value(&projection, "AGENTVAULT_TEST_SECRET", "test secret", 32)
        .unwrap()
        .unwrap();
    assert_eq!(value.as_str(), "projected-value");
    assert!(
        SecretReader::read_projected_env_value(&projection, "MISSING", "test secret", 32)
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejects_unsafe_or_ambiguous_projected_env_files() {
    let temp = TempDir::new().unwrap();
    let projection = temp.path().join("secrets.env");
    fs::write(&projection, "DUPLICATE=one\nDUPLICATE=two\n").unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(SecretReader::read_projected_env_value(&projection, "DUPLICATE", "test secret", 32).is_err());

    fs::write(&projection, "MALFORMED\n").unwrap();
    assert!(SecretReader::read_projected_env_value(&projection, "MISSING", "test secret", 32).is_err());

    fs::write(&projection, "SAFE=value\n").unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(SecretReader::read_projected_env_value(&projection, "SAFE", "test secret", 32).is_err());
}

#[test]
fn projection_selector_treats_an_empty_value_as_unset() {
    let reader = SecretReader::with_projection(Some("/secrets.env".into()));
    assert_eq!(reader.projection, Some(PathBuf::from("/secrets.env")));
    assert!(SecretReader::with_projection(Some("".into())).projection.is_none());
}

#[test]
fn canonical_secret_field_reads_without_global_environment_mutation() {
    let reader = SecretReader::with_projection(None);
    let variable = "AGENTVAULT__STORAGE__POSTGRES__URL";
    let value = reader
        .read_env(variable, "test key", 32, |name| {
            (name == variable).then(|| "value".into())
        })
        .unwrap();
    assert_eq!(value.as_str(), "value");
    let error = reader
        .read_env(variable, "test key", 2, |_| Some("do-not-log".into()))
        .unwrap_err();
    assert!(!error.to_string().contains("do-not-log"));
    assert!(error.to_string().contains("exceeds"));
    assert!(reader.read_env(variable, "test key", 32, |_| None).is_err());
}

#[test]
fn secret_projection_preserves_provider_priority_and_rejects_duplicates() {
    let temp = TempDir::new().unwrap();
    let projection = temp.path().join("secrets.env");
    let reader = SecretReader::with_projection(Some(projection.clone().into_os_string()));
    let variable = "AGENTVAULT__STORAGE__POSTGRES__URL";
    fs::write(&projection, format!("{variable}=projected\n")).unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o444)).unwrap();
    let value = reader
        .read_env(variable, "test key", 32, |_| panic!("projection must win"))
        .unwrap();
    assert_eq!(value.as_str(), "projected");
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&projection, format!("{variable}=first\n{variable}=second\n")).unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o444)).unwrap();
    let error = reader
        .read_env(variable, "test key", 32, |_| panic!("ambiguous projection must fail"))
        .unwrap_err();
    assert!(!error.to_string().contains("first"));
    assert!(!error.to_string().contains("second"));
}
