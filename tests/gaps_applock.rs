//! Application locks (issue #713): sp_getapplock, sp_releaseapplock,
//! APPLOCK_MODE and APPLOCK_TEST against SQL Server's captured behavior in
//! reference/gaps-applock.json.
use msduck::engine::Session;
use msduck::server::Server;
use std::{
    thread,
    time::{Duration, Instant},
};

/// What a batch reported: RETURNSTATUS values, diagnostics (number, state,
/// class, message) and DONEINPROC tokens (status, command, count).
#[derive(Debug, Default)]
struct Outcome {
    ok: bool,
    statuses: Vec<i32>,
    diagnostics: Vec<(i32, u8, u8, String)>,
    in_proc: Vec<(u16, u16, u64)>,
}

impl Outcome {
    fn numbers(&self) -> Vec<i32> {
        self.diagnostics.iter().map(|d| d.0).collect()
    }
    /// PRINT output, in order.
    fn printed(&self) -> Vec<&str> {
        self.diagnostics
            .iter()
            .filter(|d| d.0 == 0)
            .map(|d| d.3.as_str())
            .collect()
    }
}

fn utf16(bytes: &[u8]) -> String {
    String::from_utf16_lossy(
        &bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect::<Vec<_>>(),
    )
}

/// Run a batch that returns no result sets.
fn run(session: &mut Session, sql: &str) -> Outcome {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    let mut outcome = Outcome {
        ok,
        ..Default::default()
    };
    let mut at = 0;
    while at < tokens.len() {
        let kind = tokens[at];
        at += 1;
        match kind {
            0xfd..=0xff => {
                let status = u16::from_le_bytes([tokens[at], tokens[at + 1]]);
                let command = u16::from_le_bytes([tokens[at + 2], tokens[at + 3]]);
                let count = u64::from_le_bytes(tokens[at + 4..at + 12].try_into().unwrap());
                if kind == 0xff {
                    outcome.in_proc.push((status, command, count));
                }
                at += 12;
            }
            0x79 => {
                outcome
                    .statuses
                    .push(i32::from_le_bytes(tokens[at..at + 4].try_into().unwrap()));
                at += 4;
            }
            0xaa | 0xab | 0xe3 => {
                let length = u16::from_le_bytes([tokens[at], tokens[at + 1]]) as usize;
                let body = &tokens[at + 2..at + 2 + length];
                if kind != 0xe3 {
                    let number = i32::from_le_bytes(body[0..4].try_into().unwrap());
                    let units = u16::from_le_bytes([body[6], body[7]]) as usize;
                    let message = utf16(&body[8..8 + units * 2]);
                    outcome
                        .diagnostics
                        .push((number, body[4], body[5], message));
                }
                at += 2 + length;
            }
            other => panic!("unexpected token 0x{other:02x} in {sql}"),
        }
    }
    outcome
}

/// The lock table is process-wide, like SQL Server's lock manager, and keyed
/// by database name; each test uses its own database so tests running in
/// parallel (each with its own in-memory server) cannot contend.
struct Fixture {
    server: Server,
    database: String,
}

fn server(database: &str) -> Fixture {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert!(run(&mut session, &format!("CREATE DATABASE {database}")).ok);
    Fixture {
        server,
        database: database.into(),
    }
}

fn session(fixture: &Fixture) -> Session {
    let mut session = Session::new(fixture.server.connection().unwrap()).unwrap();
    session.use_database(&fixture.database).unwrap();
    session
}

fn get(resource: &str, mode: &str, owner: &str, timeout: i32) -> String {
    format!(
        "DECLARE @rc int; EXEC @rc = sp_getapplock @Resource = N'{resource}', @LockMode = '{mode}', @LockOwner = '{owner}', @LockTimeout = {timeout}; PRINT CAST(@rc AS varchar(10))"
    )
}

fn release(resource: &str, owner: &str) -> String {
    format!(
        "DECLARE @rc int; EXEC @rc = sp_releaseapplock @Resource = N'{resource}', @LockOwner = '{owner}'; PRINT CAST(@rc AS varchar(10))"
    )
}

fn mode(session: &mut Session, resource: &str, owner: &str) -> String {
    let outcome = run(
        session,
        &format!("PRINT APPLOCK_MODE('public', N'{resource}', '{owner}')"),
    );
    outcome.printed()[0].to_string()
}

fn status(session: &mut Session, sql: &str) -> i32 {
    let outcome = run(session, sql);
    outcome.printed().last().unwrap().parse().unwrap()
}

/// Expected diagnostics: number, state and class.
type Diagnostics = [(i32, u8, u8)];

/// The DONEINPROC stream of a successful sp_getapplock with an explicit
/// timeout (reference "repro").
const GET_BODY: [(u16, u16, u64); 9] = [
    (0x11, 193, 1),
    (0x01, 192, 0),
    (0x11, 193, 1),
    (0x01, 192, 0),
    (0x01, 192, 0),
    (0x11, 193, 1),
    (0x01, 192, 0),
    (0x01, 224, 0),
    (0x11, 193, 1),
];

#[test]
fn repro_and_argument_forms() {
    let server = server("applock_repro");
    let mut a = session(&server);
    let outcome = run(
        &mut a,
        "EXEC sp_getapplock @Resource=N'foo', @LockMode='Exclusive', @LockOwner='Session', @LockTimeout=0;",
    );
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.statuses, [0]);
    assert_eq!(outcome.in_proc, GET_BODY);
    assert!(outcome.diagnostics.is_empty());
    // Positional, EXEC @rc =, lowercase names, qualified names and DEFAULT.
    for sql in [
        "DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Exclusive', 'Session', 0; PRINT CAST(@rc AS varchar(10))",
        "DECLARE @rc int; EXECUTE @rc = dbo.sp_getapplock @resource = N'foo', @lockmode = 'exclusive ', @lockowner = 'SESSION'; PRINT CAST(@rc AS varchar(10))",
        "DECLARE @rc int; EXEC @rc = sys.sp_getapplock N'foo', 'Exclusive', 'Session', DEFAULT, DEFAULT; PRINT CAST(@rc AS varchar(10))",
        "DECLARE @r nvarchar(300) = N'foo', @t int = 0, @rc int; EXEC @rc = sp_getapplock @r, 'Exclusive', 'Session', @t; PRINT CAST(@rc AS varchar(10))",
    ] {
        assert_eq!(status(&mut a, sql), 0, "{sql}");
    }
    assert_eq!(mode(&mut a, "foo", "Session"), "Exclusive");
    // Five references: four releases keep the lock, the fifth frees it.
    for _ in 0..4 {
        assert_eq!(status(&mut a, &release("foo", "Session")), 0);
        assert_eq!(mode(&mut a, "foo", "Session"), "Exclusive");
    }
    assert_eq!(status(&mut a, &release("foo", "Session")), 0);
    assert_eq!(mode(&mut a, "foo", "Session"), "NoLock");
    let outcome = run(&mut a, &release("foo", "Session"));
    assert_eq!(outcome.numbers(), [1223, 0]);
    assert_eq!(
        outcome.diagnostics[0].3,
        "Cannot release the application lock (Database Principal: 'public', Resource: 'foo') because it is not currently held."
    );
    assert_eq!(outcome.printed(), ["-999"]);
    // The return status converts to the variable's type.
    let outcome = run(
        &mut a,
        "DECLARE @rc varchar(10); EXEC @rc = sp_getapplock N'v', 'Shared', 'Session', 0; PRINT @rc; EXEC sp_releaseapplock N'v', 'Session'",
    );
    assert_eq!(outcome.printed(), ["0"]);
}

#[test]
fn validation_messages_and_statuses() {
    let server = server("applock_validation");
    let mut a = session(&server);
    let cases: [(&str, &Diagnostics); 13] = [
        (
            "EXEC sp_getapplock N'x', 'Bogus', 'Session'",
            &[(15625, 1, 0)],
        ),
        ("EXEC sp_getapplock N'x', NULL, 'Session'", &[(15625, 1, 0)]),
        (
            "EXEC sp_getapplock N'x', 'Shared', 'Bogus'",
            &[(15625, 1, 0)],
        ),
        ("EXEC sp_getapplock N'x', 'Shared'", &[(15626, 1, 0)]),
        (
            "EXEC sp_getapplock NULL, 'Shared', 'Session'",
            &[(1224, 5, 16)],
        ),
        (
            "EXEC sp_getapplock N'x', 'Shared', 'Session', -2, 'nobody'",
            &[(1227, 2, 16)],
        ),
        (
            "EXEC sp_getapplock N'x', 'Shared', 'Session', 0, 'nobody'",
            &[(1202, 1, 16)],
        ),
        (
            "EXEC sp_getapplock N'x', 'Shared', 'Session', 0, NULL",
            &[(1230, 1, 16)],
        ),
        ("EXEC sp_releaseapplock N'x', 'Bogus'", &[(15625, 1, 0)]),
        ("EXEC sp_releaseapplock N'x'", &[(3918, 1, 16)]),
        ("EXEC sp_releaseapplock NULL, 'Session'", &[(1224, 5, 16)]),
        (
            "EXEC sp_releaseapplock N'x', 'Session', 'nobody'",
            &[(1202, 1, 16)],
        ),
        ("EXEC sp_releaseapplock N'x', 'Session'", &[(1223, 1, 16)]),
    ];
    for (sql, expected) in cases {
        let outcome = run(&mut a, sql);
        let diagnostics: Vec<_> = outcome
            .diagnostics
            .iter()
            .map(|d| (d.0, d.1, d.2))
            .collect();
        assert_eq!(diagnostics, expected, "{sql}");
        assert_eq!(outcome.statuses, [-999], "{sql}");
    }
    let outcome = run(&mut a, "EXEC sp_getapplock N'x', 'Bogus', 'Session'");
    assert_eq!(
        outcome.diagnostics[0].3,
        "Option 'Bogus' not recognized for '@LockMode' parameter."
    );
    // The procedure body returns, so @@ERROR is 0 and @@ROWCOUNT is 1.
    let outcome = run(
        &mut a,
        "EXEC sp_releaseapplock N'x', 'Session'; PRINT CAST(@@ERROR AS varchar(10)) + ',' + CAST(@@ROWCOUNT AS varchar(10))",
    );
    assert_eq!(outcome.printed(), ["0,1"]);
    // Call errors.
    for (sql, number) in [
        (
            "EXEC sp_getapplock @Resource = N'x', @LockMode = 'Shared', @Bogus = 1",
            8145,
        ),
        ("EXEC sp_getapplock @Resource = N'x', 'Shared'", 119),
        (
            "EXEC sp_getapplock N'x', 'Shared', 'Session', 0, 'public', 7",
            8144,
        ),
        ("EXEC sp_getapplock N'x'", 201),
        ("EXEC sp_getapplock N'x', 'Shared', 'Session', 'abc'", 8114),
        (
            "EXEC @undeclared = sp_getapplock N'x', 'Shared', 'Session'",
            137,
        ),
    ] {
        assert_eq!(run(&mut a, sql).numbers(), [number], "{sql}");
    }
    // With NOCOUNT ON only the failed xp_userlock call reports completion.
    let outcome = run(
        &mut a,
        "SET NOCOUNT ON; EXEC sp_getapplock N'n', 'Shared', 'Session', 0; EXEC sp_releaseapplock N'n', 'Session'; EXEC sp_releaseapplock N'n', 'Session'; SET NOCOUNT OFF",
    );
    assert_eq!(outcome.statuses, [0, 0, -999]);
    assert_eq!(outcome.in_proc, [(0x03, 224, 0)]);
}

#[test]
fn contention_timeouts_and_waits() {
    let server = server("applock_contention");
    let mut a = session(&server);
    let mut b = session(&server);
    assert_eq!(status(&mut a, &get("foo", "Exclusive", "Session", 0)), 0);
    assert_eq!(status(&mut b, &get("foo", "Exclusive", "Session", 0)), -1);
    let started = Instant::now();
    assert_eq!(status(&mut b, &get("foo", "Shared", "Session", 300)), -1);
    assert!(started.elapsed() >= Duration::from_millis(300));
    // Locks are per principal (case-insensitive), resource (exact) and database.
    for sql in [
        format!(
            "{}; EXEC sp_releaseapplock N'foo', 'Session', 'DBO'",
            get("foo", "Exclusive", "Session', @DbPrincipal = 'dbo", 0)
        ),
        format!(
            "{}; EXEC sp_releaseapplock N'FOO', 'Session'",
            get("FOO", "Exclusive", "Session", 0)
        ),
        format!(
            "{}; EXEC sp_releaseapplock N'foo ', 'Session'",
            get("foo ", "Exclusive", "Session", 0)
        ),
        format!(
            "USE master; {}; EXEC sp_releaseapplock N'foo', 'Session'; USE applock_contention",
            get("foo", "Exclusive", "Session", 0)
        ),
    ] {
        assert_eq!(run(&mut b, &sql).printed()[0], "0", "{sql}");
    }
    // A database-qualified call runs in that database's context.
    let qualified = run(
        &mut b,
        "DECLARE @rc int; EXEC @rc = master..sp_getapplock N'foo', 'Exclusive', 'Session', 0; PRINT CAST(@rc AS varchar(10)); USE master; PRINT APPLOCK_MODE('public', 'foo', 'Session'); EXEC @rc = sp_releaseapplock N'foo', 'Session'; USE applock_contention",
    );
    assert_eq!(qualified.printed(), ["0", "Exclusive"]);
    assert_eq!(
        run(
            &mut b,
            "EXEC missing..sp_getapplock N'foo', 'Exclusive', 'Session', 0"
        )
        .numbers(),
        [911]
    );
    // A decimal timeout converts to int like any int parameter.
    assert_eq!(
        status(
            &mut b,
            "DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Exclusive', 'Session', 1.9; PRINT CAST(@rc AS varchar(10))"
        ),
        -1
    );
    let outcome = run(
        &mut b,
        "PRINT CAST(APPLOCK_TEST('public', 'foo', 'IntentShared', 'Session') AS varchar(1)) + APPLOCK_MODE('public', 'foo', 'Session')",
    );
    assert_eq!(outcome.printed(), ["0NoLock"]);
    assert_eq!(run(&mut b, &release("foo", "Session")).numbers(), [1223, 0]);
    // A waiting request is granted (status 1) once the holder releases.
    let waiter = thread::spawn(move || {
        let status = status(&mut b, &get("foo", "Exclusive", "Session", 10_000));
        (b, status)
    });
    thread::sleep(Duration::from_millis(200));
    assert_eq!(status(&mut a, &release("foo", "Session")), 0);
    let (mut b, granted) = waiter.join().unwrap();
    assert_eq!(granted, 1);
    assert_eq!(mode(&mut b, "foo", "Session"), "Exclusive");
    // Ending the session releases its session locks.
    let waiter = thread::spawn(move || status(&mut a, &get("foo", "Exclusive", "Session", -1)));
    thread::sleep(Duration::from_millis(200));
    drop(b);
    assert_eq!(waiter.join().unwrap(), 1);
}

#[test]
fn compatibility_and_mode_unions() {
    let server = server("applock_matrix");
    let mut a = session(&server);
    let mut b = session(&server);
    let modes = [
        "IntentShared",
        "Shared",
        "Update",
        "IntentExclusive",
        "Exclusive",
    ];
    // reference/gaps-applock.json "compatibility": held mode, then IS, S, U, IX, X.
    let matrix: [(&[&str], &str, [i32; 5]); 7] = [
        (&["IntentShared"], "IntentShared", [0, 0, 0, 0, -1]),
        (&["Shared"], "Shared", [0, 0, 0, -1, -1]),
        (&["Update"], "Update", [0, 0, -1, -1, -1]),
        (&["IntentExclusive"], "IntentExclusive", [0, -1, -1, 0, -1]),
        (
            &["Shared", "IntentExclusive"],
            "SharedIntentExclusive",
            [0, -1, -1, -1, -1],
        ),
        (
            &["Update", "IntentExclusive"],
            "UpdateIntentExclusive",
            [0, -1, -1, -1, -1],
        ),
        (&["Exclusive"], "Exclusive", [-1; 5]),
    ];
    for (sequence, held, expected) in matrix {
        for step in sequence {
            assert_eq!(status(&mut a, &get("m", step, "Session", 0)), 0);
        }
        assert_eq!(mode(&mut a, "m", "Session"), held);
        for (request, expected) in modes.iter().zip(expected) {
            let test = run(
                &mut b,
                &format!(
                    "PRINT CAST(APPLOCK_TEST('public', 'm', '{request}', 'Session') AS varchar(1))"
                ),
            );
            assert_eq!(test.printed()[0], if expected == 0 { "1" } else { "0" });
            assert_eq!(
                status(&mut b, &get("m", request, "Session", 0)),
                expected,
                "{held} vs {request}"
            );
            if expected == 0 {
                assert_eq!(status(&mut b, &release("m", "Session")), 0);
            }
        }
        for _ in sequence {
            assert_eq!(status(&mut a, &release("m", "Session")), 0);
        }
    }
}

#[test]
fn transaction_locks_end_with_the_outermost_transaction() {
    let server = server("applock_transaction");
    let mut a = session(&server);
    let mut b = session(&server);
    let outcome = run(&mut a, &get("t", "Exclusive", "Transaction", 0));
    assert_eq!(outcome.numbers(), [15626, 0]);
    assert_eq!(outcome.printed(), ["-999"]);
    assert!(
        run(
            &mut a,
            &format!("BEGIN TRAN; {}", get("t", "Exclusive", "Transaction", 0))
        )
        .ok
    );
    assert_eq!(mode(&mut a, "t", "Transaction"), "Exclusive");
    assert_eq!(mode(&mut a, "t", "Session"), "NoLock");
    // The session owner of the same session is never blocked by it.
    assert_eq!(status(&mut a, &get("t", "Exclusive", "Session", 0)), 0);
    assert_eq!(status(&mut b, &get("t", "Shared", "Session", 0)), -1);
    run(&mut a, "BEGIN TRAN; COMMIT");
    assert_eq!(mode(&mut a, "t", "Transaction"), "Exclusive");
    assert!(run(&mut a, "COMMIT").ok);
    // The session lock survives the transaction.
    assert_eq!(status(&mut b, &get("t", "Shared", "Session", 0)), -1);
    assert_eq!(status(&mut a, &release("t", "Session")), 0);
    assert_eq!(status(&mut b, &get("t", "Shared", "Session", 0)), 0);
    assert_eq!(status(&mut b, &release("t", "Session")), 0);
    // ROLLBACK releases transaction locks and grants waiters.
    assert!(
        run(
            &mut a,
            &format!("BEGIN TRAN; {}", get("r", "Exclusive", "Transaction", 0))
        )
        .ok
    );
    let waiter = thread::spawn(move || {
        let status = status(&mut b, &get("r", "Shared", "Session", 10_000));
        (b, status)
    });
    thread::sleep(Duration::from_millis(200));
    assert!(run(&mut a, "ROLLBACK").ok);
    let (_, granted) = waiter.join().unwrap();
    assert_eq!(granted, 1);
    // Functions with the Transaction owner need a transaction.
    let outcome = run(&mut a, "PRINT APPLOCK_MODE('public', 'r', 'Transaction')");
    assert_eq!(outcome.numbers(), [3918]);
    let outcome = run(&mut a, "PRINT APPLOCK_MODE('public', 'r', NULL)");
    assert_eq!(outcome.numbers(), [3918]);
}

#[test]
fn deadlocks_return_minus_three_without_rolling_back() {
    let server = server("applock_deadlock");
    let mut a = session(&server);
    let mut b = session(&server);
    assert!(
        run(
            &mut a,
            &format!("BEGIN TRAN; {}", get("d1", "Exclusive", "Transaction", 0))
        )
        .ok
    );
    assert!(
        run(
            &mut b,
            &format!("BEGIN TRAN; {}", get("d2", "Exclusive", "Transaction", 0))
        )
        .ok
    );
    let first = thread::spawn(move || {
        let outcome = run(
            &mut a,
            &format!(
                "{}; PRINT CAST(@@TRANCOUNT AS varchar(10))",
                get("d2", "Exclusive", "Transaction", -1)
            ),
        );
        (
            a,
            outcome
                .printed()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
    });
    thread::sleep(Duration::from_millis(200));
    let second = thread::spawn(move || {
        let status = status(&mut b, &get("d1", "Exclusive", "Transaction", 10_000));
        (b, status)
    });
    let (mut a, printed) = first.join().unwrap();
    // The longest-waiting request is the victim; its transaction stays open.
    assert_eq!(printed, ["-3", "1"]);
    assert!(run(&mut a, "ROLLBACK").ok);
    let (mut b, status) = second.join().unwrap();
    assert_eq!(status, 1);
    assert!(run(&mut b, "ROLLBACK").ok);
}

#[test]
fn functions_report_modes_and_errors() {
    let server = server("applock_functions");
    let mut a = session(&server);
    assert_eq!(status(&mut a, &get("f", "Shared", "Session", 0)), 0);
    assert_eq!(
        status(&mut a, &get("f", "IntentExclusive", "Session", 0)),
        0
    );
    let outcome = run(
        &mut a,
        "DECLARE @r nvarchar(10) = N'f'; PRINT APPLOCK_MODE(N'PUBLIC', @r, 'session ') + ',' + CAST(APPLOCK_TEST('public', @r, 'Exclusive', 'Session') AS varchar(1))",
    );
    assert_eq!(outcome.printed(), ["SharedIntentExclusive,1"]);
    for (sql, number, state) in [
        ("PRINT APPLOCK_MODE('public', 'f', 'Bogus')", 1226, 1),
        (
            "PRINT APPLOCK_TEST('public', 'f', 'Bogus', 'Session')",
            1225,
            3,
        ),
        (
            "PRINT APPLOCK_TEST('public', 'f', 'SharedIntentExclusive', 'Session')",
            1225,
            3,
        ),
        (
            "DECLARE @v nvarchar(10); PRINT APPLOCK_TEST('public', 'f', @v, 'Session')",
            1225,
            2,
        ),
        (
            "DECLARE @v nvarchar(10); PRINT APPLOCK_MODE('public', @v, 'Session')",
            1225,
            1,
        ),
        (
            "DECLARE @v nvarchar(10); PRINT APPLOCK_MODE(@v, 'f', 'Session')",
            1230,
            3,
        ),
        ("PRINT APPLOCK_MODE('nobody', 'f', 'Session')", 1202, 1),
        ("PRINT APPLOCK_MODE(NULL, 'f', 'Session')", 8116, 1),
        ("PRINT APPLOCK_MODE('public', 1, 'Session')", 8116, 1),
    ] {
        let outcome = run(&mut a, sql);
        let diagnostics: Vec<_> = outcome.diagnostics.iter().map(|d| (d.0, d.1)).collect();
        assert_eq!(diagnostics, [(number, state)], "{sql}");
    }
    // Resource names are truncated to 255 characters.
    let outcome = run(
        &mut a,
        "DECLARE @long nvarchar(300) = REPLICATE(N'a', 255) + N'b', @rc int; EXEC @rc = sp_getapplock @long, 'Exclusive', 'Session', 0; PRINT APPLOCK_MODE('public', REPLICATE(N'a', 255), 'Session'); EXEC @rc = sp_releaseapplock @long, 'Session'; PRINT CAST(@rc AS varchar(10))",
    );
    assert_eq!(outcome.printed(), ["Exclusive", "0"]);
}
