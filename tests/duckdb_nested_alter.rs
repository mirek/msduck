//! Direct bundled-DuckDB regressions, independent of msduck's SQL translator.
use duckdb::Connection;

fn has_column(db: &Connection, name: &str) -> bool {
    let mut statement = db.prepare("SELECT * FROM x LIMIT 0").unwrap();
    statement.query([]).unwrap().next().unwrap();
    statement
        .column_names()
        .iter()
        .any(|column| column.eq_ignore_ascii_case(name))
}

#[test]
fn direct_add_of_constant_defaults_then_not_null_commits() {
    for (kind, default) in [
        ("INTEGER", "42"),
        ("VARCHAR", "'x'"),
        ("BLOB", "from_hex('7800')"),
        ("STRUCT(u INTEGER)", "{'u':42}"),
        (
            "STRUCT(u BLOB)",
            "CAST(row(from_hex('780020002000')) AS STRUCT(u BLOB))",
        ),
        ("INTEGER[]", "[42]"),
    ] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE x(id INTEGER)").unwrap();
        db.execute_batch("INSERT INTO x VALUES(1)").unwrap();
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        assert!(!has_column(&db, "s"));
        db.execute_batch(&format!(
            "ALTER TABLE x ADD COLUMN IF NOT EXISTS s {kind} DEFAULT {default}"
        ))
        .unwrap();
        db.execute_batch("ALTER TABLE x ALTER COLUMN s SET NOT NULL")
            .unwrap();
        db.execute_batch("COMMIT").unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM x WHERE s IS NOT NULL", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(db.execute_batch("INSERT INTO x VALUES(2,NULL)").is_err());
        db.execute_batch("INSERT INTO x(id) VALUES(2)").unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM x WHERE s IS NOT NULL", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}

#[test]
fn direct_add_rolls_back_and_real_nulls_still_reject_not_null() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE x(id INT);INSERT INTO x VALUES(1)")
        .unwrap();
    db.execute_batch("BEGIN;ALTER TABLE x ADD COLUMN IF NOT EXISTS s STRUCT(u BLOB) DEFAULT {'u':from_hex('7800')};ALTER TABLE x ALTER COLUMN s SET NOT NULL;ROLLBACK").unwrap();
    assert!(!has_column(&db, "s"));
    db.execute_batch("BEGIN;ALTER TABLE x ADD COLUMN IF NOT EXISTS s STRUCT(u BLOB)")
        .unwrap();
    let error = db
        .execute_batch("ALTER TABLE x ALTER COLUMN s SET NOT NULL")
        .unwrap_err();
    assert!(
        error.to_string().contains("NOT NULL constraint failed"),
        "{error}"
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert!(!has_column(&db, "s"));
}

#[test]
fn absence_check_does_not_hide_a_competing_catalog_writer() {
    let first = Connection::open_in_memory().unwrap();
    first
        .execute_batch("CREATE TABLE x(id INT);INSERT INTO x VALUES(1)")
        .unwrap();
    let second = first.try_clone().unwrap();
    first.execute_batch("BEGIN").unwrap();
    assert!(!has_column(&first, "s"));
    second
        .execute_batch("ALTER TABLE x ADD COLUMN s INTEGER DEFAULT 7")
        .unwrap();
    let error=first.execute_batch("ALTER TABLE x ADD COLUMN IF NOT EXISTS s STRUCT(u BLOB) DEFAULT {'u':from_hex('7800')}").unwrap_err();
    assert!(
        error.to_string().contains("Conflict") || error.to_string().contains("conflict"),
        "{error}"
    );
    first.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        second
            .query_row("SELECT s FROM x", [], |r| r.get::<_, i32>(0))
            .unwrap(),
        7
    );
}

#[test]
fn direct_constant_struct_default_survives_reopen() {
    let path = std::env::temp_dir().join(format!(
        "msduck-duckdb-alter-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE x(id INT);INSERT INTO x VALUES(1)")
            .unwrap();
        db.execute_batch("BEGIN;ALTER TABLE x ADD COLUMN IF NOT EXISTS s STRUCT(u BLOB) DEFAULT {'u':from_hex('3ed8')};ALTER TABLE x ALTER COLUMN s SET NOT NULL;COMMIT").unwrap();
    }
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("INSERT INTO x(id) VALUES(2)").unwrap();
        let values = db
            .prepare("SELECT s.u FROM x ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(values, vec![vec![0x3e, 0xd8]; 2]);
    }
    std::fs::remove_file(path).unwrap();
}
