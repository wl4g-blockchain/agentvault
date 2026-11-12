use rusqlite::Connection;

use super::{persist_run, ToolResult};

#[test]
fn completed_free_and_paid_results_are_persisted_atomically() {
    let path = std::env::temp_dir().join(format!(
        "agentvault-quantstream-rust-{}-{}.sqlite3",
        std::process::id(),
        super::unix_time()
    ));
    let free = ToolResult {
        body: br#"{"paid":false}"#.to_vec(),
        payment_intent_id: None,
    };
    let paid = ToolResult {
        body: br#"{"paid":true}"#.to_vec(),
        payment_intent_id: Some("intent-rust-1".to_owned()),
    };

    persist_run(path.to_str().expect("UTF-8 path"), "run-rust-1", &free, &paid).expect("persist run");
    let database = Connection::open(&path).expect("open evidence database");
    let row: (String, String) = database
        .query_row("SELECT run_id,payment_intent_id FROM ingestion_run", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read persisted run");
    drop(database);
    std::fs::remove_file(&path).expect("remove test database");

    assert_eq!(row, ("run-rust-1".to_owned(), "intent-rust-1".to_owned()));
}
