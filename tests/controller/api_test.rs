use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

#[tokio::test]
async fn price_routes_require_canonical_names() {
    let fixture = PriceApiFixture::new().await;
    let owner = fixture.principal(&fixture.owner.client.auth_principal).await;
    for (method, path, status) in [
        (
            "GET",
            format!("/admin/prices?scenario_id={}", fixture.owner.scenario.id),
            StatusCode::OK,
        ),
        ("POST", "/prices/resolve".to_owned(), StatusCode::UNPROCESSABLE_ENTITY),
        ("GET", "/admin/offers".to_owned(), StatusCode::NOT_FOUND),
        ("POST", "/offers/resolve".to_owned(), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            fixture
                .request(Some(owner.clone()), method, &path, serde_json::Value::Null)
                .await
                .status(),
            status
        );
    }
    fixture.close().await;
}

#[test]
fn domain_errors_have_stable_http_statuses() {
    assert_eq!(
        ApiError::from(Error::Unauthenticated).into_response().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ApiError::from(Error::PaymentConflict("duplicate".to_owned()))
            .into_response()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        ApiError::from(Error::DependencyUnavailable("cache".to_owned()))
            .into_response()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

struct PriceApiFixture {
    _directory: tempfile::TempDir,
    _environment: PriceApiEnvironment,
    runtime: crate::cmd::payment_runtime::AssembledPaymentRuntime,
    context: crate::controller::principal_context::PrincipalContext,
    signing_key: String,
    owner: crate::storage::schema_tests::RepositoryFixture,
    outsider: crate::storage::schema_tests::RepositoryFixture,
}

struct PriceApiEnvironment(String);

impl Drop for PriceApiEnvironment {
    fn drop(&mut self) {
        std::env::remove_var(&self.0);
    }
}

impl PriceApiFixture {
    async fn new() -> Self {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::TempDir::new().unwrap();
        let key_path = directory.path().join("test-key");
        let signing_key = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let environment = PriceApiEnvironment(format!("AGENTVAULT_PRICE_TEST_KEK_{}", uuid::Uuid::new_v4().simple()));
        std::env::set_var(&environment.0, &signing_key);
        std::fs::write(&key_path, &signing_key).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let secret = crate::config::SecretRefConfig {
            file: key_path.display().to_string(),
            env: String::new(),
        };
        let mut config = crate::config::AppConfig::default();
        config.agentvault.wallet_balance_sync.enabled = false;
        config.agentvault.reconciliation.enabled = false;
        config.storage.provider = crate::config::StorageProvider::Sqlite;
        config.storage.sqlite.database_file = directory.path().join("price.sqlite3").display().to_string();
        for reference in config.agentvault.key_management.keys.values_mut() {
            *reference = crate::config::SecretRefConfig {
                file: String::new(),
                env: environment.0.clone(),
            };
        }
        for reference in config
            .agentvault
            .recovery_encryption
            .keys
            .values_mut()
            .chain(config.agentvault.approval.jwt.signing_keys.values_mut())
        {
            *reference = secret.clone();
        }
        config.server.authguard.enabled = true;
        config.server.authguard.direct_context_hmac_key = secret;
        let context =
            crate::controller::principal_context::PrincipalContext::from_config(&config.server.authguard).unwrap();
        let mut chain = crate::config::ChainConfig::default();
        chain.rpc.endpoint = "http://127.0.0.1:18545".to_owned();
        chain.rpc.allow_insecure_loopback = true;
        config.agentvault.chains.insert("eip155:31337".to_owned(), chain);
        let runtime = crate::cmd::payment_runtime::PaymentRuntimeFactory::assemble(
            &config,
            crate::apm::AgentVaultMetrics::default(),
        )
        .await
        .unwrap();
        let owner = crate::storage::schema_tests::RepositoryFixture::create(runtime.resources.storage.clone()).await;
        let outsider = crate::storage::schema_tests::RepositoryFixture::create(runtime.resources.storage.clone()).await;
        Self {
            _directory: directory,
            _environment: environment,
            runtime,
            context,
            signing_key,
            owner,
            outsider,
        }
    }

    async fn principal(&self, principal: &str) -> CallerPrincipal {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use hmac::{Hmac, Mac as _};
        let now = u64::try_from(PaymentApi::unix_time().unwrap()).unwrap();
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "version":3,"principal_id":principal,"issued_at_epoch_seconds":now,"expires_at_epoch_seconds":now+30
            }))
            .unwrap(),
        );
        let input = format!("agctx1.{payload}");
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(self.signing_key.as_bytes()).unwrap();
        mac.update(input.as_bytes());
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-authguard-context",
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
                .parse()
                .unwrap(),
        );
        self.context.resolve_request(&headers).await.unwrap().unwrap()
    }

    fn spec(price: &PaymentPrice) -> serde_json::Value {
        let mut spec = serde_json::to_value(price).unwrap();
        let fields = spec.as_object_mut().unwrap();
        fields.remove("created_at");
        fields.remove("updated_at");
        spec
    }

    async fn request(
        &self,
        principal: Option<CallerPrincipal>,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(principal) = principal {
            request = request.extension(principal);
        }
        self.runtime
            .router
            .clone()
            .oneshot(request.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap())
            .await
            .unwrap()
    }

    async fn close(self) {
        self.runtime.resources.cache.close().await.unwrap();
        self.runtime.resources.storage.close().await.unwrap();
    }
}

#[tokio::test]
async fn price_admin_rejects_unauthenticated_and_cross_scenario_writes() {
    use serde_json::json;
    let fixture = PriceApiFixture::new().await;
    let outsider = fixture.principal(&fixture.outsider.client.auth_principal).await;
    let price = &fixture.owner.price;
    let path = format!("/admin/prices/{}", price.id);
    let spec = PriceApiFixture::spec(price);
    assert_eq!(
        fixture
            .request(None, "POST", "/admin/prices", spec.clone())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for (method, uri, body) in [
        ("GET", path.clone(), json!(null)),
        ("POST", "/admin/prices".to_owned(), spec.clone()),
        (
            "PUT",
            path.clone(),
            json!({"price":spec,"expected_updated_at":price.updated_at}),
        ),
        (
            "DELETE",
            format!("{path}?expected_updated_at={}", price.updated_at),
            json!(null),
        ),
    ] {
        assert_eq!(
            fixture
                .request(Some(outsider.clone()), method, &uri, body)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        fixture.owner.ports.prices.get(&price.id).await.unwrap(),
        Some(price.clone())
    );
    fixture.close().await;
}

#[tokio::test]
async fn price_admin_create_update_delete_uses_server_revisions() {
    use serde_json::json;
    let fixture = PriceApiFixture::new().await;
    let owner = fixture.principal(&fixture.owner.client.auth_principal).await;
    let mut spec = PriceApiFixture::spec(&fixture.owner.price);
    let id = format!("{}-new", fixture.owner.price.id);
    spec["id"] = json!(id);
    spec["resource"] = json!("https://feed.example/new-price");
    let created = fixture
        .request(Some(owner.clone()), "POST", "/admin/prices", spec.clone())
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: PaymentPrice =
        serde_json::from_slice(&axum::body::to_bytes(created.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let path = format!("/admin/prices/{id}");
    let mut transplanted = spec.clone();
    transplanted["scenario_id"] = json!(fixture.outsider.scenario.id);
    assert_eq!(
        fixture
            .request(
                Some(owner.clone()),
                "PUT",
                &path,
                json!({"price":transplanted,"expected_updated_at":created.updated_at})
            )
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    spec["amount"] = json!("11");
    let update = json!({"price":spec,"expected_updated_at":created.updated_at});
    let updated = fixture.request(Some(owner.clone()), "PUT", &path, update.clone()).await;
    assert_eq!(updated.status(), StatusCode::OK);
    let updated: PaymentPrice =
        serde_json::from_slice(&axum::body::to_bytes(updated.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(updated.created_at, created.created_at);
    assert!(updated.updated_at > created.updated_at);
    assert_eq!(
        fixture
            .request(Some(owner.clone()), "PUT", &path, update)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let delete = format!("{path}?expected_updated_at={}", updated.updated_at);
    assert_eq!(
        fixture
            .request(
                Some(owner.clone()),
                "DELETE",
                &format!("{path}?expected_updated_at={}", created.updated_at),
                json!(null)
            )
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture
            .request(Some(owner.clone()), "DELETE", &delete, json!(null))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture.request(Some(owner), "GET", &path, json!(null)).await.status(),
        StatusCode::NOT_FOUND
    );
    fixture.close().await;
}
