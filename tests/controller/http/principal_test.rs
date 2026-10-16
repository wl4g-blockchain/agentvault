use std::{fs, os::unix::fs::PermissionsExt as _};

use axum::http::{HeaderMap, HeaderValue};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac as _};
use serde_json::json;
use sha2::Sha256;
use tempfile::TempDir;

use super::*;
use crate::config::SecretRefConfig;

const KEY: &[u8] = b"agentvault-authguard-test-context-key-minimum-32-bytes";

fn authguard_context() -> (TempDir, PrincipalContext) {
    let directory = TempDir::new().unwrap();
    let key_path = directory.path().join("context.key");
    fs::write(&key_path, KEY).unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
    let config = AuthGuardIntegrationConfig {
        enabled: true,
        direct_context_hmac_key: SecretRefConfig {
            file: key_path.to_string_lossy().into_owned(),
            env: String::new(),
        },
    };
    (directory, PrincipalContext::from_config(&config).unwrap())
}

fn signed_context(principal: &str, issued_at: u64, expires_at: u64) -> String {
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "version": 3,
            "principal_id": principal,
            "action": "agentvault.api.use",
            "resource_urn": "urn:authguard:prod:agentvault:global:example:api",
            "allow_resource_urns": [],
            "deny_resource_urns": [],
            "policy_revision": 1,
            "issued_at_epoch_seconds": issued_at,
            "expires_at_epoch_seconds": expires_at
        }))
        .unwrap(),
    );
    let input = format!("agctx1.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).unwrap();
    mac.update(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

#[test]
fn requires_a_valid_signed_authguard_context_when_enabled() {
    let (_directory, context) = authguard_context();
    assert!(context.resolve(&HeaderMap::new()).is_none());

    let now = epoch_seconds();
    let signed = signed_context("principal:marketpulse", now, now + 30);
    let mut headers = HeaderMap::new();
    headers.insert(ACCESS_CONTEXT_HEADER, HeaderValue::from_str(&signed).unwrap());
    assert_eq!(context.resolve(&headers).unwrap().as_str(), "principal:marketpulse");

    headers.insert(
        ACCESS_CONTEXT_HEADER,
        HeaderValue::from_static("agctx1.forged.signature"),
    );
    assert!(context.resolve(&headers).is_none());
}

#[test]
fn rejects_expired_or_future_authguard_contexts() {
    let (_directory, context) = authguard_context();
    let now = epoch_seconds();
    for signed in [
        signed_context("principal:marketpulse", now.saturating_sub(60), now),
        signed_context("principal:marketpulse", now + 31, now + 60),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(ACCESS_CONTEXT_HEADER, HeaderValue::from_str(&signed).unwrap());
        assert!(context.resolve(&headers).is_none());
    }
}

#[test]
fn disabled_authguard_is_explicitly_unrestricted() {
    let context = PrincipalContext::from_config(&AuthGuardIntegrationConfig {
        enabled: false,
        ..AuthGuardIntegrationConfig::default()
    })
    .unwrap();
    assert_eq!(
        context.resolve(&HeaderMap::new()).unwrap().as_str(),
        UNRESTRICTED_PRINCIPAL
    );
}
