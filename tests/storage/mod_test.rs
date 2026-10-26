use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const POSTGRES_DDL: &str = include_str!("../../migrations/postgres/001_init.ddl.sql");
const POSTGRES_DML: &str = include_str!("../../migrations/postgres/001_init.dml.sql");
const SQLITE_DDL: &str = include_str!("../../migrations/sqlite/001_init.ddl.sql");
const SQLITE_DML: &str = include_str!("../../migrations/sqlite/001_init.dml.sql");

#[test]
fn sqlite_and_postgres_define_the_same_logical_schema() {
    assert_eq!(tables(POSTGRES_DDL), tables(SQLITE_DDL));
    assert_eq!(foreign_keys(POSTGRES_DDL), foreign_keys(SQLITE_DDL));
    assert_eq!(indexes(POSTGRES_DDL), indexes(SQLITE_DDL));
    assert_eq!(POSTGRES_DML.trim(), SQLITE_DML.trim());
}

#[test]
fn each_dialect_contains_only_the_canonical_init_pair() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for dialect in ["postgres", "sqlite"] {
        let mut names = fs::read_dir(root.join(dialect))
            .expect("read migration directory")
            .map(|entry| {
                entry
                    .expect("read migration entry")
                    .file_name()
                    .into_string()
                    .expect("UTF-8 migration name")
            })
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["001_init.ddl.sql", "001_init.dml.sql", "002_price.ddl.sql"]);
    }
}

#[test]
fn canonical_schema_contains_no_procedural_business_triggers() {
    for (dialect, ddl) in [("PostgreSQL", POSTGRES_DDL), ("SQLite", SQLITE_DDL)] {
        let normalized = ddl.to_ascii_uppercase();
        assert!(
            !normalized.contains("CREATE TRIGGER"),
            "{dialect} business invariants must be enforced by explicit storage transactions"
        );
        assert!(
            !normalized.contains("LANGUAGE PLPGSQL"),
            "{dialect} canonical schema must not contain procedural business functions"
        );
    }
}

#[test]
fn credential_replay_identity_is_unique_within_each_payment_direction() {
    for ddl in [POSTGRES_DDL, SQLITE_DDL] {
        let normalized = ddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains(
            "CREATE UNIQUE INDEX uq_pay_intent_credential ON pay_intent(protocol, direction, credential_hash) WHERE credential_hash IS NOT NULL;"
        ));
    }
}

#[test]
fn wallet_kek_cannot_reuse_an_aes_gcm_wrap_nonce() {
    for ddl in [POSTGRES_DDL, SQLITE_DDL] {
        let normalized = ddl.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized
            .contains("CREATE UNIQUE INDEX uq_wallet_key_kek_wrap_nonce ON wallet_key_version(kek_id, wrap_nonce);"));
    }
}

fn tables(ddl: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut result = BTreeMap::new();
    let mut current: Option<(String, BTreeSet<String>)> = None;
    for source_line in ddl.lines() {
        let indentation = source_line.len() - source_line.trim_start().len();
        let line = source_line.trim();
        if let Some(name) = line
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .or_else(|| line.strip_prefix("CREATE TABLE "))
            .and_then(|rest| rest.split_whitespace().next())
        {
            current = Some((name.trim_end_matches('(').to_owned(), BTreeSet::new()));
            continue;
        }
        let Some((_, columns)) = current.as_mut() else {
            continue;
        };
        if line == ");" {
            let (name, columns) = current.take().expect("active table");
            result.insert(name, columns);
            continue;
        }
        if line.is_empty()
            || line.starts_with("--")
            // Canonical migrations indent table columns by exactly four
            // spaces. Deeper lines are continuations of dialect-specific
            // CHECK expressions, never additional columns.
            || indentation != 4
            || ["CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK"]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }
        if let Some(column) = line.split_whitespace().next() {
            columns.insert(column.trim_matches('"').trim_end_matches(',').to_owned());
        }
    }
    result
}

fn foreign_keys(ddl: &str) -> BTreeSet<(String, String, String)> {
    let mut result = BTreeSet::new();
    let mut table = None;
    let mut composite_source = None;

    for source_line in ddl.lines() {
        let indentation = source_line.len() - source_line.trim_start().len();
        let line = source_line.trim();

        if let Some(name) = line
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .or_else(|| line.strip_prefix("CREATE TABLE "))
            .and_then(|rest| rest.split_whitespace().next())
        {
            table = Some(name.trim_end_matches('(').to_owned());
            composite_source = None;
            continue;
        }
        if line == ");" {
            table = None;
            composite_source = None;
            continue;
        }
        let Some(table_name) = table.as_ref() else {
            continue;
        };

        if indentation == 4 && line.starts_with("FOREIGN KEY ") {
            composite_source = parenthesized(line).map(normalize_column_list);
            continue;
        }

        let Some(reference_offset) = line.find("REFERENCES ") else {
            continue;
        };
        let target = &line[reference_offset + "REFERENCES ".len()..];
        let Some(target_end) = target.find(')') else {
            continue;
        };
        let target = normalize_column_list(&target[..=target_end]);

        let source = if let Some(source) = composite_source.take() {
            source
        } else if indentation == 4 {
            line.split_whitespace()
                .next()
                .map(|column| column.trim_matches('"').to_owned())
                .expect("inline foreign key column")
        } else {
            continue;
        };
        result.insert((table_name.clone(), source, target));
    }

    result
}

fn parenthesized(value: &str) -> Option<&str> {
    let start = value.find('(')?;
    let end = value[start..].find(')')? + start;
    Some(&value[start..=end])
}

fn normalize_column_list(value: &str) -> String {
    value.chars().filter(|character| !character.is_whitespace()).collect()
}

fn indexes(ddl: &str) -> BTreeSet<String> {
    ddl.lines()
        .filter_map(|line| {
            let line = line.trim();
            [
                "CREATE UNIQUE INDEX IF NOT EXISTS ",
                "CREATE INDEX IF NOT EXISTS ",
                "CREATE UNIQUE INDEX ",
                "CREATE INDEX ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .and_then(|rest| rest.split_whitespace().next())
            .map(str::to_owned)
        })
        .collect()
}

/// One provider-neutral contract fixture, exercised against both SQL dialects.
pub(crate) struct RepositoryFixture {
    pub ports: crate::storage::StoragePorts,
    pub client: crate::model::PaymentClient,
    pub scenario: crate::model::PaymentScenario,
    pub wallet: crate::model::Wallet,
    pub policy: crate::model::PaymentPolicy,
    pub offer: crate::model::PaymentPrice,
}

impl RepositoryFixture {
    pub async fn create(storage: std::sync::Arc<dyn crate::storage::IStorage>) -> Self {
        use crate::{model::*, storage::PageRequest};

        let ports = storage.ports();
        let prefix = uuid::Uuid::new_v4().to_string();
        let client = PaymentClient {
            id: format!("{prefix}-client"),
            name: "Repository contract".into(),
            auth_principal: format!("principal:{prefix}"),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        };
        ports.clients.insert(&client).await.unwrap();
        let scenario = PaymentScenario {
            id: format!("{prefix}-scenario"),
            client_id: client.id.clone(),
            name: "Contract".into(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        };
        ports.scenarios.register(&scenario).await.unwrap();
        let wallet = Wallet {
            id: format!("{prefix}-wallet"),
            algorithm: "external".into(),
            address: format!("0x{:040x}", uuid::Uuid::new_v4().as_u128()),
            public_key: String::new(),
            created_at: 1,
        };
        ports.wallets.create_external(&wallet).await.unwrap();
        let asset = AssetIdentity {
            network: "eip155:31337".into(),
            asset: "USDC".into(),
            token_contract: Some("0x1111111111111111111111111111111111111111".into()),
            decimals: 6,
        };
        ports
            .wallets
            .set_spending_limits(
                &wallet.id,
                &asset.budget_unit(),
                Some(Amount::new(100)),
                Some(Amount::new(1000)),
                2,
            )
            .await
            .unwrap();
        let policy = PaymentPolicy {
            id: format!("{prefix}-policy"),
            scenario_id: scenario.id.clone(),
            wallet_ids: vec![wallet.id.clone()],
            protocols: vec!["x402".into()],
            networks: vec![asset.network.clone()],
            assets: vec![asset.clone()],
            budget_unit: asset.budget_unit(),
            allowed_recipients: vec![wallet.address.clone()],
            allowed_origins: vec!["https://feed.example".into()],
            inbound_limits: Some(PaymentPolicyLimits {
                max_per_payment: Amount::new(100),
                max_per_day: Amount::new(1000),
                approval_threshold: None,
                approval_enabled: false,
                approval_overage: None,
            }),
            outbound_limits: None,
            enabled: true,
            created_at: 3,
            updated_at: 3,
        };
        ports.policies.insert(&policy).await.unwrap();
        let offer = PaymentPrice {
            id: format!("{prefix}-offer"),
            scenario_id: scenario.id.clone(),
            policy_id: policy.id.clone(),
            resource: format!("https://feed.example/{prefix}"),
            protocols: vec!["x402".into()],
            asset,
            amount: Amount::new(10),
            wallet_id: wallet.id.clone(),
            enabled: true,
            created_at: 4,
            updated_at: 4,
        };
        ports.offers.insert(&offer).await.unwrap();
        assert!(ports.clients.list(PageRequest::default()).await.unwrap().total > 0);
        Self {
            ports,
            client,
            scenario,
            wallet,
            policy,
            offer,
        }
    }
}

crate::storage::repository_entity! { "repository_macro_contract", Catalog;
    keys: ["id"], immutable: ["created_at"], revision: Some("updated_at");
    /// Exercises metadata-driven binding, not a second production row model.
    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
    pub(crate) struct EntityCrudRecord {
        pub id: String,
        #[serde(rename = "label")]
        #[sqlx(rename = "label")]
        pub name: String,
        pub enabled: bool,
        pub count: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub optional_text: Option<String>,
        #[sqlx(json)]
        pub payload: serde_json::Value,
        pub amount: crate::model::Amount,
        pub created_at: i64,
        pub updated_at: i64,
    }
}

macro_rules! entity_crud_contract {
    ($database:expr, $base:ty, $ddl:literal) => {{
        use crate::storage::schema_tests::EntityCrudRecord;
        use crate::storage::{
            EntityComparison, EntityQuery, EntityValue, EntityValues, IGenericStorage, RepositoryEntity,
        };
        let database = $database;
        sqlx::query($ddl).execute(database.pool()).await.unwrap();
        let record = EntityCrudRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            enabled: false,
            count: 0,
            optional_text: Some("original".into()),
            payload: serde_json::Value::Null,
            amount: crate::model::Amount::new(u128::MAX),
            created_at: 10,
            updated_at: 10,
        };
        assert_eq!(database.entity_insert_returning(&record).await.unwrap(), record);
        let filter = EntityQuery::new().eq("id", EntityValue::from(&record.id));
        let page = database
            .entity_page::<EntityCrudRecord>(filter.clone(), crate::storage::PageRequest::default())
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items.as_slice(), std::slice::from_ref(&record));
        assert!(database.entity_insert_returning(&record).await.is_err());
        let injection = "a' ; DROP TABLE pay_client; -- $1 ?";
        let patch = EntityValues::from([
            ("label", injection.into()),
            ("enabled", true.into()),
            ("optional_text", EntityValue::Text(None)),
            ("updated_at", 11_i64.into()),
        ]);
        assert!(database
            .entity_update_returning::<EntityCrudRecord>(
                patch.clone(),
                filter.clone().eq("updated_at", EntityValue::from(999_i64))
            )
            .await
            .unwrap()
            .is_none());
        let changed = database
            .entity_update_returning::<EntityCrudRecord>(
                patch,
                filter.clone().eq("updated_at", EntityValue::from(10_i64)),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(changed.name, injection);
        assert!(changed.enabled);
        assert_eq!(changed.count, 0);
        assert_eq!(changed.optional_text, None);
        assert_eq!(changed.payload, serde_json::Value::Null);
        assert_eq!(changed.amount, crate::model::Amount::new(u128::MAX));
        assert!(database
            .select_one::<EntityCrudRecord>(EntityQuery::new().eq("name", EntityValue::from(injection)))
            .await
            .is_err());
        assert!(database
            .select_one::<EntityCrudRecord>(EntityQuery::new().order("label; DROP TABLE pay_client", false))
            .await
            .is_err());
        assert!(database
            .entity_update_returning::<EntityCrudRecord>(EntityValues::from([("id", "replace".into())]), filter.clone())
            .await
            .is_err());
        assert!(<$base as IGenericStorage>::update::<EntityCrudRecord>(
            EntityValues::from([("enabled", true.into())]),
            &EntityQuery::new()
        )
        .is_err());
        assert!(<$base as IGenericStorage>::delete_by::<crate::model::PaymentLedgerEntry>(&filter).is_err());
        assert!(<$base as IGenericStorage>::update::<crate::model::PaymentEvent>(
            EntityValues::from([("payload", EntityValue::Json(serde_json::json!({})))]),
            &filter
        )
        .is_err());
        assert!(<$base as IGenericStorage>::select::<EntityCrudRecord>(
            &EntityQuery::new().compare(
                "amount",
                EntityComparison::Less,
                EntityValue::from(&crate::model::Amount::new(1))
            ),
            false
        )
        .is_err());
        let mut replacement = changed.clone();
        replacement.name = "replacement".into();
        replacement.updated_at = 12;
        let mut upsert = <$base as IGenericStorage>::upsert(
            &replacement,
            &["id"],
            &["label", "updated_at"],
            &["created_at"],
            "updated_at",
        )
        .unwrap();
        let stored = upsert
            .push(" RETURNING *")
            .build_query_as::<EntityCrudRecord>()
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(stored, replacement);
        let mut replay = <$base as IGenericStorage>::upsert(
            &replacement,
            &["id"],
            &["label", "updated_at"],
            &["created_at"],
            "updated_at",
        )
        .unwrap();
        assert!(replay
            .push(" RETURNING *")
            .build()
            .fetch_optional(database.pool())
            .await
            .unwrap()
            .is_none());
        let mut fresh = replacement.clone();
        fresh.updated_at += 1;
        fresh.name = "new revision".into();
        assert!(
            <$base as crate::storage::IGenericRepository<EntityCrudRecord>>::update(&database, &replacement)
                .await
                .unwrap()
                .is_none()
        );
        let mut wrong_owner = fresh.clone();
        wrong_owner.created_at += 1;
        assert!(
            <$base as crate::storage::IGenericRepository<EntityCrudRecord>>::update(&database, &wrong_owner)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            <$base as crate::storage::IGenericRepository<EntityCrudRecord>>::update(&database, &fresh)
                .await
                .unwrap(),
            Some(fresh)
        );
        replacement.created_at += 1;
        replacement.updated_at += 1;
        let mut invalid = <$base as IGenericStorage>::upsert(
            &replacement,
            &["id"],
            &["label", "updated_at"],
            &["created_at"],
            "updated_at",
        )
        .unwrap();
        assert!(invalid
            .push(" RETURNING *")
            .build()
            .fetch_optional(database.pool())
            .await
            .unwrap()
            .is_none());
        assert!(!EntityCrudRecord::columns().contains(&"name"));
        assert!(EntityCrudRecord::columns().contains(&"label"));
        let mut delete = <$base as IGenericStorage>::delete_by::<EntityCrudRecord>(&filter).unwrap();
        assert_eq!(
            delete.build().execute(database.pool()).await.unwrap().rows_affected(),
            1
        );
        assert!(database.select_one::<EntityCrudRecord>(filter).await.unwrap().is_none());
    }};
}
pub(crate) use entity_crud_contract;

crate::storage::repository_entity! { "repository_catalog_book", Catalog;
    keys: ["id"], immutable: [], revision: None;
    #[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
    pub(crate) struct CatalogBook {
        pub id: String,
        pub title: String,
        pub copies: i64,
        pub available: bool,
        pub borrowed_at: Option<i64>,
    }
}

macro_rules! independent_catalog_contract {
    ($database:expr, $base:ty, $operation:ident, $ddl:literal) => {{
        use crate::storage::schema_tests::CatalogBook;
        use crate::storage::{
            EntityKey, EntityQuery, EntityValues, IGenericRepository, IGenericStorage, PageRequest, StorageSchema,
        };
        let database = $database;
        let schema = StorageSchema {
            version: 7,
            version_table: "repository_catalog_version",
            tables: &["repository_catalog_version", "repository_catalog_book"],
            ddl: $ddl,
            dml: "",
            advisory_lock: 788_142,
            migrations: &[],
        };
        database.migrate(&schema).await.unwrap();
        database.migrate(&schema).await.unwrap();
        let book = CatalogBook {
            id: uuid::Uuid::new_v4().to_string(),
            title: "library".into(),
            copies: 0,
            available: false,
            borrowed_at: None,
        };
        assert_eq!(database.entity_insert_returning(&book).await.unwrap(), book);
        let filter = EntityQuery::new().eq("id", &book.id);
        let updated = database
            .entity_update_returning::<CatalogBook>(
                EntityValues::from([("borrowed_at", Some(9_i64).into())]),
                filter.clone(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.borrowed_at, Some(9));
        let cleared = database
            .entity_update_returning::<CatalogBook>(
                EntityValues::from([("borrowed_at", Option::<i64>::None.into())]),
                filter,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cleared, book);
        // Exercise the public six-operation contract, not SQL builder internals.
        let mut api_book = CatalogBook {
            id: uuid::Uuid::new_v4().to_string(),
            ..book.clone()
        };
        assert_eq!(
            <$base as IGenericRepository<CatalogBook>>::insert(&database, &api_book)
                .await
                .unwrap(),
            api_book
        );
        let key = EntityKey::of(&api_book).unwrap();
        assert_eq!(
            <$base as IGenericRepository<CatalogBook>>::select_by(&database, key.clone())
                .await
                .unwrap(),
            Some(api_book.clone())
        );
        let found = <$base as IGenericRepository<CatalogBook>>::select(&database, &api_book, PageRequest::default())
            .await
            .unwrap();
        assert_eq!(found.total, 1);
        assert_eq!(found.items, vec![api_book.clone()]);
        api_book.title = "updated".into();
        api_book.borrowed_at = Some(17);
        assert_eq!(
            <$base as IGenericRepository<CatalogBook>>::update(&database, &api_book)
                .await
                .unwrap(),
            Some(api_book.clone())
        );
        api_book.borrowed_at = None;
        assert_eq!(
            <$base as IGenericRepository<CatalogBook>>::upsert(&database, &api_book)
                .await
                .unwrap(),
            Some(api_book.clone())
        );
        assert_eq!(
            <$base as IGenericRepository<CatalogBook>>::delete_by(&database, key.clone())
                .await
                .unwrap(),
            1
        );
        assert!(<$base as IGenericRepository<CatalogBook>>::select_by(&database, key)
            .await
            .unwrap()
            .is_none());
        let pending_book = CatalogBook {
            id: uuid::Uuid::new_v4().to_string(),
            ..book.clone()
        };
        let pending_id = pending_book.id.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let pending_database = database.clone();
        let task = tokio::spawn(async move {
            pending_database
                .$operation(move |connection| {
                    Box::pin(async move {
                        <$base as IGenericStorage>::insert(&pending_book, false)?
                            .build()
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| crate::Error::Storage(error.to_string()))?;
                        entered.send(()).unwrap();
                        std::future::pending::<crate::Result<()>>().await
                    })
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let missing = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            database.select_one::<CatalogBook>(EntityQuery::new().eq("id", pending_id)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(missing.is_none(), "cancelled transaction must not commit");
        let next = CatalogBook {
            id: uuid::Uuid::new_v4().to_string(),
            ..book
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            database.entity_insert_returning(&next),
        )
        .await
        .unwrap()
        .unwrap();
    }};
}
pub(crate) use independent_catalog_contract;
