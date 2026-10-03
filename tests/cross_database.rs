//! Three-part names that reach another database from the current one
//! (issue #871, docs/databases.md). Rows, descriptors and errors are compared
//! with SQL Server through tedious in tests/compat/cross_database.test.mjs;
//! these tests check the engine state around such statements: the DuckDB
//! default catalog, features that run in the other database, transactions
//! and preparation.
use msduck::engine::Session;
use msduck::server::Server;
use msduck_core::types::Type;

fn session(server: &Server) -> Session {
    Session::new(server.connection().unwrap()).unwrap()
}

/// Run a batch; return its tokens and success.
fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) {
    let (out, ok) = run(session, sql);
    assert!(ok, "{sql}: {:?}", errors(&out));
}

/// ERROR tokens (number, state, class, message), decoded only where their
/// fields fill the token exactly.
fn errors(out: &[u8]) -> Vec<(i32, u8, u8, String)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 3 < out.len() {
        if out[i] == 0xAA {
            let length = u16::from_le_bytes([out[i + 1], out[i + 2]]) as usize;
            if let Some(body) = out.get(i + 3..i + 3 + length)
                && body.len() >= 8
            {
                let number = i32::from_le_bytes(body[0..4].try_into().unwrap());
                let characters = u16::from_le_bytes([body[6], body[7]]) as usize;
                let mut at = 8 + characters * 2;
                let mut valid = at <= body.len();
                for _ in 0..2 {
                    if valid && at < body.len() {
                        at += 1 + body[at] as usize * 2;
                    } else {
                        valid = false;
                    }
                }
                if valid && at + 4 == body.len() {
                    let units: Vec<u16> = body[8..8 + characters * 2]
                        .chunks(2)
                        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                        .collect();
                    found.push((number, body[4], body[5], String::from_utf16_lossy(&units)));
                    i += 3 + length;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

/// The single error of a failed batch.
fn fails(session: &mut Session, sql: &str) -> (i32, u8, u8, String) {
    let (out, ok) = run(session, sql);
    assert!(!ok, "{sql} should fail");
    let mut found = errors(&out);
    assert_eq!(found.len(), 1, "{sql}: {found:?}");
    found.remove(0)
}

/// A value read directly from DuckDB, bypassing the engine.
fn native(session: &Session, sql: &str) -> String {
    session
        .db
        .query_row(sql, [], |row| row.get::<_, String>(0))
        .unwrap()
}

/// The connection's DuckDB default catalog, which must be the session's
/// database again after every statement.
fn catalog(session: &Session) -> String {
    native(session, "SELECT current_database()||'.'||current_schema()")
}

/// A server with database `foo` (a table with a trigger that logs into
/// foo) and a table in master, with a session in master.
fn fixture() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let mut session = session(&server);
    ok(&mut session, "CREATE DATABASE foo");
    ok(
        &mut session,
        "USE foo; CREATE TABLE dbo.items (id INT IDENTITY PRIMARY KEY, name NVARCHAR(50) NOT NULL, v VARCHAR(10)); CREATE TABLE dbo.log (msg NVARCHAR(100))",
    );
    ok(
        &mut session,
        "CREATE TRIGGER items_log ON dbo.items AFTER INSERT AS INSERT dbo.log SELECT N'logged ' + name FROM inserted",
    );
    ok(
        &mut session,
        "INSERT dbo.items (name, v) VALUES (N'hello', 'x'); DELETE dbo.log; USE master",
    );
    ok(
        &mut session,
        "CREATE TABLE dbo.loc (id INT, label NVARCHAR(20)); INSERT dbo.loc VALUES (1, N'one')",
    );
    (server, session)
}

fn count(session: &Session, sql: &str) -> i64 {
    session.db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn statements_in_another_database_run_there_and_restore_the_catalog() {
    let (_server, mut session) = fixture();
    let home = catalog(&session);
    assert_eq!(home, "memory.dbo");
    ok(&mut session, "SELECT id, name FROM foo.dbo.items");
    assert_eq!(catalog(&session), home);
    // The trigger belongs to foo and fires there.
    ok(
        &mut session,
        "INSERT foo.dbo.items (name, v) VALUES (N'two', 'y')",
    );
    assert_eq!(catalog(&session), home);
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.log"), 1);
    ok(
        &mut session,
        "IF NOT EXISTS (SELECT 1 FROM foo.dbo.log WHERE msg = N'logged two') THROW 50001, 'not logged', 1",
    );
    // Writes from the current database through a three-part target run
    // in that database too, reading master's tables by their catalog.
    ok(
        &mut session,
        "INSERT foo.dbo.items (name) SELECT label FROM dbo.loc; UPDATE i SET v = 'z' FROM foo.dbo.items i JOIN loc l ON l.id = i.id",
    );
    assert_eq!(catalog(&session), home);
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 3);
    assert_eq!(
        native(&session, "SELECT v FROM foo.dbo.items WHERE id = 1"),
        "z"
    );
    // Reading another database while writing the current one stays here.
    ok(
        &mut session,
        "INSERT dbo.loc SELECT id, name FROM foo.dbo.items WHERE id = 2",
    );
    assert_eq!(count(&session, "SELECT count(*) FROM memory.dbo.loc"), 2);
    assert_eq!(catalog(&session), home);
    // DB_NAME() keeps naming the session's database outside the trigger.
    ok(
        &mut session,
        "IF DB_NAME() <> N'master' OR NOT EXISTS (SELECT 1 FROM foo.dbo.items WHERE name = N'two') THROW 50001, 'wrong database', 1",
    );
    assert_eq!(catalog(&session), home);
}

#[test]
fn another_database_does_not_take_ddl_or_temporary_objects() {
    let (_server, mut session) = fixture();
    // DuckDB's name for master's catalog is not a database name.
    assert_eq!(
        fails(&mut session, "SELECT * FROM \"memory\".dbo.loc"),
        (208, 1, 16, "Invalid object name 'memory.dbo.loc'.".into())
    );
    let (number, _, _, message) = fails(&mut session, "CREATE TABLE foo.dbo.made (id INT)");
    assert_eq!(number, 40515);
    assert_eq!(
        message,
        "unsupported reference to foo.dbo.made in another database; USE foo first"
    );
    let (number, _, _, message) = fails(&mut session, "DROP TABLE IF EXISTS foo.dbo.items");
    assert_eq!(number, 40515);
    assert_eq!(
        message,
        "unsupported reference to foo.dbo.items in another database; USE foo first"
    );
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 1);
    let (number, _, _, message) = fails(
        &mut session,
        "SELECT label INTO #t FROM dbo.loc; INSERT foo.dbo.items (name) SELECT label FROM #t",
    );
    assert_eq!(number, 40515);
    assert_eq!(
        message,
        "unsupported cross-database statement: it writes dbo.items in database 'foo' and also references temporary tables or table variables"
    );
    assert_eq!(catalog(&session), "memory.dbo");
}

#[test]
fn a_transaction_writes_one_database() {
    let (_server, mut session) = fixture();
    for (sql, written, modified) in [
        (
            "BEGIN TRAN; INSERT dbo.loc VALUES (5, N'five'); INSERT foo.dbo.items (name) VALUES (N'x')",
            "foo",
            "master",
        ),
        (
            "BEGIN TRAN; INSERT foo.dbo.items (name) VALUES (N'x'); INSERT dbo.loc VALUES (5, N'five')",
            "master",
            "foo",
        ),
    ] {
        let (number, state, class, message) = fails(&mut session, sql);
        assert_eq!((number, state, class), (40515, 1, 16), "{sql}");
        assert_eq!(
            message,
            format!(
                "unsupported cross-database transaction: database '{written}' cannot be modified in a transaction that has already modified database '{modified}'; a transaction may write only one database"
            )
        );
        // The doomed transaction ended with the batch; nothing committed.
        assert_eq!(session.transactions, 0, "{sql}");
        assert_eq!(catalog(&session), "memory.dbo", "{sql}");
        assert_eq!(count(&session, "SELECT count(*) FROM memory.dbo.loc"), 1);
        assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 1);
    }
    // Inside TRY the transaction is doomed instead, as for other
    // uncommittable errors.
    ok(
        &mut session,
        "BEGIN TRAN; BEGIN TRY INSERT dbo.loc VALUES (5, N'five'); INSERT foo.dbo.items (name) VALUES (N'x') END TRY BEGIN CATCH IF XACT_STATE() <> -1 THROW 50001, 'not doomed', 1; ROLLBACK END CATCH",
    );
    assert_eq!(session.transactions, 0);
    // One database per transaction works, in either direction.
    ok(
        &mut session,
        "BEGIN TRAN; INSERT foo.dbo.items (name) VALUES (N'x'); UPDATE foo.dbo.items SET v = 'w' WHERE name = N'x'; SELECT COUNT(*) FROM dbo.loc; COMMIT",
    );
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 2);
    assert_eq!(catalog(&session), "memory.dbo");
}

#[test]
fn a_failed_statement_in_another_database_restores_the_catalog_after_rollback() {
    let (_server, mut session) = fixture();
    // The conversion error aborts DuckDB's transaction, where USE cannot
    // run; the catalog comes back at ROLLBACK.
    let (out, success) = run(
        &mut session,
        "BEGIN TRAN; INSERT foo.dbo.items (name, v) VALUES (N'x', 'y'); UPDATE foo.dbo.items SET v = CAST(CAST('nope' AS INT) AS VARCHAR(10))",
    );
    assert!(!success, "{:?}", errors(&out));
    assert_eq!(session.transactions, 1);
    ok(&mut session, "ROLLBACK");
    assert_eq!(catalog(&session), "memory.dbo");
    ok(&mut session, "SELECT label FROM dbo.loc");
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 1);
}

#[test]
fn preparation_binds_other_databases_without_running() {
    let (_server, session) = fixture();
    for sql in [
        "SELECT name, LEN(name) FROM foo.dbo.items WHERE id = @id",
        "SELECT l.label, i.name FROM dbo.loc l JOIN foo.dbo.items i ON i.id = l.id WHERE i.id = @id",
        "INSERT foo.dbo.items (name) SELECT label FROM dbo.loc WHERE id = @id",
    ] {
        session
            .validate_prepared_sql(sql, &[("@id".into(), Type::Int)])
            .unwrap_or_else(|error| panic!("{sql}: {error:#}"));
        assert_eq!(catalog(&session), "memory.dbo", "{sql}");
    }
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 1);
}

#[test]
fn single_user_databases_admit_only_their_user() {
    let server = Server::open(":memory:").unwrap();
    let mut owner = session(&server);
    ok(&mut owner, "CREATE DATABASE solo");
    ok(
        &mut owner,
        "USE solo; CREATE TABLE dbo.t (id INT); INSERT dbo.t VALUES (1); USE master",
    );
    ok(&mut owner, "ALTER DATABASE solo SET SINGLE_USER");
    let mut other = session(&server);
    assert_eq!(
        fails(&mut other, "SELECT id FROM solo.dbo.t"),
        (
            924,
            1,
            14,
            "Database 'solo' is already open and can only have one user at a time.".into()
        )
    );
    assert_eq!(
        fails(
            &mut other,
            "SELECT o.id FROM solo.dbo.t o CROSS JOIN sys.objects"
        )
        .0,
        924
    );
    ok(&mut owner, "SELECT id FROM solo.dbo.t");
    assert_eq!(catalog(&other), "memory.dbo");
    // A statement in progress keeps the database in use, like USE.
    ok(&mut owner, "ALTER DATABASE solo SET MULTI_USER");
    ok(&mut other, "SELECT id FROM solo.dbo.t");
}

#[test]
fn aliases_exempt_only_the_update_or_delete_target_they_name() {
    let (_server, mut session) = fixture();
    // `loc` is an outer alias of foo's table, but the inner `loc` is
    // master's table: the statement reads both databases.
    ok(
        &mut session,
        "IF (SELECT COUNT(*) FROM foo.dbo.items AS loc WHERE EXISTS (SELECT 1 FROM loc AS inner_loc WHERE inner_loc.label = N'one')) <> 1 THROW 50001, 'wrong loc', 1",
    );
    ok(
        &mut session,
        "UPDATE loc SET v = 'a' FROM foo.dbo.items AS loc WHERE EXISTS (SELECT 1 FROM dbo.loc AS l WHERE l.id = loc.id)",
    );
    assert_eq!(
        native(&session, "SELECT v FROM foo.dbo.items WHERE id = 1"),
        "a"
    );
    // Only the target node is the alias; an inner `i` is master's table.
    ok(
        &mut session,
        "CREATE TABLE dbo.i (id INT); INSERT dbo.i VALUES (1)",
    );
    ok(
        &mut session,
        "UPDATE i SET v = 'b' FROM foo.dbo.items AS i WHERE EXISTS (SELECT 1 FROM i AS inner_i WHERE inner_i.id = i.id)",
    );
    assert_eq!(
        native(&session, "SELECT v FROM foo.dbo.items WHERE id = 1"),
        "b"
    );
    ok(
        &mut session,
        "INSERT foo.dbo.items (name) VALUES (N'gone'); DELETE i FROM foo.dbo.items AS i WHERE NOT EXISTS (SELECT 1 FROM i AS inner_i WHERE inner_i.id = i.id)",
    );
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 1);
    assert_eq!(catalog(&session), "memory.dbo");
}

#[test]
fn preparing_expressions_checks_access_to_other_databases() {
    let server = Server::open(":memory:").unwrap();
    let mut owner = session(&server);
    ok(&mut owner, "CREATE DATABASE solo");
    ok(
        &mut owner,
        "USE solo; CREATE TABLE dbo.t (id INT); INSERT dbo.t VALUES (1); USE master",
    );
    let other = session(&server);
    for sql in [
        "IF EXISTS (SELECT 1 FROM solo.dbo.t WHERE id = @id) SELECT 1",
        "DECLARE @n INT = (SELECT COUNT(*) FROM solo.dbo.t WHERE id = @id)",
    ] {
        other
            .validate_prepared_sql(sql, &[("@id".into(), Type::Int)])
            .unwrap_or_else(|error| panic!("{sql}: {error:#}"));
    }
    ok(&mut owner, "ALTER DATABASE solo SET SINGLE_USER");
    for sql in [
        "IF EXISTS (SELECT 1 FROM solo.dbo.t WHERE id = @id) SELECT 1",
        "DECLARE @n INT = (SELECT COUNT(*) FROM solo.dbo.t WHERE id = @id)",
    ] {
        let error = other
            .validate_prepared_sql(sql, &[("@id".into(), Type::Int)])
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<msduck_core::diagnostic::SqlError>()
                .map(|error| error.number),
            Some(924),
            "{sql}: {error:#}"
        );
    }
    assert_eq!(catalog(&other), "memory.dbo");
}

#[test]
fn nested_statements_reuse_the_session_s_own_uses_of_other_databases() {
    let server = Server::open(":memory:").unwrap();
    let mut owner = session(&server);
    ok(&mut owner, "CREATE DATABASE solo");
    ok(
        &mut owner,
        "USE solo; CREATE TABLE dbo.t (id INT); INSERT dbo.t VALUES (1); USE master",
    );
    // A trigger in master reads solo while the triggering statement, which
    // also reads solo, holds it.
    ok(
        &mut owner,
        "CREATE TABLE dbo.copy (id INT); CREATE TABLE dbo.log (n INT)",
    );
    ok(
        &mut owner,
        "CREATE TRIGGER copy_log ON dbo.copy AFTER INSERT AS INSERT dbo.log SELECT COUNT(*) FROM solo.dbo.t",
    );
    ok(&mut owner, "ALTER DATABASE solo SET SINGLE_USER");
    ok(&mut owner, "INSERT dbo.copy SELECT id FROM solo.dbo.t");
    assert_eq!(count(&owner, "SELECT count(*) FROM memory.dbo.log"), 1);
    // A statement in solo whose trigger reads master and solo again.
    ok(
        &mut owner,
        "USE solo; CREATE TABLE dbo.u (id INT); CREATE TABLE dbo.ulog (n INT)",
    );
    ok(
        &mut owner,
        "CREATE TRIGGER u_log ON dbo.u AFTER INSERT AS INSERT dbo.ulog SELECT COUNT(*) FROM master.dbo.copy CROSS JOIN dbo.t",
    );
    ok(&mut owner, "USE master; INSERT solo.dbo.u VALUES (1)");
    assert_eq!(count(&owner, "SELECT count(*) FROM solo.dbo.ulog"), 1);
    let mut other = session(&server);
    assert_eq!(fails(&mut other, "SELECT id FROM solo.dbo.t").0, 924);
    assert_eq!(catalog(&owner), "memory.dbo");
}

#[test]
fn ctes_and_table_functions_hide_only_their_own_names() {
    let (_server, mut session) = fixture();
    // A CTE hides only its own name; `loc` is master's table.
    ok(
        &mut session,
        "WITH c AS (SELECT id FROM foo.dbo.items) UPDATE foo.dbo.items SET v = 'c' WHERE id IN (SELECT id FROM c) AND EXISTS (SELECT 1 FROM loc)",
    );
    assert_eq!(
        native(&session, "SELECT v FROM foo.dbo.items WHERE id = 1"),
        "c"
    );
    // A table spelled like a table function stays a table.
    ok(
        &mut session,
        "CREATE TABLE dbo.string_split (value NVARCHAR(10)); INSERT dbo.string_split VALUES (N'kept')",
    );
    ok(
        &mut session,
        "IF (SELECT COUNT(*) FROM foo.dbo.items i CROSS JOIN [string_split] t CROSS APPLY STRING_SPLIT(N'a,b', N',') f WHERE t.value = N'kept') <> 2 THROW 50001, 'wrong string_split', 1",
    );
    assert_eq!(catalog(&session), "memory.dbo");
}

#[test]
fn procedure_bodies_report_writes_to_a_second_database() {
    let (_server, mut session) = fixture();
    ok(
        &mut session,
        "CREATE PROCEDURE dbo.two AS BEGIN INSERT foo.dbo.items (name) VALUES (N'p'); INSERT dbo.loc VALUES (9, N'nine') END",
    );
    let (number, state, class, message) = fails(&mut session, "BEGIN TRAN; EXEC dbo.two; SELECT 1");
    assert_eq!((number, state, class), (40515, 1, 16));
    assert_eq!(
        message,
        "unsupported cross-database transaction: database 'master' cannot be modified in a transaction that has already modified database 'foo'; a transaction may write only one database"
    );
    assert_eq!(session.transactions, 0);
    assert_eq!(count(&session, "SELECT count(*) FROM foo.dbo.items"), 1);
    assert_eq!(count(&session, "SELECT count(*) FROM memory.dbo.loc"), 1);
    assert_eq!(catalog(&session), "memory.dbo");
    ok(&mut session, "SELECT label FROM dbo.loc");
}

#[test]
fn only_duckdb_reports_writes_to_a_second_database() {
    let (_server, mut session) = fixture();
    let (number, _, _, message) = fails(
        &mut session,
        "BEGIN TRAN; INSERT dbo.loc VALUES (7, N'seven'); THROW 50000, 'TransactionContext Error: Attempting to write to database \"foo\" in a transaction that has already modified database \"memory\"', 1",
    );
    assert_eq!(number, 50000);
    assert!(
        message.starts_with("TransactionContext Error: Attempting"),
        "{message}"
    );
    // THROW ends the batch but keeps the transaction and its work.
    assert_eq!(session.transactions, 1);
    assert_eq!(count(&session, "SELECT count(*) FROM memory.dbo.loc"), 2);
    ok(&mut session, "ROLLBACK");
    assert_eq!(count(&session, "SELECT count(*) FROM memory.dbo.loc"), 1);
}
