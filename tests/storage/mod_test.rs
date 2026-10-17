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
        assert_eq!(names, ["001_init.ddl.sql", "001_init.dml.sql"]);
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
