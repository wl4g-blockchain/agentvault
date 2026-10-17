use std::{fs, os::unix::fs::symlink};

use tempfile::TempDir;

use super::*;

#[test]
fn rejects_symlink_and_overly_permissive_secret() {
    let temp = TempDir::new().unwrap();
    let secret = temp.path().join("secret");
    fs::write(&secret, "value").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_private_text(&secret, "test secret", 32).is_err());

    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    let link = temp.path().join("link");
    symlink(&secret, &link).unwrap();
    assert!(read_private_text(&link, "test secret", 32).is_err());
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

    let value = read_projected_env_value(&projection, "AGENTVAULT_TEST_SECRET", "test secret", 32)
        .unwrap()
        .unwrap();
    assert_eq!(value.as_str(), "projected-value");
    assert!(read_projected_env_value(&projection, "MISSING", "test secret", 32)
        .unwrap()
        .is_none());
}

#[test]
fn rejects_unsafe_or_ambiguous_projected_env_files() {
    let temp = TempDir::new().unwrap();
    let projection = temp.path().join("secrets.env");
    fs::write(&projection, "DUPLICATE=one\nDUPLICATE=two\n").unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_projected_env_value(&projection, "DUPLICATE", "test secret", 32).is_err());

    fs::write(&projection, "MALFORMED\n").unwrap();
    assert!(read_projected_env_value(&projection, "MISSING", "test secret", 32).is_err());

    fs::write(&projection, "SAFE=value\n").unwrap();
    fs::set_permissions(&projection, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(read_projected_env_value(&projection, "SAFE", "test secret", 32).is_err());
}
