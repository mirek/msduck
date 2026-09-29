//! Direct bundled-DuckDB checks for the private IDENTITY_INSERT allocator.
use duckdb::Connection;
use std::path::PathBuf;

const PRIVATE: &str = "main.__msduck_identity_00000000000000000000000000000001";
const NAME: &str = "__msduck_identity_00000000000000000000000000000001";

fn next(db: &Connection) -> duckdb::Result<i64> {
    db.query_row(&format!("SELECT nextval('{PRIVATE}')"), [], |row| {
        row.get(0)
    })
}

fn current(db: &Connection) -> i64 {
    db.query_row(
        &format!("SELECT last_value FROM duckdb_sequences() WHERE sequence_name='{NAME}'"),
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn advance(db: &Connection, value: i64) -> duckdb::Result<bool> {
    db.query_row(
        &format!("SELECT __msduck_identity_advance('{PRIVATE}', ?)"),
        [value],
        |row| row.get(0),
    )
}

fn path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "msduck-identity-advance-{label}-{}-{nanos}.duckdb",
        std::process::id()
    ))
}

#[test]
fn directional_advance_is_atomic_with_nextval_and_rejects_public_sequences() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&format!(
        "CREATE SEQUENCE {PRIVATE} START 10 INCREMENT 2 MINVALUE -100 MAXVALUE 100 NO CYCLE; \
         CREATE SEQUENCE public_seq START 1"
    ))
    .unwrap();
    assert!(!advance(&db, 5).unwrap());
    assert_eq!(next(&db).unwrap(), 10);
    assert!(advance(&db, 50).unwrap());
    assert_eq!(current(&db), 50);
    assert_eq!(
        db.query_row(&format!("SELECT currval('{PRIVATE}')"), [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        50
    );
    assert!(!advance(&db, 40).unwrap());
    assert_eq!(next(&db).unwrap(), 52);
    assert!(advance(&db, 100).unwrap());
    assert!(next(&db).is_err());
    assert!(advance(&db, 101).is_err());
    assert_eq!(current(&db), 100);
    let error = db
        .query_row(
            "SELECT __msduck_identity_advance('public_seq', 8)",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_err();
    assert!(error.to_string().contains("not a private"), "{error}");
    assert_eq!(
        db.query_row("SELECT nextval('public_seq')", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn negative_increment_and_rollback_keep_nontransactional_low_water_mark() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(&format!(
        "CREATE SEQUENCE {PRIVATE} START 0 INCREMENT -2 MINVALUE -100 MAXVALUE 100 NO CYCLE"
    ))
    .unwrap();
    assert_eq!(next(&db).unwrap(), 0);
    assert!(advance(&db, -20).unwrap());
    assert!(!advance(&db, 5).unwrap());
    assert_eq!(next(&db).unwrap(), -22);
    db.execute_batch("BEGIN TRANSACTION").unwrap();
    assert!(advance(&db, -100).unwrap());
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(current(&db), -100);
    assert!(next(&db).is_err());
    assert!(advance(&db, -101).is_err());
    assert_eq!(current(&db), -100);
}

#[test]
fn concurrent_advances_and_reopen_preserve_the_high_water_mark() {
    let file = path("concurrent");
    let db = Connection::open(&file).unwrap();
    db.execute_batch(&format!(
        "CREATE SEQUENCE {PRIVATE} START 1 INCREMENT 1 MINVALUE 1 MAXVALUE 1000 NO CYCLE"
    ))
    .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut threads = Vec::new();
    for value in [100, 200] {
        // A new Connection::open constructs a separate DuckDB instance. The
        // server shares one instance and creates sessions with try_clone.
        let connection = db.try_clone().unwrap();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            advance(&connection, value).unwrap();
        }));
    }
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(current(&db), 200);
    drop(db);
    let db = Connection::open(&file).unwrap();
    assert_eq!(current(&db), 200);
    assert_eq!(next(&db).unwrap(), 201);
    drop(db);
    std::fs::remove_file(file).unwrap();
}

#[test]
fn terminal_bigint_values_remain_allocated_but_exhausted() {
    for (seed, increment, candidate, min, max) in [
        (i64::MAX - 1, 1, i64::MAX, i64::MIN, i64::MAX),
        (i64::MIN + 1, -1, i64::MIN, i64::MIN, i64::MAX),
    ] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "CREATE SEQUENCE {PRIVATE} START {seed} INCREMENT {increment} MINVALUE {min} MAXVALUE {max} NO CYCLE"
        ))
        .unwrap();
        assert!(advance(&db, candidate).unwrap());
        assert_eq!(current(&db), candidate);
        assert!(next(&db).is_err());
        assert_eq!(current(&db), candidate);
    }
}

#[test]
fn unclean_exit_replays_explicit_advance_from_wal() {
    const CHILD: &str = "MSDUCK_IDENTITY_ADVANCE_WAL_CHILD";
    if let Ok(file) = std::env::var(CHILD) {
        let db = Connection::open(file).unwrap();
        db.execute_batch(&format!(
            "CREATE SEQUENCE {PRIVATE} START 1 INCREMENT 1 MINVALUE 1 MAXVALUE 1000 NO CYCLE; CHECKPOINT"
        ))
        .unwrap();
        assert!(advance(&db, 200).unwrap());
        std::process::exit(0);
    }
    let file = path("wal");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["unclean_exit_replays_explicit_advance_from_wal", "--exact"])
        .env(CHILD, &file)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        PathBuf::from(format!("{}.wal", file.display())).exists(),
        "child did not leave a WAL"
    );
    let db = Connection::open(&file).unwrap();
    assert_eq!(current(&db), 200);
    assert_eq!(next(&db).unwrap(), 201);
    drop(db);
    std::fs::remove_file(file).unwrap();
}

#[test]
fn rollback_then_unclean_exit_keeps_explicit_advance() {
    const CHILD: &str = "MSDUCK_IDENTITY_ADVANCE_ROLLBACK_WAL_CHILD";
    if let Ok(file) = std::env::var(CHILD) {
        let db = Connection::open(file).unwrap();
        db.execute_batch(&format!(
            "CREATE SEQUENCE {PRIVATE} START 1 INCREMENT 1 MINVALUE 1 MAXVALUE 1000 NO CYCLE; \
             CREATE TABLE rolled_back(v INT); CHECKPOINT"
        ))
        .unwrap();
        db.execute_batch("BEGIN TRANSACTION").unwrap();
        db.execute_batch("INSERT INTO rolled_back VALUES(7)")
            .unwrap();
        assert!(advance(&db, 200).unwrap());
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(current(&db), 200);
        std::process::exit(0);
    }
    let file = path("rollback-wal");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "rollback_then_unclean_exit_keeps_explicit_advance",
            "--exact",
        ])
        .env(CHILD, &file)
        .status()
        .unwrap();
    assert!(status.success());
    let db = Connection::open(&file).unwrap();
    assert_eq!(current(&db), 200);
    assert_eq!(next(&db).unwrap(), 201);
    assert_eq!(
        db.query_row("SELECT count(*) FROM rolled_back", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_file(file).unwrap();
}

#[test]
fn rolled_back_new_sequence_has_no_standalone_wal_record() {
    const CHILD: &str = "MSDUCK_IDENTITY_ADVANCE_NEW_SEQUENCE_CHILD";
    if let Ok(file) = std::env::var(CHILD) {
        let db = Connection::open(file).unwrap();
        db.execute_batch("CHECKPOINT; BEGIN TRANSACTION").unwrap();
        db.execute_batch(&format!(
            "CREATE SEQUENCE {PRIVATE} START 1 INCREMENT 1 MINVALUE 1 MAXVALUE 1000 NO CYCLE"
        ))
        .unwrap();
        assert!(advance(&db, 200).unwrap());
        db.execute_batch("ROLLBACK").unwrap();
        std::process::exit(0);
    }
    let file = path("new-sequence-wal");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "rolled_back_new_sequence_has_no_standalone_wal_record",
            "--exact",
        ])
        .env(CHILD, &file)
        .status()
        .unwrap();
    assert!(status.success());
    let db = Connection::open(&file).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM duckdb_sequences()", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_file(file).unwrap();
}
