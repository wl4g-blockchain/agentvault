use super::*;
#[test]
fn driver_schema_plans_share_the_same_authoritative_table_set() {
    let postgres = PaymentStorageSchema::postgres().unwrap();
    let sqlite = PaymentStorageSchema::sqlite().unwrap();
    assert_eq!(postgres.version, sqlite.version);
    assert_eq!(postgres.tables, sqlite.tables);
    assert!(postgres.tables.contains(&"pay_intent"));
    assert!(!postgres.ddl.contains("BEGIN;"));
    assert!(!sqlite.ddl.contains("BEGIN IMMEDIATE;"));
}

#[test]
fn migration_plan_rejects_missing_or_forged_history() {
    let schema = PaymentStorageSchema::postgres().unwrap();
    assert!(schema.validate().is_ok());
    assert_eq!(schema.current_version(&[1]).unwrap(), 1);
    assert_eq!(schema.current_version(&[1, 2]).unwrap(), 2);
    for invalid in [&[][..], &[2][..], &[1, 1][..], &[1, 3][..]] {
        assert!(schema.current_version(invalid).is_err());
    }
    let invalid = crate::storage::StorageSchema {
        migrations: &[crate::storage::SchemaMigration {
            from: 0,
            to: 2,
            sql: "SELECT 1",
        }],
        ..schema
    };
    assert!(invalid.validate().is_err());
}

// Historical v1 wire SQL is intentionally preserved. The upgrade changes only
// the pricing relation, never the intent/event/ledger records already in it.
const LEGACY_ROWS: &str = r#"
INSERT INTO pay_client(id,name,auth_principal,enabled,created_at,updated_at)
VALUES('legacy-client','Legacy','legacy-principal',TRUE,1,1);
INSERT INTO pay_scenario(id,client_id,name,enabled,created_at,updated_at)
VALUES('legacy-scenario','legacy-client','Legacy',TRUE,1,1);
INSERT INTO pay_wallet(id,custody_type,algorithm,address,budget_asset,budget_decimals,enabled,created_at,updated_at)
VALUES('legacy-wallet','external','external','0x1111111111111111111111111111111111111111','USDC',6,TRUE,1,1);
INSERT INTO pay_policy(id,scenario_id,protocols,networks,assets,budget_asset,budget_decimals,
allowed_recipients,allowed_origins,inbound_max_per_payment,inbound_max_per_day,enabled,created_at,updated_at)
VALUES('legacy-policy','legacy-scenario','["x402"]','["eip155:31337"]',
'[{"network":"eip155:31337","asset":"USDC","token_contract":"0x2222222222222222222222222222222222222222","decimals":6}]',
'USDC',6,'["0x1111111111111111111111111111111111111111"]','["https://legacy.test"]','100','1000',TRUE,1,1);
INSERT INTO pay_policy_wallet(policy_id,wallet_id,created_at) VALUES('legacy-policy','legacy-wallet',1);
INSERT INTO pay_price(id,scenario_id,policy_id,resource,protocols,network,asset,token_contract,asset_decimals,
amount,wallet_id,enabled,created_at,updated_at)
VALUES('legacy-price','legacy-scenario','legacy-policy','https://legacy.test/resource','["x402"]',
'eip155:31337','USDC','0x2222222222222222222222222222222222222222',6,'10','legacy-wallet',TRUE,1,1);
INSERT INTO pay_intent(id,scenario_id,run_id,wallet_id,policy_id,offer_id,direction,protocol,network,asset,
token_contract,asset_decimals,recipient,amount,settled_amount,resource,challenge_hash,credential_hash,request_hash,
status,reservation_status,reserved_amount,tx_hash,settlement_ref,created_at,updated_at,settled_at)
VALUES('legacy-intent','legacy-scenario','legacy-run','legacy-wallet','legacy-policy','legacy-price','inbound','x402',
'eip155:31337','USDC','0x2222222222222222222222222222222222222222',6,
'0x1111111111111111111111111111111111111111','10','10','https://legacy.test/resource',
'legacy-challenge','legacy-credential','legacy-request','settled','consumed','10',
'0x4444444444444444444444444444444444444444444444444444444444444444','legacy-settlement',1,2,2);
INSERT INTO pay_event(id,intent_id,event_type,attempt_no,payload,created_at)
VALUES('legacy-event','legacy-intent','settled',1,'{"settled":true}',2);
INSERT INTO pay_ledger(id,intent_id,scenario_id,wallet_id,entry_type,direction,network,asset,token_contract,asset_decimals,
amount,posting_key,tx_hash,settlement_ref,effective_at,created_at)
VALUES('legacy-ledger','legacy-intent','legacy-scenario','legacy-wallet','payment','inbound','eip155:31337','USDC',
'0x2222222222222222222222222222222222222222',6,'10','legacy:posting',
'0x4444444444444444444444444444444444444444444444444444444444444444','legacy-settlement',2,2);
"#;

#[tokio::test]
async fn sqlite_price_upgrade_keeps_financial_history_and_foreign_keys() {
    use crate::storage::sqlite::SqliteRepository;
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let database = SqliteRepository::new(
        &directory.path().join("legacy.sqlite").display().to_string(),
        2,
        Duration::from_secs(5),
    )
    .unwrap();
    let schema = PaymentStorageSchema::sqlite().unwrap();
    let mut transaction = database.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::raw_sql(schema.ddl).execute(&mut *transaction).await.unwrap();
    sqlx::raw_sql(LEGACY_ROWS).execute(&mut *transaction).await.unwrap();
    sqlx::query("PRAGMA user_version = 1")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    database.migrate(&schema).await.unwrap();
    database.migrate(&schema).await.unwrap();
    let versions: Vec<i32> = sqlx::query_scalar("SELECT version FROM pay_schema_version ORDER BY version")
        .fetch_all(database.pool())
        .await
        .unwrap();
    assert_eq!(versions, [1, 2]);
    for (query, value) in [
        ("SELECT id FROM pay_price", "legacy-price"),
        ("SELECT offer_id FROM pay_intent", "legacy-price"),
        ("SELECT intent_id FROM pay_event", "legacy-intent"),
        ("SELECT CAST(amount AS TEXT) FROM pay_ledger", "10"),
        ("SELECT posting_key FROM pay_ledger", "legacy:posting"),
    ] {
        let actual: String = sqlx::query_scalar(query).fetch_one(database.pool()).await.unwrap();
        assert_eq!(actual, value);
    }
    let broken: i64 = sqlx::query_scalar("SELECT count(*) FROM pragma_foreign_key_check")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(broken, 0);
    assert!(sqlx::query("DELETE FROM pay_price WHERE id='legacy-price'")
        .execute(database.pool())
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires a dedicated AGENTVAULT_TEST_POSTGRES_URL database"]
async fn postgres_price_upgrade_keeps_financial_history_and_foreign_keys() {
    use crate::storage::postgres::PostgresRepository;
    let database = crate::storage::postgres::tests::storage().await;
    let schema = PaymentStorageSchema::postgres().unwrap();
    let mut transaction = database.pool().begin().await.unwrap();
    let result: crate::Result<()> = async {
        let connection = &mut *transaction;
        // An isolated transactional schema lets other contract tests retain
        // their public data while the actual PostgreSQL migration is exercised.
        let namespace = format!("migration_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {namespace}")).execute(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        sqlx::query(&format!("SET LOCAL search_path TO {namespace}, public")).execute(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        sqlx::raw_sql(schema.ddl).execute(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        sqlx::raw_sql(LEGACY_ROWS).execute(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        PostgresRepository::migrate_locked(connection,&schema).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        PostgresRepository::migrate_locked(connection,&schema).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        let versions: Vec<i32> = sqlx::query_scalar("SELECT version FROM pay_schema_version ORDER BY version")
            .fetch_all(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        assert_eq!(versions,[1,2]);
        for (query, value) in [
            ("SELECT id FROM pay_price", "legacy-price"),
            ("SELECT offer_id FROM pay_intent", "legacy-price"),
            ("SELECT intent_id FROM pay_event", "legacy-intent"),
            ("SELECT CAST(amount AS TEXT) FROM pay_ledger", "10"),
            ("SELECT posting_key FROM pay_ledger", "legacy:posting"),
        ] {
            let actual: String = sqlx::query_scalar(query).fetch_one(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
            assert_eq!(actual,value);
        }
        let foreign_keys: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint WHERE contype='f' AND conrelid='pay_intent'::regclass AND confrelid='pay_price'::regclass")
            .fetch_one(&mut *connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        assert!(foreign_keys>0);
        sqlx::query(&format!("DROP SCHEMA {namespace} CASCADE")).execute(connection).await.map_err(|error| crate::Error::Storage(error.to_string()))?;
        Ok(())
    }.await;
    result.unwrap();
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn failed_sqlite_migration_rolls_back_schema_history_and_user_version() {
    use crate::storage::{sqlite::SqliteRepository, SchemaMigration, StorageSchema};
    let directory = tempfile::tempdir().unwrap();
    let database = SqliteRepository::new(
        &directory.path().join("rollback.sqlite").display().to_string(),
        2,
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let legacy = StorageSchema {
        version: 1,
        version_table: "migration_versions",
        tables: &["migration_versions", "migration_source"],
        ddl: "CREATE TABLE migration_versions(version INTEGER PRIMARY KEY); INSERT INTO migration_versions VALUES(1); CREATE TABLE migration_source(id INTEGER PRIMARY KEY); INSERT INTO migration_source VALUES(7);",
        dml: "",
        advisory_lock: 72,
        migrations: &[],
    };
    database.migrate(&legacy).await.unwrap();
    let broken = StorageSchema {
        version: 2,
        tables: &["migration_versions", "migration_target"],
        migrations: &[SchemaMigration {
            from: 1,
            to: 2,
            sql: "ALTER TABLE migration_source RENAME TO migration_target; INSERT INTO migration_versions VALUES(2); INSERT INTO missing_relation VALUES(1);",
        }],
        ..legacy
    };
    assert!(database.migrate(&broken).await.is_err());
    let versions: Vec<i32> = sqlx::query_scalar("SELECT version FROM migration_versions ORDER BY version")
        .fetch_all(database.pool())
        .await
        .unwrap();
    assert_eq!(versions, [1]);
    let version: i32 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(version, 1);
    let id: i32 = sqlx::query_scalar("SELECT id FROM migration_source")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(id, 7);
    let upgraded = StorageSchema {
        migrations: &[SchemaMigration {
            from: 1,
            to: 2,
            sql: "ALTER TABLE migration_source RENAME TO migration_target; INSERT INTO migration_versions VALUES(2);",
        }],
        ..broken
    };
    database.migrate(&upgraded).await.unwrap();
    database.migrate(&upgraded).await.unwrap();
    let id: i32 = sqlx::query_scalar("SELECT id FROM migration_target")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(id, 7);
}

#[tokio::test]
#[ignore = "requires a dedicated AGENTVAULT_TEST_POSTGRES_URL database"]
async fn postgres_migration_does_not_reuse_another_schemas_public_tables() {
    use crate::storage::postgres::PostgresRepository;
    let database = crate::storage::postgres::tests::storage().await;
    let schema = PaymentStorageSchema::postgres().unwrap();
    let mut transaction = database.pool().begin().await.unwrap();
    let namespace = format!("migration_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {namespace}"))
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query(&format!("SET LOCAL search_path TO {namespace}, public"))
        .execute(&mut *transaction)
        .await
        .unwrap();
    PostgresRepository::migrate_locked(&mut transaction, &schema)
        .await
        .unwrap();
    let own_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema=$1 AND table_name='pay_price'",
    )
    .bind(&namespace)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(
        own_table, 1,
        "a different public schema must never count as this repository's schema"
    );
    transaction.rollback().await.unwrap();
}
