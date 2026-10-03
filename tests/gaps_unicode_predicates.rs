//! Comparisons, LIKE, concatenation and conversions over Unicode carrier
//! columns (docs/gaps-unicode-predicates.md), through the engine. Results are
//! materialized into tables and read back from DuckDB. Definitions DuckDB
//! keeps (views, computed columns) work again after a restart that replays
//! the write-ahead log.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) {
    let (response, ok) = run(session, sql);
    assert!(ok && !response.contains(&0xAA), "{sql}: {response:?}");
}

/// The ids of `dbo.t` rows matching `filter`, in order.
fn ids(session: &mut Session, filter: &str) -> String {
    ok(session, "DELETE FROM dbo.found");
    ok(
        session,
        &format!("INSERT dbo.found SELECT id FROM dbo.t AS t WHERE {filter}"),
    );
    session
        .db
        .query_row(
            "SELECT coalesce(string_agg(CAST(id AS VARCHAR), ',' ORDER BY id), '') FROM dbo.found",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// One text value, computed by `expression` over `dbo.t` row `id`.
fn text(session: &mut Session, expression: &str, id: i32) -> Option<String> {
    ok(session, "DELETE FROM dbo.texts");
    ok(
        session,
        &format!("INSERT dbo.texts SELECT {expression} FROM dbo.t WHERE id = {id}"),
    );
    session
        .db
        .query_row("SELECT value FROM dbo.texts", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap()
}

const SETUP: &str =
    "CREATE TABLE dbo.t (id int NOT NULL, n nvarchar(20) NULL, c nchar(4) NULL, v varchar(20) NULL)
CREATE TABLE dbo.found (id int NOT NULL)
CREATE TABLE dbo.texts (value varchar(100) NULL)
INSERT dbo.t VALUES
  (1, N'x', N'x', 'x'),
  (2, N'x  ', N'x', 'y'),
  (3, N'X', N'X', 'X'),
  (4, N'a' + NCHAR(0), N'a', NULL),
  (5, N'a', N'a', 'a'),
  (6, NCHAR(256), NCHAR(256), NULL),
  (7, N'\u{1F986}', N'\u{1F986}', NULL),
  (8, NCHAR(57344), NCHAR(57344), NULL),
  (9, CAST(0x3DD8 AS nvarchar(1)), CAST(0x3DD8 AS nvarchar(1)), NULL),
  (10, NULL, NULL, NULL),
  (11, N'ab%_[c]', N'ab%_', 'ab')";

#[test]
fn comparisons_follow_the_default_collation_and_ignore_trailing_spaces() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, SETUP);
    let s = &mut session;
    // SQL_Latin1_General_CP1_CI_AS: case-insensitive, and NUL and
    // surrogates (so supplementary characters) are ignored.
    assert_eq!(ids(s, "n = N'x'"), "1,2,3");
    assert_eq!(ids(s, "n = 'x   '"), "1,2,3");
    assert_eq!(ids(s, "n <> N'x'"), "4,5,6,7,8,9,11");
    assert_eq!(ids(s, "n = v"), "1,3,5");
    assert_eq!(ids(s, "c = N'x'"), "1,2,3");
    // The ignored surrogates leave empty strings, which sort first; N'a' +
    // NCHAR(0) equals N'a'.
    assert_eq!(ids(s, "n < N'a'"), "7,9");
    // U+0100 sorts beside a; U+E000 after letters.
    assert_eq!(ids(s, "n > N'x'"), "8");
    assert_eq!(ids(s, "n > N'\u{1F986}'"), "1,2,3,4,5,6,8,11");
    assert_eq!(ids(s, "n BETWEEN NCHAR(256) AND N'\u{1F986}'"), "");
    assert_eq!(
        ids(s, "n IN (N'X', 'a', CAST(0x3DD8 AS nvarchar(1)))"),
        "1,2,3,4,5,7,9"
    );
    assert_eq!(ids(s, "n NOT IN (N'x', NULL)"), "");
    assert_eq!(ids(s, "n IN (SELECT v FROM dbo.t)"), "1,2,3,4,5");
    assert_eq!(ids(s, "v IN (SELECT n FROM dbo.t)"), "1,3,5");
    assert_eq!(ids(s, "n IS NULL"), "10");
    // CASE, joins and correlated subqueries.
    assert_eq!(ids(s, "CASE n WHEN N'x' THEN 1 ELSE 0 END = 1"), "1,2,3");
    assert_eq!(
        ids(
            s,
            "EXISTS (SELECT 1 FROM dbo.t u WHERE u.n = t.n AND u.id <> t.id)"
        ),
        "1,2,3,4,5,7,9"
    );
}

#[test]
fn like_follows_sql_server_unicode_patterns() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, SETUP);
    let s = &mut session;
    assert_eq!(ids(s, "n LIKE N'x%'"), "1,2,3");
    // Trailing spaces are significant in Unicode LIKE.
    assert_eq!(ids(s, "n LIKE N'x'"), "1,3");
    assert_eq!(ids(s, "c LIKE N'x'"), "");
    // Ranges follow the collation's order (U+0100 beside a); ignored units
    // (NUL, surrogates) are not characters.
    assert_eq!(ids(s, "n LIKE N'[a-x]'"), "1,3,4,5,6");
    assert_eq!(ids(s, "n LIKE N'[^a-x]%'"), "8");
    assert_eq!(ids(s, "n LIKE N'_'"), "1,3,4,5,6,8");
    assert_eq!(ids(s, "n LIKE N'__'"), "");
    assert_eq!(ids(s, "n LIKE N'ab!%!_![c]' ESCAPE '!'"), "11");
    assert_eq!(ids(s, "n LIKE N'%[%]%'"), "11");
    assert_eq!(ids(s, "n NOT LIKE N'%x%'"), "4,5,6,7,8,9,11");
    let (response, _) = run(s, "SELECT id FROM dbo.t WHERE n LIKE N'x' ESCAPE 'ab'");
    let message = "The invalid escape character \"ab\" was specified in a LIKE predicate.";
    let units: Vec<u8> = message.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut body = 506i32.to_le_bytes().to_vec();
    body.extend([2, 16]);
    body.extend((message.encode_utf16().count() as u16).to_le_bytes());
    body.extend(units);
    assert!(response.windows(body.len()).any(|w| w == body.as_slice()));
}

#[test]
fn concatenation_and_conversions_produce_text() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, SETUP);
    let s = &mut session;
    assert_eq!(text(s, "n + N'!'", 1).as_deref(), Some("x!"));
    assert_eq!(text(s, "c + N'|'", 1).as_deref(), Some("x   |"));
    assert_eq!(text(s, "v + n", 3).as_deref(), Some("XX"));
    assert_eq!(text(s, "CONVERT(nvarchar(2), n)", 2).as_deref(), Some("x "));
    assert_eq!(
        text(s, "CAST(c AS nvarchar(10))", 5).as_deref(),
        Some("a   ")
    );
    assert_eq!(text(s, "CAST(n AS nchar(3))", 1).as_deref(), Some("x  "));
    assert_eq!(text(s, "CONCAT(n, N'-', c)", 3).as_deref(), Some("X-X   "));
    assert_eq!(text(s, "ISNULL(n, N'-')", 10).as_deref(), Some("-"));
    assert_eq!(text(s, "COALESCE(n, v)", 1).as_deref(), Some("x"));
    assert_eq!(
        text(s, "CASE WHEN id = 1 THEN n ELSE N'other' END", 1).as_deref(),
        Some("x")
    );
    assert_eq!(
        text(s, "REPLACE(n, N'%', N'p')", 11).as_deref(),
        Some("abp_[c]")
    );
    assert_eq!(text(s, "n", 10), None);
}

#[test]
fn views_and_computed_columns_survive_restart_and_write_ahead_log_replay() {
    let directory =
        std::env::temp_dir().join(format!("msduck-unicode-predicates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("predicates.duckdb");
    let path = path.to_str().unwrap();
    let checks = |session: &mut Session| {
        assert_eq!(ids(session, "id IN (SELECT id FROM dbo.v)"), "1,2,3,8");
        assert_eq!(
            ids(session, "id IN (SELECT id FROM dbo.w WHERE flag = 1)"),
            "1,2"
        );
    };
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        ok(&mut session, SETUP);
        ok(
            &mut session,
            // Computed columns over NVARCHAR are not supported yet, but the
            // dispatch also wraps this comparison in DuckDB's definition.
            "CREATE TABLE dbo.w (id int NOT NULL, n nvarchar(10) NULL, flag AS (CASE WHEN id <= 2 THEN 1 ELSE 0 END))
             INSERT dbo.w (id, n) SELECT id, n FROM dbo.t",
        );
        ok(
            &mut session,
            "CREATE VIEW dbo.v AS SELECT id FROM dbo.t WHERE n LIKE N'x%' OR n > N'x'",
        );
        checks(&mut session);
    }
    for _ in 0..2 {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        checks(&mut session);
    }
    let _ = std::fs::remove_dir_all(&directory);
}
