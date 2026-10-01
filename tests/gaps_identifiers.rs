//! Contextual identifiers and database-qualified diagnostics
//! (docs/gaps-identifiers.md). Expected values and messages come from
//! reference/gaps-identifiers.json, captured from SQL Server 2022.
use msduck::engine::Session;
use msduck::server::Server;
use std::collections::HashMap;

fn session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    (server, session)
}

fn ok(session: &mut Session, sql: &str) {
    let (tokens, ok) = session.batch_response(sql, &HashMap::new(), false, None);
    assert!(ok, "{sql}: {:?}", error(&tokens));
}

/// The first ERROR token of a response whose first token is ERROR (a
/// failed single statement), as (number, state, class, message).
fn error(tokens: &[u8]) -> Option<(i32, u8, u8, String)> {
    let mut cursor = msduck_tds::Cursor::new(tokens);
    if cursor.u8().ok()? != 0xaa {
        return None;
    }
    let length = cursor.u16().ok()? as usize;
    let mut diagnostic = msduck_tds::Cursor::new(cursor.take(length).ok()?);
    let number = diagnostic.u32().ok()? as i32;
    let state = diagnostic.u8().ok()?;
    let class = diagnostic.u8().ok()?;
    let count = diagnostic.u16().ok()?;
    Some((number, state, class, diagnostic.text(count as usize).ok()?))
}

fn fails(session: &mut Session, sql: &str) -> (i32, u8, u8, String) {
    let (tokens, _) = session.batch_response(sql, &HashMap::new(), false, None);
    error(&tokens).unwrap_or_else(|| panic!("{sql} did not fail"))
}

fn values(session: &Session, sql: &str) -> Vec<i64> {
    session
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn backend_reserved_words_are_regular_identifiers() {
    let (_server, mut session) = session();
    ok(&mut session, "CREATE DATABASE foo");
    session.use_database("foo").unwrap();
    ok(&mut session, "CREATE TABLE items(offset int NOT NULL)");
    for word in [
        "offset", "limit", "qualify", "at", "using", "window", "interval", "lateral", "trim",
        "true",
    ] {
        ok(
            &mut session,
            &format!(
                "CREATE TABLE {word}({word} int NOT NULL, other int NULL);
                 INSERT INTO {word}({word}, other) VALUES (1, 10);
                 INSERT {word} VALUES (2, 20);
                 UPDATE {word} SET {word} = {word} + 10 WHERE {word} = 2;
                 CREATE INDEX ix_{word} ON {word}({word})"
            ),
        );
        ok(
            &mut session,
            &format!("CREATE VIEW v_{word} AS SELECT {word} FROM {word}"),
        );
        // Aliases with and without AS, CTE names and parameters.
        ok(
            &mut session,
            &format!(
                "DECLARE @{word} int = 12;
                 CREATE TABLE copy_{word}({word} int, other int);
                 INSERT INTO copy_{word}
                 SELECT {word}.{word} {word}, {word}.other AS other FROM dbo.{word} {word}
                 WHERE {word}.{word} = @{word};
                 WITH {word}({word}) AS (SELECT {word} FROM dbo.{word})
                 INSERT INTO copy_{word}({word}) SELECT x.{word} FROM {word} AS x WHERE x.{word} = 1"
            ),
        );
        assert_eq!(
            values(
                &session,
                &format!("SELECT \"{word}\" FROM \"{word}\" ORDER BY 1")
            ),
            [1, 12],
            "{word}"
        );
        assert_eq!(
            values(
                &session,
                &format!("SELECT \"{word}\" FROM \"v_{word}\" ORDER BY 1")
            ),
            [1, 12],
            "{word}"
        );
        assert_eq!(
            values(
                &session,
                &format!("SELECT \"{word}\" FROM \"copy_{word}\" ORDER BY 1")
            ),
            [1, 12],
            "{word}"
        );
        ok(
            &mut session,
            &format!(
                "DELETE FROM {word} WHERE {word} = 12; DROP VIEW v_{word}; DROP TABLE {word}; DROP TABLE copy_{word}"
            ),
        );
    }
}

#[test]
fn tsql_syntax_around_contextual_words_is_unchanged() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE t(offset int, at datetime2, using int);
         INSERT t VALUES (1, '2020-01-01', 1), (2, '2020-01-02', 2), (3, '2020-01-03', 3)",
    );
    ok(
        &mut session,
        "CREATE TABLE r(offset int);
         INSERT r SELECT offset FROM t ORDER BY offset OFFSET 1 ROWS FETCH NEXT 1 ROWS ONLY",
    );
    assert_eq!(values(&session, "SELECT \"offset\" FROM r"), [2]);
    // AT TIME ZONE keeps its meaning next to a column named `at`.
    ok(&mut session, "CREATE TABLE z(at datetimeoffset)");
    ok(
        &mut session,
        "INSERT z SELECT CAST(at AS datetime2) AT TIME ZONE 'UTC' at FROM t WHERE using = 1",
    );
    assert_eq!(values(&session, "SELECT count(*) FROM z"), [1]);
}

#[test]
fn truncation_names_the_current_database() {
    let (_server, mut session) = session();
    ok(&mut session, "CREATE DATABASE foo");
    session.use_database("foo").unwrap();
    ok(
        &mut session,
        "CREATE TABLE items(id int NOT NULL PRIMARY KEY, name nvarchar(3) NULL, code varchar(2) NULL)",
    );
    ok(
        &mut session,
        "INSERT INTO items(id, name) VALUES (1, N'abc')",
    );
    assert_eq!(
        fails(&mut session, "INSERT INTO items(id, name) VALUES (2, N'abcdef')"),
        (
            2628,
            1,
            16,
            "String or binary data would be truncated in table 'foo.dbo.items', column 'name'. Truncated value: 'abc'.".into()
        )
    );
    assert_eq!(
        fails(&mut session, "INSERT INTO items(id, code) VALUES (3, 'abcdef')"),
        (
            2628,
            1,
            16,
            "String or binary data would be truncated in table 'foo.dbo.items', column 'code'. Truncated value: 'ab'.".into()
        )
    );
    assert_eq!(
        fails(&mut session, "UPDATE items SET name = N'wxyz' WHERE id = 1"),
        (
            2628,
            1,
            16,
            "String or binary data would be truncated in table 'foo.dbo.items', column 'name'. Truncated value: 'wxy'.".into()
        )
    );
    // Inside an explicit transaction (the materialized write path).
    ok(&mut session, "BEGIN TRANSACTION");
    assert_eq!(
        fails(
            &mut session,
            "INSERT INTO items(id, name) VALUES (4, N'abcdef')"
        )
        .3,
        "String or binary data would be truncated in table 'foo.dbo.items', column 'name'. Truncated value: 'abc'."
    );
    ok(&mut session, "ROLLBACK");
    // master keeps its own name.
    session.use_database("master").unwrap();
    ok(&mut session, "CREATE TABLE items(name nvarchar(2))");
    assert_eq!(
        fails(&mut session, "INSERT INTO items VALUES (N'abc')").3,
        "String or binary data would be truncated in table 'master.dbo.items', column 'name'. Truncated value: 'ab'."
    );
}

#[test]
fn not_null_violation_names_the_current_database() {
    let (_server, mut session) = session();
    ok(&mut session, "CREATE DATABASE foo");
    session.use_database("foo").unwrap();
    ok(&mut session, "CREATE SCHEMA app");
    ok(
        &mut session,
        "CREATE TABLE items(id int NOT NULL, offset int NULL);
         CREATE TABLE app.items(id int NULL, offset int NOT NULL)",
    );
    assert_eq!(
        fails(&mut session, "INSERT INTO items(id, offset) VALUES (NULL, NULL)"),
        (
            515,
            2,
            16,
            "Cannot insert the value NULL into column 'id', table 'foo.dbo.items'; column does not allow nulls. INSERT fails.".into()
        )
    );
    // SQL Server follows the error with 3621 "The statement has been
    // terminated." and completes the INSERT.
    let (tokens, _) = session.batch_response(
        "INSERT INTO items(id) VALUES (NULL)",
        &HashMap::new(),
        false,
        None,
    );
    let terminated: Vec<u8> = "The statement has been terminated."
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert!(
        tokens
            .windows(terminated.len())
            .any(|w| w == terminated.as_slice())
    );
    ok(&mut session, "INSERT INTO items(id) VALUES (1)");
    assert_eq!(
        fails(&mut session, "UPDATE items SET id = NULL"),
        (
            515,
            2,
            16,
            "Cannot insert the value NULL into column 'id', table 'foo.dbo.items'; column does not allow nulls. UPDATE fails.".into()
        )
    );
    assert_eq!(
        fails(&mut session, "INSERT INTO app.items(id) VALUES (1)").3,
        "Cannot insert the value NULL into column 'offset', table 'foo.app.items'; column does not allow nulls. INSERT fails."
    );
    // The statement continues the batch and is catchable, as before.
    ok(
        &mut session,
        "CREATE TABLE log(n int, message nvarchar(400));
         BEGIN TRY INSERT INTO items(id) VALUES (NULL) END TRY
         BEGIN CATCH INSERT INTO log VALUES (ERROR_NUMBER(), ERROR_MESSAGE()) END CATCH",
    );
    assert_eq!(values(&session, "SELECT n FROM log"), [515]);
}

#[test]
fn check_constraints_on_contextual_columns_are_enforced() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE ck(id int NOT NULL, interval int NULL CHECK (interval > 0), trim int NULL, cast int NULL,
                         CONSTRAINT ck_trim CHECK (trim > 0 AND cast > 0))",
    );
    ok(&mut session, "INSERT ck VALUES (1, 1, 1, 1)");
    ok(&mut session, "UPDATE ck SET interval = 5 WHERE trim = 1");
    assert_eq!(fails(&mut session, "UPDATE ck SET interval = -5").0, 547);
    assert_eq!(fails(&mut session, "INSERT ck VALUES (2, 1, 0, 1)").0, 547);
    ok(
        &mut session,
        "ALTER TABLE ck WITH CHECK ADD CONSTRAINT ck_more CHECK (interval < 100)",
    );
    assert_eq!(values(&session, "SELECT \"interval\" FROM ck"), [5]);
}

#[test]
fn not_null_violation_in_table_variables_and_temporary_tables() {
    let (_server, mut session) = session();
    assert_eq!(
        fails(
            &mut session,
            "DECLARE @t TABLE(offset int NOT NULL); INSERT @t VALUES (NULL)"
        )
        .3,
        "Cannot insert the value NULL into column 'offset', table '@t'; column does not allow nulls. INSERT fails."
    );
}
