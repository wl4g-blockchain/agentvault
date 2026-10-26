use crate::{
    storage::{SchemaMigration, StorageSchema},
    Error, Result,
};

pub(crate) struct PaymentStorageSchema;
impl PaymentStorageSchema {
    const TABLES: &'static [&'static str] = &[
        "pay_schema_version",
        "pay_wallet",
        "pay_wallet_balance",
        "wallet_key_version",
        "pay_client",
        "pay_scenario",
        "pay_scenario_approver",
        "pay_policy",
        "pay_policy_wallet",
        "pay_price",
        "pay_intent",
        "pay_event",
        "pay_approval",
        "pay_ledger",
        "pay_outbox",
    ];
    pub(crate) fn postgres() -> Result<StorageSchema> {
        Ok(StorageSchema {
            version: 2,
            version_table: "pay_schema_version",
            tables: Self::TABLES,
            ddl: Self::body(include_str!("../../migrations/postgres/001_init.ddl.sql"), "BEGIN;")?,
            dml: include_str!("../../migrations/postgres/001_init.dml.sql"),
            advisory_lock: 0x41_56_50_41_59_44_42,
            migrations: &[SchemaMigration {
                from: 1,
                to: 2,
                sql: include_str!("../../migrations/postgres/002_price.ddl.sql"),
            }],
        })
    }
    pub(crate) fn sqlite() -> Result<StorageSchema> {
        Ok(StorageSchema {
            ddl: Self::body(
                include_str!("../../migrations/sqlite/001_init.ddl.sql"),
                "BEGIN IMMEDIATE;",
            )?,
            dml: include_str!("../../migrations/sqlite/001_init.dml.sql"),
            migrations: &[SchemaMigration {
                from: 1,
                to: 2,
                sql: include_str!("../../migrations/sqlite/002_price.ddl.sql"),
            }],
            ..Self::postgres()?
        })
    }
    fn body(ddl: &'static str, begin: &str) -> Result<&'static str> {
        let (_, body) = ddl
            .split_once(begin)
            .ok_or_else(|| Error::Storage("schema transaction preamble missing".into()))?;
        body.trim()
            .strip_suffix("COMMIT;")
            .ok_or_else(|| Error::Storage("schema transaction terminator missing".into()))
    }
}

#[cfg(test)]
#[path = "../../tests/storage/schema_test.rs"]
mod tests;
