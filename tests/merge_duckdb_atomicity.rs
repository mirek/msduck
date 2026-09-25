//! Observations from the exact DuckDB library bundled with this Rust workspace.
//! This test intentionally uses DuckDB SQL directly, not msduck's T-SQL engine.
use duckdb::Connection;

fn query(db: &Connection, sql: &str) -> String {
    let result = (|| -> duckdb::Result<Vec<Vec<String>>> {
        let mut statement = db.prepare(sql)?;
        let mut rows = statement.query([])?;
        let width = rows
            .as_ref()
            .expect("query owns its statement")
            .column_count();
        let mut output = Vec::new();
        while let Some(row) = rows.next()? {
            output.push(
                (0..width)
                    .map(|index| row.get_ref(index).map(|value| format!("{value:?}")))
                    .collect::<duckdb::Result<Vec<_>>>()?,
            );
        }
        Ok(output)
    })();
    match result {
        Ok(rows) => format!("OK {rows:?}"),
        Err(error) => format!("ERR {error}"),
    }
}

fn execute(db: &Connection, sql: &str) -> String {
    match db.execute_batch(sql) {
        Ok(()) => "OK".into(),
        Err(error) => format!("ERR {error}"),
    }
}

fn state(db: &Connection) -> String {
    query(db, "SELECT id,n FROM t ORDER BY id,n")
}

fn pairs(db: &Connection) -> duckdb::Result<Vec<(i32, i32)>> {
    let mut statement = db.prepare("SELECT id,n FROM t ORDER BY id,n")?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn actions(db: &Connection, sql: &str) -> duckdb::Result<Vec<(String, i32, i32)>> {
    let mut statement = db.prepare(sql)?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect()
}

fn base() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY,n INTEGER CHECK(n>0)); INSERT INTO t VALUES(1,10),(2,20),(3,30)")
        .unwrap();
    db
}

#[test]
fn observe_native_merge_actions_and_returning() {
    let db = base();
    db.execute_batch("CREATE TABLE s(id INTEGER,n INTEGER); INSERT INTO s VALUES(1,11),(4,44)")
        .unwrap();
    let version: String = db
        .query_row("SELECT version()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, "v1.5.5");
    println!("version: {version}");
    println!("before: {}", state(&db));
    println!(
        "rowid before: {}",
        query(&db, "SELECT rowid,id,n FROM t ORDER BY id")
    );
    let mut mixed = actions(
        &db,
        "MERGE INTO t USING s ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT(id,n) VALUES(s.id,s.n) WHEN NOT MATCHED BY SOURCE THEN DELETE RETURNING merge_action,id,n",
    ).unwrap();
    println!("mixed: {mixed:?}");
    mixed.sort(); // Native RETURNING order is not the SQL Server contract.
    assert_eq!(
        mixed,
        vec![
            ("DELETE".into(), 2, 20),
            ("DELETE".into(), 3, 30),
            ("INSERT".into(), 4, 44),
            ("UPDATE".into(), 1, 11)
        ]
    );
    assert_eq!(pairs(&db).unwrap(), vec![(1, 11), (4, 44)]);
    println!("after: {}", state(&db));
    println!(
        "rowid after: {}",
        query(&db, "SELECT rowid,id,n FROM t ORDER BY id")
    );
    let old_new = query(
        &db,
        "MERGE INTO t USING (SELECT 1 id,12 n) s ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n RETURNING merge_action, old.n, new.n",
    );
    println!("old/new: {old_new}");
    assert!(old_new.contains("Referenced table \"old\" not found"));
    assert_eq!(pairs(&db).unwrap(), vec![(1, 11), (4, 44)]);
}

#[test]
fn observe_duplicate_source_and_self_reference() {
    let db = base();
    db.execute_batch("CREATE TABLE s(id INTEGER,n INTEGER); INSERT INTO s VALUES(1,11),(1,12)")
        .unwrap();
    let duplicate = execute(
        &db,
        "MERGE INTO t USING s ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n",
    );
    println!("duplicate: {duplicate}");
    assert_eq!(duplicate, "OK"); // SQL Server's captured result is error 8672.
    assert!(matches!(pairs(&db).unwrap()[0], (1, 11 | 12)));
    println!("duplicate state: {}", state(&db));
    let before_self = pairs(&db).unwrap();
    let self_merge = actions(&db, "MERGE INTO t USING (SELECT id,n+1 AS n FROM t) s ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n RETURNING merge_action,id,n").unwrap();
    println!("self: {self_merge:?}");
    assert_eq!(self_merge.len(), 3);
    assert!(self_merge.iter().all(|(kind, _, _)| kind == "UPDATE"));
    println!("self state: {}", state(&db));
    assert_eq!(
        pairs(&db).unwrap(),
        before_self
            .into_iter()
            .map(|(id, value)| (id, value + 1))
            .collect::<Vec<_>>()
    );
}

#[test]
fn observe_constraint_failure_inside_explicit_transaction() {
    let db = base();
    println!("begin: {}", execute(&db, "BEGIN TRANSACTION"));
    println!("prior: {}", execute(&db, "INSERT INTO t VALUES(9,9)"));
    let failure = execute(
        &db,
        "MERGE INTO t USING (VALUES (1,-1),(4,40)) s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT(id,n) VALUES(s.id,s.n)",
    );
    println!("check failure: {failure}");
    assert!(failure.contains("CHECK constraint failed"));
    assert!(state(&db).contains("Current transaction is aborted"));
    assert!(execute(&db, "INSERT INTO t VALUES(8,8)").contains("Current transaction is aborted"));
    println!("after check failure: {}", state(&db));
    println!("later write: {}", execute(&db, "INSERT INTO t VALUES(8,8)"));
    assert_eq!(execute(&db, "COMMIT"), "OK");
    println!("commit: OK");
    println!("committed state: {}", state(&db));
    assert_eq!(pairs(&db).unwrap(), vec![(1, 10), (2, 20), (3, 30)]);
}

#[test]
fn failing_merge_outside_explicit_transaction_leaves_target_unchanged() {
    let db = base();
    let failure = execute(
        &db,
        "MERGE INTO t USING (VALUES (1,-1),(4,40)) s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT(id,n) VALUES(s.id,s.n)",
    );
    assert!(failure.contains("CHECK constraint failed"));
    assert_eq!(pairs(&db).unwrap(), vec![(1, 10), (2, 20), (3, 30)]);
}

#[test]
fn observe_unique_failure_and_rollback() {
    let db = base();
    println!("begin: {}", execute(&db, "BEGIN TRANSACTION"));
    println!("prior: {}", execute(&db, "INSERT INTO t VALUES(9,9)"));
    let failure = execute(
        &db,
        "MERGE INTO t USING (VALUES (4,40),(4,41)) s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT(id,n) VALUES(s.id,s.n)",
    );
    println!("unique failure: {failure}");
    assert!(failure.contains("PRIMARY KEY or UNIQUE constraint violation"));
    assert!(state(&db).contains("Current transaction is aborted"));
    assert!(execute(&db, "INSERT INTO t VALUES(8,8)").contains("Current transaction is aborted"));
    println!("after unique failure: {}", state(&db));
    println!("later write: {}", execute(&db, "INSERT INTO t VALUES(8,8)"));
    println!("rollback: {}", execute(&db, "ROLLBACK"));
    println!("rolled back state: {}", state(&db));
    assert_eq!(pairs(&db).unwrap(), vec![(1, 10), (2, 20), (3, 30)]);
}

#[test]
fn observe_successful_merge_then_transaction_rollback() {
    let db = base();
    println!("begin: {}", execute(&db, "BEGIN TRANSACTION"));
    println!(
        "merge: {}",
        execute(
            &db,
            "MERGE INTO t USING (VALUES (1,11),(4,40)) s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT(id,n) VALUES(s.id,s.n)"
        )
    );
    println!("pending: {}", state(&db));
    assert_eq!(
        pairs(&db).unwrap(),
        vec![(1, 11), (2, 20), (3, 30), (4, 40)]
    );
    println!("rollback: {}", execute(&db, "ROLLBACK"));
    println!("rolled back: {}", state(&db));
    assert_eq!(pairs(&db).unwrap(), vec![(1, 10), (2, 20), (3, 30)]);
}
