//! Isolation levels, savepoints, named transactions, WAITFOR and DBCC
//! USEROPTIONS in process (issue #727). Expected values come from
//! reference/savepoint.json and reference/gaps-transactions.json; see
//! docs/gaps-transactions.md. Client-level coverage is in
//! tests/compat/transactions.test.mjs.
use msduck::{
    database_catalog::SnapshotIsolation,
    engine::Session,
    read_cancellation::{Mode, Outcome},
    server::Server,
    tds::{BeginTransaction, TransactionRequest},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
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

/// Bound a real-clock probe without sleeping through the deadline on success.
/// Dropping also wakes/joins the watchdog if a probe panics.
struct RequestWatchdog {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RequestWatchdog {
    fn new(flag: Arc<AtomicBool>, deadline: Duration) -> Self {
        let (stop, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            if matches!(
                receiver.recv_timeout(deadline),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                flag.store(true, Ordering::SeqCst);
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for RequestWatchdog {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            // The watchdog only receives a message or sets an atomic flag.
            // Do not cause a second panic if its caller is already unwinding.
            let _ = worker.join();
        }
    }
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
    // Allow query work a five-second margin. Missing a one-second target
    // otherwise waits until tomorrow, which is correct server behavior but an
    // unbounded test. Cancellation is a failure here, never a successful wait.
    let flag = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let watchdog = RequestWatchdog::new(flag.clone(), Duration::from_secs(10));
    let outcome = s.batch_response_with_read_cancel(
        "DECLARE @t INT = DATEDIFF(SECOND, CAST(CAST(GETDATE() AS DATE) AS DATETIME), GETDATE()) + 5; WAITFOR TIME @t",
        &Default::default(),
        Mode::Batch,
        flag,
    );
    drop(watchdog);
    assert!(
        matches!(outcome, Outcome::Finished { success: true, .. }),
        "WAITFOR TIME must finish before its watchdog: {outcome:?}"
    );
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_secs(10));
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
fn missed_waitfor_time_target_is_cancelled_before_the_next_day() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE TABLE dbo.waitfor_time_probe(n INT)").unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let watchdog = RequestWatchdog::new(flag.clone(), Duration::from_secs(4));
    let outcome = s.batch_response_with_read_cancel(
        "DECLARE @t INT = DATEDIFF(SECOND, CAST(CAST(GETDATE() AS DATE) AS DATETIME), GETDATE()) + 1;
         WAITFOR DELAY '00:00:02'; WAITFOR TIME @t;
         INSERT dbo.waitfor_time_probe VALUES (1);
         THROW 51000, 'missed target ran a later statement', 1",
        &Default::default(),
        Mode::Batch,
        flag,
    );
    drop(watchdog);
    let Outcome::Cancelled { tokens, .. } = outcome else {
        panic!("not cancelled: {outcome:?}");
    };
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < Duration::from_secs(8));
    // Check before SELECT resets last_error: the cancellation outcome alone
    // would not reveal a wrongly executed trailing statement.
    assert_eq!(s.last_error, 0);
    assert!(!contains_utf16(
        &tokens,
        "missed target ran a later statement"
    ));
    assert!(ints(&s, "SELECT n FROM dbo.waitfor_time_probe").is_empty());
    // The watchdog was specific to the completed request; the session remains usable.
    run(&mut s, "SELECT 1").unwrap();
    assert_eq!(ints(&s, "SELECT 1"), [1]);
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
        "MERGE dbo.k AS t USING (VALUES (5, 5), (1, 99)) AS src(id, v) ON t.id = src.id WHEN MATCHED THEN UPDATE SET v = src.v WHEN NOT MATCHED THEN INSERT (id, v) VALUES (src.id, src.v);",
        "TRUNCATE TABLE dbo.other",
    ] {
        run(&mut s, sql).unwrap_or_else(|number| panic!("{sql}: {number}"));
    }
    assert_eq!(ints(&s, "SELECT id FROM dbo.k ORDER BY id"), [1, 2, 4, 5]);
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

#[test]
fn columns_named_like_aliases_and_cte_writes_are_restored() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE dbo.keyed(id INT PRIMARY KEY, c INT, t INT);
         CREATE TABLE dbo.heap(c INT, t INT);
         INSERT dbo.keyed VALUES (1, 1, 1); INSERT dbo.heap VALUES (1, 1), (1, 2)",
    )
    .unwrap();
    run(
        &mut s,
        "BEGIN TRAN; SAVE TRAN s;
         UPDATE dbo.keyed SET t = 9;
         UPDATE dbo.heap SET t = 9 WHERE t = 2;
         WITH src AS (SELECT 2 AS id) INSERT dbo.keyed(id, c, t) SELECT id, 2, 2 FROM src;
         ROLLBACK TRAN s; COMMIT",
    )
    .unwrap();
    assert_eq!(
        pairs(
            &s,
            "SELECT t, CAST(c AS VARCHAR) FROM dbo.keyed ORDER BY id"
        ),
        [(1, Some("1".into()))]
    );
    assert_eq!(ints(&s, "SELECT t FROM dbo.heap ORDER BY t"), [1, 2]);
}

/// `snapshot_isolation_state` and its description for a database.
fn snapshot_state(session: &Session, name: &str) -> Option<(i64, Option<String>)> {
    session
        .db
        .query_row(
            "SELECT snapshot_isolation_state, snapshot_isolation_state_desc FROM sys.databases WHERE name = ?",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
}

/// Fail with 50001 unless `condition` holds, evaluated through T-SQL.
fn check(session: &mut Session, condition: &str) -> Result<Vec<u8>, i32> {
    run(
        session,
        &format!("IF NOT ({condition}) THROW 50001, 'check failed', 1"),
    )
}

#[test]
fn allow_snapshot_isolation_is_published_and_persists_across_restart() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-snapshot-isolation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut s = session(&server);
        run(&mut s, "CREATE DATABASE probe_db").unwrap();
        // master always allows snapshot isolation (3987 is informational).
        assert_eq!(snapshot_state(&s, "master"), Some((1, Some("ON".into()))));
        assert_eq!(
            snapshot_state(&s, "probe_db"),
            Some((0, Some("OFF".into())))
        );
        let tokens = run(
            &mut s,
            "ALTER DATABASE master SET ALLOW_SNAPSHOT_ISOLATION OFF",
        )
        .unwrap();
        assert!(contains_utf16(
            &tokens,
            "SNAPSHOT ISOLATION is always enabled in this database."
        ));
        assert_eq!(snapshot_state(&s, "master"), Some((1, Some("ON".into()))));
        run(
            &mut s,
            "ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON",
        )
        .unwrap();
        assert_eq!(snapshot_state(&s, "probe_db"), Some((1, Some("ON".into()))));
        // Independent of READ_COMMITTED_SNAPSHOT.
        run(
            &mut s,
            "ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE",
        )
        .unwrap();
        assert_eq!(snapshot_state(&s, "probe_db"), Some((1, Some("ON".into()))));
    }
    {
        // A change that a stop interrupted returns to the state it started
        // from.
        let server = Server::open(path).unwrap();
        let db = server.connection().unwrap();
        db.databases()
            .set_snapshot_isolation(&db, "probe_db", SnapshotIsolation::ToOff)
            .unwrap();
    }
    let server = Server::open(path).unwrap();
    let mut s = session(&server);
    assert_eq!(snapshot_state(&s, "probe_db"), Some((1, Some("ON".into()))));
    s.use_database("probe_db").unwrap();
    run(
        &mut s,
        "ALTER DATABASE CURRENT SET ALLOW_SNAPSHOT_ISOLATION OFF",
    )
    .unwrap();
    assert_eq!(
        snapshot_state(&s, "probe_db"),
        Some((0, Some("OFF".into())))
    );
    drop(s);
    drop(server);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn allow_snapshot_isolation_errors_follow_sql_server() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE DATABASE probe_db").unwrap();
    // Captured order: transaction, CURRENT in master, the database, other
    // options, then the termination clause.
    assert_eq!(
        run(
            &mut s,
            "ALTER DATABASE CURRENT SET ALLOW_SNAPSHOT_ISOLATION ON"
        ),
        Err(12104)
    );
    assert_eq!(
        run(
            &mut s,
            "BEGIN TRAN; ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON"
        ),
        Err(226)
    );
    run(&mut s, "ROLLBACK").unwrap();
    for (sql, number) in [
        (
            "ALTER DATABASE missing_db SET ALLOW_SNAPSHOT_ISOLATION ON WITH NO_WAIT",
            5069,
        ),
        (
            "ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON, MULTI_USER",
            5069,
        ),
        (
            "ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON WITH ROLLBACK IMMEDIATE",
            5069,
        ),
        (
            "ALTER DATABASE master SET ALLOW_SNAPSHOT_ISOLATION ON WITH NO_WAIT",
            5069,
        ),
    ] {
        let tokens = s.batch_response(sql, &Default::default(), false, None).0;
        assert_eq!(s.last_error, number, "{sql}");
        let first = match sql {
            s if s.contains("missing_db") => {
                "User does not have permission to alter database 'missing_db'"
            }
            s if s.contains("MULTI_USER") => {
                "Cannot change the versioning state on database \"probe_db\" together with another database state."
            }
            _ => "The termination option is not supported when making versioning state changes.",
        };
        assert!(contains_utf16(&tokens, first), "{sql}");
    }
    // Conflicting values fail before any statement of the batch runs.
    let tokens = s
        .batch_response(
            "CREATE TABLE probe_db.dbo.ran (v INT); ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON, ALLOW_SNAPSHOT_ISOLATION OFF",
            &Default::default(),
            false,
            None,
        )
        .0;
    assert_eq!(s.last_error, 5062);
    assert!(contains_utf16(
        &tokens,
        "The option \"ALLOW_SNAPSHOT_ISOLATION\" conflicts with another requested option."
    ));
    assert_eq!(
        ints(
            &s,
            "SELECT count(*) FROM duckdb_tables() WHERE table_name = 'ran'"
        ),
        [0]
    );
    // Repeating the same value is accepted.
    run(
        &mut s,
        "ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON, ALLOW_SNAPSHOT_ISOLATION ON",
    )
    .unwrap();
    assert_eq!(snapshot_state(&s, "probe_db"), Some((1, Some("ON".into()))));
}

#[test]
fn snapshot_transactions_need_the_option_and_read_their_snapshot() {
    let server = Server::open(":memory:").unwrap();
    let mut a = session(&server);
    run(&mut a, "CREATE DATABASE probe_db").unwrap();
    a.use_database("probe_db").unwrap();
    let mut b = session(&server);
    b.use_database("probe_db").unwrap();
    run(
        &mut a,
        "CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT t VALUES (1, 1); CREATE TABLE #tmp (v INT)",
    )
    .unwrap();
    run(&mut a, "SET TRANSACTION ISOLATION LEVEL SNAPSHOT").unwrap();
    // Statements without table access, temporary tables, table variables and
    // catalog views work while the option is OFF.
    run(
        &mut a,
        "SELECT 1; INSERT #tmp VALUES (1); SELECT v FROM #tmp; DECLARE @x TABLE (v INT); INSERT @x VALUES (1); SELECT COUNT(*) FROM sys.objects",
    )
    .unwrap();
    // A common table expression or a table alias shadows the table of the
    // same name.
    run(&mut a, "CREATE TABLE x (v INT)").unwrap();
    run(&mut a, "UPDATE x SET v = 2 FROM #tmp AS x").unwrap();
    run(&mut a, "DELETE x FROM #tmp AS x").unwrap();
    assert_eq!(run(&mut a, "UPDATE x SET v = 2 FROM t AS x"), Err(3952));
    assert_eq!(run(&mut a, "SELECT v FROM t AS t"), Err(3952));
    // msduck's internal name prefix does not exempt user tables.
    run(&mut b, "CREATE TABLE __msduck_customer (v INT)").unwrap();
    assert_eq!(run(&mut a, "SELECT v FROM __msduck_customer"), Err(3952));
    assert_eq!(run(&mut a, "UPDATE t SET v = 2 FROM t AS t"), Err(3952));
    run(&mut a, "WITH t AS (SELECT 1 AS v) SELECT v FROM t").unwrap();
    assert_eq!(
        run(
            &mut a,
            "WITH c AS (SELECT 1 AS v) SELECT t.v FROM t JOIN c ON 1 = 1"
        ),
        Err(3952)
    );
    let tokens = a
        .batch_response("SELECT v FROM t", &Default::default(), false, None)
        .0;
    assert_eq!(a.last_error, 3952);
    assert!(contains_utf16(
        &tokens,
        "Snapshot isolation transaction failed accessing database 'probe_db' because snapshot isolation is not allowed in this database. Use ALTER DATABASE to allow snapshot isolation."
    ));
    // 3952 ends the batch and rolls back the transaction.
    assert_eq!(
        run(&mut a, "BEGIN TRAN; UPDATE t SET v = 2; SELECT 1"),
        Err(3952)
    );
    check(&mut a, "@@TRANCOUNT = 0").unwrap();
    // Inside TRY it dooms the transaction instead.
    run(
        &mut a,
        "BEGIN TRAN; BEGIN TRY DELETE t END TRY BEGIN CATCH IF ERROR_NUMBER() <> 3952 OR XACT_STATE() <> -1 THROW 50001, 'not doomed', 1; ROLLBACK END CATCH",
    )
    .unwrap();
    // Another database's table, through a three-part name.
    run(&mut b, "CREATE TABLE u (v INT)").unwrap();
    let mut m = session(&server);
    run(&mut m, "SET TRANSACTION ISOLATION LEVEL SNAPSHOT").unwrap();
    assert_eq!(run(&mut m, "SELECT v FROM probe_db.dbo.u"), Err(3952));

    run(
        &mut b,
        "ALTER DATABASE CURRENT SET ALLOW_SNAPSHOT_ISOLATION ON",
    )
    .unwrap();
    // A SNAPSHOT transaction keeps reading the value it first saw after
    // another connection commits an update.
    run(&mut a, "BEGIN TRAN; SELECT v FROM t").unwrap();
    run(&mut b, "UPDATE t SET v = 2 WHERE id = 1").unwrap();
    check(&mut a, "(SELECT v FROM t WHERE id = 1) = 1").unwrap();
    check(&mut b, "(SELECT v FROM t WHERE id = 1) = 2").unwrap();
    // Updating that row is a write-write conflict: 3960 ends the batch and
    // rolls the transaction back.
    let tokens = a
        .batch_response(
            "UPDATE t SET v = 3 WHERE id = 1; SELECT 1",
            &Default::default(),
            false,
            None,
        )
        .0;
    assert_eq!(a.last_error, 3960);
    assert!(contains_utf16(
        &tokens,
        "Snapshot isolation transaction aborted due to update conflict. You cannot use snapshot isolation to access table 'dbo.t' directly or indirectly in database 'probe_db' to update, delete, or insert the row that has been modified or deleted by another transaction."
    ));
    check(
        &mut a,
        "@@TRANCOUNT = 0 AND (SELECT v FROM t WHERE id = 1) = 2",
    )
    .unwrap();
    // Caught, the conflict dooms the transaction; the batch end rolls it back.
    run(&mut a, "BEGIN TRAN; SELECT v FROM t").unwrap();
    run(&mut b, "UPDATE t SET v = 4 WHERE id = 1").unwrap();
    assert_eq!(
        run(
            &mut a,
            "BEGIN TRY UPDATE t SET v = 5 WHERE id = 1 END TRY BEGIN CATCH IF ERROR_NUMBER() <> 3960 OR XACT_STATE() <> -1 OR @@TRANCOUNT <> 1 THROW 50001, 'not doomed', 1 END CATCH"
        ),
        Err(3998)
    );
    check(
        &mut a,
        "@@TRANCOUNT = 0 AND (SELECT v FROM t WHERE id = 1) = 4",
    )
    .unwrap();
    // DML after a WITH clause reports the conflict too.
    run(&mut a, "BEGIN TRAN; SELECT v FROM t").unwrap();
    run(&mut b, "UPDATE t SET v = 6 WHERE id = 1").unwrap();
    assert_eq!(
        run(
            &mut a,
            "WITH s AS (SELECT 1 AS id) UPDATE t SET v = 7 WHERE id IN (SELECT id FROM s)"
        ),
        Err(3960)
    );
    check(
        &mut a,
        "@@TRANCOUNT = 0 AND (SELECT v FROM t WHERE id = 1) = 6",
    )
    .unwrap();
    // An aliased target names the table it aliases.
    run(&mut a, "BEGIN TRAN; SELECT v FROM t").unwrap();
    run(&mut b, "UPDATE t SET v = 4 WHERE id = 1").unwrap();
    let tokens = a
        .batch_response(
            "UPDATE x SET v = 7 FROM dbo.t AS x WHERE x.id = 1",
            &Default::default(),
            false,
            None,
        )
        .0;
    assert_eq!(a.last_error, 3960);
    assert!(contains_utf16(&tokens, "access table 'dbo.t' directly"));
    // Writes to other rows commit.
    run(&mut a, "BEGIN TRAN; SELECT v FROM t").unwrap();
    run(&mut b, "INSERT t VALUES (10, 10)").unwrap();
    run(&mut a, "UPDATE t SET v = 8 WHERE id = 1; COMMIT").unwrap();
    check(&mut b, "(SELECT SUM(v) FROM t) = 18").unwrap();
    // Writes of a trigger fired by a SNAPSHOT write keep their savepoint
    // images, so rolling back to the savepoint undoes them too.
    run(&mut b, "CREATE TABLE audit (v INT)").unwrap();
    run(
        &mut b,
        "CREATE TRIGGER t_audit ON t AFTER UPDATE AS INSERT audit SELECT v FROM inserted",
    )
    .unwrap();
    run(
        &mut a,
        "BEGIN TRAN; SAVE TRAN s; UPDATE t SET v = 20 WHERE id = 1; ROLLBACK TRAN s; COMMIT",
    )
    .unwrap();
    check(
        &mut b,
        "(SELECT COUNT(*) FROM audit) = 0 AND (SELECT v FROM t WHERE id = 1) = 8",
    )
    .unwrap();
}

#[test]
fn allow_snapshot_isolation_changes_wait_for_open_transactions() {
    let server = Server::open(":memory:").unwrap();
    let mut writer = session(&server);
    run(&mut writer, "CREATE DATABASE probe_db").unwrap();
    writer.use_database("probe_db").unwrap();
    run(
        &mut writer,
        "CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT t VALUES (1, 1)",
    )
    .unwrap();
    let mut reader = session(&server);
    reader.use_database("probe_db").unwrap();
    let mut observer = session(&server);
    let alter = |sql: &'static str| {
        let mut s = session(&server);
        s.process().set_login(
            msduck::sessions::Client::default(),
            "sa",
            Some(Arc::new(|| {})),
        );
        thread::spawn(move || {
            let started = Instant::now();
            let result = run(&mut s, sql).map(|_| ());
            (result, started.elapsed())
        })
    };
    // ON waits for a transaction that wrote, not for one that only read.
    run(&mut reader, "BEGIN TRAN; SELECT v FROM t").unwrap();
    run(&mut writer, "BEGIN TRAN; UPDATE t SET v = 2").unwrap();
    let on = alter("ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION ON");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        snapshot_state(&observer, "probe_db"),
        Some((3, Some("IN_TRANSITION_TO_ON".into())))
    );
    run(&mut observer, "SET TRANSACTION ISOLATION LEVEL SNAPSHOT").unwrap();
    assert_eq!(
        run(&mut observer, "BEGIN TRAN; SELECT v FROM probe_db.dbo.t"),
        Err(3956)
    );
    check(&mut observer, "@@TRANCOUNT = 0").unwrap();
    run(&mut writer, "COMMIT").unwrap();
    let (result, elapsed) = on.join().unwrap();
    assert_eq!(result, Ok(()));
    assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
    assert_eq!(
        snapshot_state(&observer, "probe_db"),
        Some((1, Some("ON".into())))
    );
    run(&mut reader, "COMMIT").unwrap();
    // OFF waits for SNAPSHOT transactions too. One that already read keeps
    // reading; one that had not yet read fails with 3954.
    run(
        &mut reader,
        "SET TRANSACTION ISOLATION LEVEL SNAPSHOT; BEGIN TRAN; SELECT v FROM t",
    )
    .unwrap();
    let mut idle = session(&server);
    idle.use_database("probe_db").unwrap();
    run(
        &mut idle,
        "SET TRANSACTION ISOLATION LEVEL SNAPSHOT; BEGIN TRAN; SELECT 1",
    )
    .unwrap();
    let off = alter("ALTER DATABASE probe_db SET ALLOW_SNAPSHOT_ISOLATION OFF");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        snapshot_state(&observer, "probe_db"),
        Some((2, Some("IN_TRANSITION_TO_OFF".into())))
    );
    check(&mut reader, "(SELECT v FROM t) = 2").unwrap();
    assert_eq!(run(&mut idle, "SELECT v FROM t"), Err(3954));
    check(&mut idle, "@@TRANCOUNT = 0").unwrap();
    run(&mut reader, "COMMIT").unwrap();
    let (result, elapsed) = off.join().unwrap();
    assert_eq!(result, Ok(()));
    assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
    assert_eq!(
        snapshot_state(&observer, "probe_db"),
        Some((0, Some("OFF".into())))
    );
}
