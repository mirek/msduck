//! Isolation levels, savepoints, named transactions, WAITFOR and DBCC
//! USEROPTIONS in process (issue #727). Expected values come from
//! reference/savepoint.json and reference/gaps-transactions.json; see
//! docs/gaps-transactions.md. Client-level coverage is in
//! tests/compat/transactions.test.mjs.
use msduck::{
    engine::Session,
    read_cancellation::{Mode, Outcome},
    server::Server,
    tds::{BeginTransaction, TransactionRequest},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn session(server: &Server) -> Session {
    Session::new(server.connection().unwrap()).unwrap()
}

/// Run a batch; `Err` carries the last error number.
fn run(session: &mut Session, sql: &str) -> Result<Vec<u8>, i32> {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    if ok {
        Ok(tokens)
    } else {
        Err(session.last_error)
    }
}

fn ints(session: &Session, sql: &str) -> Vec<i64> {
    session
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap()
}

fn pairs(session: &Session, sql: &str) -> Vec<(i64, Option<String>)> {
    session
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap()
}

fn isolation(session: &Session) -> i16 {
    session
        .db
        .query_row(
            "SELECT transaction_isolation_level FROM sys.dm_exec_sessions WHERE session_id = ?",
            [session.spid()],
            |row| row.get(0),
        )
        .unwrap()
}

fn contains_utf16(tokens: &[u8], text: &str) -> bool {
    let needle = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    tokens.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn rollback_to_a_savepoint_restores_rows_and_keeps_the_transaction() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE dbo.keyed(id INT IDENTITY(1,1) CONSTRAINT pk_keyed PRIMARY KEY, v INT NOT NULL, doubled AS v * 2);
         CREATE TABLE dbo.heap(a INT, b VARCHAR(10));
         CREATE TABLE dbo.parent(id INT PRIMARY KEY, name NVARCHAR(10));
         CREATE TABLE dbo.child(id INT PRIMARY KEY, parent_id INT REFERENCES dbo.parent(id))",
    )
    .unwrap();
    run(
        &mut s,
        "BEGIN TRAN;
         INSERT dbo.keyed(v) VALUES (1), (2);
         INSERT dbo.heap VALUES (1, 'a'), (1, 'a'), (2, 'b');
         INSERT dbo.parent VALUES (1, N'one');
         SAVE TRANSACTION s1;
         INSERT dbo.keyed(v) VALUES (3);
         UPDATE dbo.keyed SET v = 10 WHERE id = 1;
         DELETE dbo.keyed WHERE id = 2;
         DELETE dbo.heap WHERE a = 1; INSERT dbo.heap VALUES (1, 'a');
         UPDATE dbo.heap SET b = 'B' WHERE a = 2;
         INSERT dbo.heap VALUES (3, 'c');
         UPDATE dbo.parent SET name = N'ONE';
         INSERT dbo.parent VALUES (2, N'two');
         INSERT dbo.child VALUES (1, 1), (2, 2)",
    )
    .unwrap();
    assert_eq!(s.transactions, 1);
    run(&mut s, "ROLLBACK TRANSACTION s1").unwrap();
    assert_eq!(s.transactions, 1, "@@TRANCOUNT is unchanged");
    assert_eq!(
        pairs(
            &s,
            "SELECT id, CAST(v AS VARCHAR) FROM dbo.keyed ORDER BY id"
        ),
        [(1, Some("1".into())), (2, Some("2".into()))]
    );
    assert_eq!(
        ints(&s, "SELECT doubled FROM dbo.keyed ORDER BY id"),
        [2, 4]
    );
    assert_eq!(
        pairs(&s, "SELECT a, b FROM dbo.heap ORDER BY a, b"),
        [
            (1, Some("a".into())),
            (1, Some("a".into())),
            (2, Some("b".into()))
        ]
    );
    // NVARCHAR values (stored as UTF-16 carriers) are restored exactly,
    // including a change of case only.
    assert_eq!(ints(&s, "SELECT id FROM dbo.parent"), [1]);
    run(
        &mut s,
        "IF (SELECT CAST(name AS VARBINARY(20)) FROM dbo.parent) <> CAST(N'one' AS VARBINARY(20)) THROW 51000, 'parent name not restored', 1",
    )
    .unwrap();
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.child"), [0]);
    // Rolling back consumes the savepoint (6401, state 1).
    assert_eq!(run(&mut s, "ROLLBACK TRANSACTION s1"), Err(6401));
    assert_eq!(s.transactions, 1);
    // Identity values are not restored: the next row gets 4.
    run(&mut s, "INSERT dbo.keyed(v) VALUES (4)").unwrap();
    assert_eq!(ints(&s, "SELECT id FROM dbo.keyed ORDER BY id"), [1, 2, 4]);
    run(&mut s, "COMMIT").unwrap();
    assert_eq!(s.transactions, 0);
    assert_eq!(ints(&s, "SELECT id FROM dbo.keyed ORDER BY id"), [1, 2, 4]);
    // Temporary copies are gone with the transaction.
    assert_eq!(
        ints(
            &s,
            "SELECT count(*) FROM duckdb_tables() WHERE temporary AND table_name LIKE '__msduck_savepoint%'"
        ),
        [0]
    );
}

#[test]
fn savepoint_names_order_and_nesting_follow_sql_server() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    // Duplicate names reach the newest remaining savepoint; a rollback to
    // an earlier savepoint removes later ones; names compare case
    // insensitively and ignore trailing blanks.
    run(
        &mut s,
        "BEGIN TRAN outer_tx;
         INSERT dbo.t VALUES (1); SAVE TRAN d;
         INSERT dbo.t VALUES (2); SAVE TRAN d;
         INSERT dbo.t VALUES (3); SAVE TRAN [MixedCase  ];
         INSERT dbo.t VALUES (4); ROLLBACK TRAN mixedcase;
         ROLLBACK TRAN d",
    )
    .unwrap();
    assert_eq!(ints(&s, "SELECT n FROM dbo.t ORDER BY n"), [1, 2]);
    run(&mut s, "ROLLBACK TRAN D").unwrap();
    assert_eq!(ints(&s, "SELECT n FROM dbo.t ORDER BY n"), [1]);
    assert_eq!(run(&mut s, "ROLLBACK TRAN d"), Err(6401));
    // A savepoint taken inside a nested BEGIN survives the inner COMMIT.
    run(
        &mut s,
        "BEGIN TRAN; SAVE TRAN inner_sp; INSERT dbo.t VALUES (5); COMMIT",
    )
    .unwrap();
    assert_eq!(s.transactions, 1);
    run(&mut s, "ROLLBACK TRAN inner_sp").unwrap();
    assert_eq!(ints(&s, "SELECT n FROM dbo.t ORDER BY n"), [1]);
    // A savepoint named like the outer transaction is reached first; the
    // second rollback ends the transaction.
    run(&mut s, "SAVE TRAN outer_tx; INSERT dbo.t VALUES (6)").unwrap();
    run(&mut s, "ROLLBACK TRAN outer_tx").unwrap();
    assert_eq!(s.transactions, 1);
    assert_eq!(ints(&s, "SELECT n FROM dbo.t ORDER BY n"), [1]);
    // Transaction names compare case sensitively.
    assert_eq!(run(&mut s, "ROLLBACK TRAN OUTER_TX"), Err(6401));
    run(&mut s, "ROLLBACK TRAN outer_tx").unwrap();
    assert_eq!(s.transactions, 0);
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.t"), [0]);
    // Only the outermost name can be rolled back.
    run(&mut s, "BEGIN TRAN a1; BEGIN TRAN b1").unwrap();
    assert_eq!(run(&mut s, "ROLLBACK TRAN b1"), Err(6401));
    assert_eq!(s.transactions, 2);
    run(&mut s, "ROLLBACK TRAN a1").unwrap();
    assert_eq!(s.transactions, 0);
}

#[test]
fn savepoint_errors_and_variable_names() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    // 628 ends the batch; the INSERT before it committed.
    assert_eq!(
        run(
            &mut s,
            "INSERT dbo.t VALUES (1); SAVE TRAN s; INSERT dbo.t VALUES (2)"
        ),
        Err(628)
    );
    assert_eq!(ints(&s, "SELECT n FROM dbo.t"), [1]);
    run(
        &mut s,
        "BEGIN TRY SAVE TRAN s END TRY BEGIN CATCH IF ERROR_NUMBER() <> 628 OR ERROR_STATE() <> 0 THROW 51000, 'expected 628', 1 END CATCH",
    )
    .unwrap();
    // A literal name longer than 32 characters fails while compiling (103),
    // so nothing in the batch runs.
    let long = "n".repeat(33);
    assert_eq!(
        run(
            &mut s,
            &format!("INSERT dbo.t VALUES (2); SAVE TRAN {long}")
        ),
        Err(103)
    );
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.t"), [1]);
    run(
        &mut s,
        &format!("BEGIN TRAN; SAVE TRAN {}; ROLLBACK", "n".repeat(32)),
    )
    .unwrap();
    // Variable names are truncated to 32 characters.
    run(
        &mut s,
        "DECLARE @n NVARCHAR(64) = REPLICATE(N'x', 40);
         BEGIN TRAN; SAVE TRAN @n; INSERT dbo.t VALUES (2);
         ROLLBACK TRAN xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    )
    .unwrap();
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.t"), [1]);
    run(&mut s, "ROLLBACK").unwrap();
    // A non-character name is 3914.
    assert_eq!(
        run(&mut s, "DECLARE @i INT = 5; BEGIN TRAN; SAVE TRAN @i"),
        Err(3914)
    );
    run(&mut s, "ROLLBACK").unwrap();
    // An empty name saves, but rolling back to it is 6401 with state 2.
    run(
        &mut s,
        "DECLARE @e NVARCHAR(10) = N''; BEGIN TRAN; SAVE TRAN @e;
         BEGIN TRY ROLLBACK TRAN @e END TRY
         BEGIN CATCH IF ERROR_NUMBER() <> 6401 OR ERROR_STATE() <> 2 THROW 51000, 'expected 6401 state 2', 1 END CATCH",
    )
    .unwrap();
    assert_eq!(s.transactions, 1);
    // A NULL name saves nothing, and ROLLBACK to NULL ends the transaction.
    run(
        &mut s,
        "DECLARE @z NVARCHAR(10) = NULL; BEGIN TRAN; SAVE TRAN @z; ROLLBACK TRAN @z",
    )
    .unwrap();
    assert_eq!(s.transactions, 0);
    // In a doomed transaction, SAVE is 3930 and ROLLBACK to a savepoint 3931.
    run(&mut s, "CREATE TABLE dbo.d(i INT, c VARCHAR(1))").unwrap();
    for (statement, number) in [("SAVE TRAN later", 3930), ("ROLLBACK TRAN s", 3931)] {
        let sql = format!(
            "SET XACT_ABORT ON; BEGIN TRAN; SAVE TRAN s;
             BEGIN TRY INSERT dbo.d VALUES (1, 'long') END TRY
             BEGIN CATCH
               IF XACT_STATE() <> -1 THROW 51000, 'expected doomed', 1;
               BEGIN TRY {statement} END TRY
               BEGIN CATCH IF ERROR_NUMBER() <> {number} THROW 51001, 'unexpected error', 1 END CATCH;
               ROLLBACK
             END CATCH; SET XACT_ABORT OFF"
        );
        run(&mut s, &sql).unwrap();
        assert_eq!(s.transactions, 0, "{statement}");
    }
}

#[test]
fn schema_changes_after_a_savepoint_fail_explicitly() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    run(&mut s, "BEGIN TRAN; INSERT dbo.t VALUES (1); SAVE TRAN s").unwrap();
    for sql in [
        "CREATE TABLE dbo.later(n INT)",
        "SELECT n INTO dbo.copied FROM dbo.t",
        "DROP TABLE dbo.t",
    ] {
        assert_eq!(run(&mut s, sql), Err(40515), "{sql}");
        assert_eq!(s.transactions, 1, "{sql}");
    }
    // The transaction stays usable and committable.
    run(&mut s, "INSERT dbo.t VALUES (2); ROLLBACK TRAN s; COMMIT").unwrap();
    assert_eq!(ints(&s, "SELECT n FROM dbo.t"), [1]);
    // Without a savepoint, DDL in a transaction is unaffected.
    run(
        &mut s,
        "BEGIN TRAN; CREATE TABLE dbo.later(n INT); ROLLBACK",
    )
    .unwrap();
}

#[test]
fn transaction_manager_savepoints() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    // Without a transaction, a save request is 628.
    assert!(
        s.transaction_request(TransactionRequest::Save { name: "x".into() })
            .is_err()
    );
    s.transaction_request(TransactionRequest::Begin(BeginTransaction {
        isolation: 4,
        name: "tm".into(),
    }))
    .unwrap();
    run(&mut s, "INSERT dbo.t VALUES (1)").unwrap();
    s.transaction_request(TransactionRequest::Save {
        name: "MixedTm".into(),
    })
    .unwrap();
    run(&mut s, "INSERT dbo.t VALUES (2)").unwrap();
    // SQL and TM savepoints share one namespace; TM names use the
    // database collation.
    s.transaction_request(TransactionRequest::Rollback {
        name: "MIXEDTM".into(),
        restart: None,
    })
    .unwrap();
    assert_eq!(s.transactions, 1);
    assert_eq!(ints(&s, "SELECT n FROM dbo.t"), [1]);
    assert!(
        s.transaction_request(TransactionRequest::Rollback {
            name: "MixedTm".into(),
            restart: None,
        })
        .is_err()
    );
    // A 33-character name is 103 (state 30), not truncated.
    let error = s
        .transaction_request(TransactionRequest::Save {
            name: "n".repeat(33),
        })
        .unwrap_err();
    let error = error
        .downcast_ref::<msduck_core::diagnostic::SqlError>()
        .unwrap();
    assert_eq!((error.number, error.state), (103, 30));
    // An empty name is 3977 and rolls back the whole transaction.
    let error = s
        .transaction_request(TransactionRequest::Save {
            name: String::new(),
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The savepoint name cannot be NULL. The batch has been aborted."
    );
    assert_eq!(s.transactions, 0);
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.t"), [0]);
}

#[test]
fn isolation_levels_are_accepted_and_reported() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    assert_eq!(isolation(&s), 2);
    for (level, name, value) in [
        ("READ UNCOMMITTED", "read uncommitted", 1),
        ("REPEATABLE READ", "repeatable read", 3),
        ("SERIALIZABLE", "serializable", 4),
        ("SNAPSHOT", "snapshot", 5),
        ("READ COMMITTED", "read committed", 2),
    ] {
        run(&mut s, &format!("SET TRANSACTION ISOLATION LEVEL {level}")).unwrap();
        assert_eq!(isolation(&s), value, "{level}");
        let tokens = run(&mut s, "DBCC USEROPTIONS").unwrap();
        assert!(contains_utf16(&tokens, "isolation level"), "{level}");
        assert!(contains_utf16(&tokens, name), "{level}");
        assert!(
            contains_utf16(
                &tokens,
                "DBCC execution completed. If DBCC printed error messages, contact your system administrator."
            ),
            "{level}"
        );
        // Every level runs on DuckDB's snapshot isolation.
        run(&mut s, "BEGIN TRAN; SELECT 1; COMMIT").unwrap();
    }
    // A level set inside a transaction stays after it ends.
    run(
        &mut s,
        "BEGIN TRAN; SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; COMMIT",
    )
    .unwrap();
    assert_eq!(isolation(&s), 4);
    let tokens = run(
        &mut s,
        "SET NOCOUNT ON; SET XACT_ABORT ON; SET ANSI_WARNINGS OFF; SET DATEFIRST 3; DBCC USEROPTIONS WITH NO_INFOMSGS",
    )
    .unwrap();
    for option in ["nocount", "xact_abort", "serializable"] {
        assert!(contains_utf16(&tokens, option), "{option}");
    }
    assert!(!contains_utf16(&tokens, "ansi_warnings"));
    assert!(!contains_utf16(&tokens, "DBCC execution completed"));
    assert!(
        run(&mut s, "SET TRANSACTION ISOLATION LEVEL READ ONLY").is_err(),
        "only isolation levels are supported"
    );
}

#[test]
fn transaction_manager_begin_accepts_every_level() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    for level in [4, 0, 1, 3, 5, 2] {
        s.transaction_request(TransactionRequest::Begin(BeginTransaction {
            isolation: level,
            name: String::new(),
        }))
        .unwrap();
        // The level chosen at begin is the session's (0 keeps it).
        let expected = if level == 0 { 4 } else { level };
        assert_eq!(isolation(&s), i16::from(expected));
        s.transaction_request(TransactionRequest::Commit { restart: None })
            .unwrap();
        assert_eq!(isolation(&s), i16::from(expected));
    }
    // A level set inside an RPC reverts when it returns; a SQL batch keeps it.
    let (_, ok) = s.batch_response(
        "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
        &Default::default(),
        true,
        None,
    );
    assert!(ok);
    assert_eq!(isolation(&s), 2);
    run(&mut s, "SET TRANSACTION ISOLATION LEVEL SNAPSHOT").unwrap();
    assert_eq!(isolation(&s), 5);
    assert!(
        s.transaction_request(TransactionRequest::Begin(BeginTransaction {
            isolation: 6,
            name: String::new(),
        }))
        .is_err()
    );
    assert_eq!(s.transactions, 0);
}

#[test]
fn named_transactions_in_sql() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    run(
        &mut s,
        "BEGIN TRAN t1 WITH MARK 'nightly'; INSERT dbo.t VALUES (1); COMMIT TRAN unrelated_name",
    )
    .unwrap();
    assert_eq!(s.transactions, 0);
    run(
        &mut s,
        "DECLARE @name VARCHAR(10) = 'vt'; BEGIN TRAN @name; INSERT dbo.t VALUES (2); ROLLBACK TRAN @name",
    )
    .unwrap();
    assert_eq!(s.transactions, 0);
    assert_eq!(ints(&s, "SELECT n FROM dbo.t"), [1]);
    assert_eq!(
        run(&mut s, &format!("BEGIN TRAN {}", "n".repeat(33))),
        Err(103)
    );
    assert_eq!(s.transactions, 0);
}

#[test]
fn waitfor_delay_and_time_wait_and_validate() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    let started = Instant::now();
    run(&mut s, "WAITFOR DELAY '00:00:00.250'").unwrap();
    assert!(started.elapsed() >= Duration::from_millis(250));
    let started = Instant::now();
    run(
        &mut s,
        "DECLARE @d VARCHAR(20) = '00:00:00:200'; WAITFOR DELAY @d",
    )
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    // NULL and the empty string do not wait.
    let started = Instant::now();
    run(
        &mut s,
        "DECLARE @d VARCHAR(20) = NULL; WAITFOR DELAY @d; WAITFOR DELAY ''; WAITFOR TIME @d",
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    // An int is a number of seconds.
    let started = Instant::now();
    run(&mut s, "DECLARE @d INT = 1; WAITFOR DELAY @d").unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    // WAITFOR TIME with seconds after midnight, one second ahead.
    let started = Instant::now();
    run(
        &mut s,
        "DECLARE @t INT = DATEDIFF(SECOND, CAST(CAST(GETDATE() AS DATE) AS DATETIME), GETDATE()) + 1; WAITFOR TIME @t",
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    // An invalid literal is 148 while compiling: nothing runs.
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    assert_eq!(
        run(&mut s, "INSERT dbo.t VALUES (1); WAITFOR DELAY '25:00'"),
        Err(148)
    );
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.t"), [0]);
    // Invalid variables fail when the statement runs.
    for (sql, number) in [
        ("DECLARE @d VARCHAR(20) = 'abc'; WAITFOR DELAY @d", 241),
        (
            "DECLARE @d VARCHAR(MAX) = '00:00:00.100'; WAITFOR DELAY @d",
            241,
        ),
        ("DECLARE @d BIGINT = 1; WAITFOR DELAY @d", 9815),
        ("DECLARE @d TIME = '00:00:01'; WAITFOR DELAY @d", 9815),
    ] {
        assert_eq!(run(&mut s, sql), Err(number), "{sql}");
    }
    run(
        &mut s,
        "BEGIN TRY DECLARE @d BIT = 1; WAITFOR DELAY @d END TRY
         BEGIN CATCH IF ERROR_NUMBER() <> 9815 OR ERROR_MESSAGE() <> 'Waitfor delay and waitfor time cannot be of type bit.' THROW 51000, 'expected 9815', 1 END CATCH",
    )
    .unwrap();
    // WAITFOR inside a transaction keeps it open.
    run(&mut s, "BEGIN TRAN; WAITFOR DELAY '00:00:00.010'").unwrap();
    assert_eq!(s.transactions, 1);
    run(&mut s, "COMMIT").unwrap();
}

#[test]
fn attention_cancels_a_wait() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.t(n INT)").unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let setter = flag.clone();
    let attention = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        setter.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let outcome = s.batch_response_with_read_cancel(
        "BEGIN TRAN; INSERT dbo.t VALUES (1); WAITFOR DELAY '00:01:00'; INSERT dbo.t VALUES (2)",
        &Default::default(),
        Mode::Batch,
        flag,
    );
    attention.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(outcome, Outcome::Cancelled { .. }), "{outcome:?}");
    // Like SQL Server with XACT_ABORT OFF, the transaction stays open and
    // the statement after WAITFOR did not run.
    assert_eq!(s.transactions, 1);
    assert_eq!(ints(&s, "SELECT n FROM dbo.t"), [1]);
    run(&mut s, "ROLLBACK").unwrap();
    // The session's own cancel handle ends a wait the same way.
    s.process().cancel();
    let started = Instant::now();
    let outcome = s.batch_response_with_read_cancel(
        "WAITFOR DELAY '00:01:00'",
        &Default::default(),
        Mode::Batch,
        Arc::new(AtomicBool::new(false)),
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(outcome, Outcome::Cancelled { .. }), "{outcome:?}");
    // A new request starts uncancelled.
    s.process().set_running(true);
    run(&mut s, "WAITFOR DELAY '00:00:00.010'").unwrap();
    s.process().set_running(false);
}

#[test]
fn terminating_a_session_ends_its_wait() {
    let server = Server::open(":memory:").unwrap();
    let mut admin = session(&server);
    run(&mut admin, "CREATE DATABASE wait_db").unwrap();
    let mut waiting = session(&server);
    waiting.process().set_login(
        msduck::sessions::Client::default(),
        "sa",
        Some(Arc::new(|| {})),
    );
    waiting.use_database("wait_db").unwrap();
    let waiter = thread::spawn(move || {
        let started = Instant::now();
        let result = run(&mut waiting, "WAITFOR DELAY '00:01:00'");
        (result.is_err(), started.elapsed())
    });
    thread::sleep(Duration::from_millis(300));
    run(
        &mut admin,
        "ALTER DATABASE wait_db SET SINGLE_USER WITH ROLLBACK IMMEDIATE",
    )
    .unwrap();
    let (failed, elapsed) = waiter.join().unwrap();
    assert!(failed);
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn every_write_form_is_restored() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE dbo.k(id INT PRIMARY KEY, v INT);
         CREATE TABLE dbo.audit(id INT, v INT);
         CREATE TABLE dbo.other(id INT);
         INSERT dbo.k VALUES (1, 1), (2, 2), (3, 3); INSERT dbo.other VALUES (1), (2)",
    )
    .unwrap();
    run(&mut s, "BEGIN TRAN; SAVE TRAN s").unwrap();
    for sql in [
        "UPDATE [dbo].[k] SET v = 10 WHERE id = 1",
        "UPDATE t SET v = 20 FROM dbo.k AS t JOIN dbo.other o ON o.id = t.id WHERE t.id = 2",
        "DELETE t FROM dbo.k t WHERE t.id = 3",
        "INSERT INTO k (id, v) VALUES (4, 4)",
        "UPDATE dbo.k SET v = v + 1 OUTPUT inserted.id, inserted.v INTO dbo.audit(id, v) WHERE id = 4",
        "TRUNCATE TABLE dbo.other",
    ] {
        run(&mut s, sql).unwrap_or_else(|number| panic!("{sql}: {number}"));
    }
    assert_eq!(ints(&s, "SELECT id FROM dbo.k ORDER BY id"), [1, 2, 4]);
    run(&mut s, "ROLLBACK TRAN s").unwrap();
    assert_eq!(
        pairs(&s, "SELECT id, CAST(v AS VARCHAR) FROM dbo.k ORDER BY id"),
        [
            (1, Some("1".into())),
            (2, Some("2".into())),
            (3, Some("3".into()))
        ]
    );
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.audit"), [0]);
    assert_eq!(ints(&s, "SELECT id FROM dbo.other ORDER BY id"), [1, 2]);
    run(&mut s, "COMMIT").unwrap();
}

#[test]
fn triggers_and_cascades_after_a_savepoint_are_restored() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE dbo.p(id INT PRIMARY KEY);
         CREATE TABLE dbo.c(id INT PRIMARY KEY, pid INT REFERENCES dbo.p(id) ON DELETE CASCADE);
         CREATE TABLE dbo.log(n INT)",
    )
    .unwrap();
    run(
        &mut s,
        "CREATE TRIGGER dbo.tr ON dbo.p AFTER INSERT AS INSERT dbo.log SELECT id FROM inserted",
    )
    .unwrap();
    run(
        &mut s,
        "INSERT dbo.p VALUES (1); INSERT dbo.c VALUES (10, 1)",
    )
    .unwrap();
    run(
        &mut s,
        "BEGIN TRAN; SAVE TRAN s; INSERT dbo.p VALUES (2); DELETE dbo.p WHERE id = 1",
    )
    .unwrap();
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.c"), [0]);
    assert_eq!(ints(&s, "SELECT count(*) FROM dbo.log"), [2]);
    run(&mut s, "ROLLBACK TRAN s; COMMIT").unwrap();
    assert_eq!(ints(&s, "SELECT id FROM dbo.p"), [1]);
    assert_eq!(ints(&s, "SELECT id FROM dbo.c"), [10]);
    assert_eq!(ints(&s, "SELECT n FROM dbo.log"), [1]);
}

#[test]
fn temporary_tables_roll_back_and_table_variables_keep_their_rows() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE #t(n INT); INSERT #t VALUES (1);
         DECLARE @v TABLE(n INT);
         BEGIN TRAN; SAVE TRAN s;
         INSERT #t VALUES (2); INSERT @v VALUES (1);
         ROLLBACK TRAN s;
         IF (SELECT count(*) FROM #t) <> 1 THROW 51000, 'temporary table not restored', 1;
         IF (SELECT count(*) FROM @v) <> 1 THROW 51001, 'table variable rolled back', 1;
         COMMIT",
    )
    .unwrap();
}
