//! Computed columns over Unicode and JSON expressions, and column DEFAULTs
//! that read session state (issue #718). Values and errors follow
//! reference/gaps-computed.json; tests/compat/computed.test.mjs covers the
//! same cases through tedious. See docs/gaps-computed.md.
use msduck::engine::Session;
use msduck::server::Server;

/// The first error of a batch: number, state and message.
fn error(session: &mut Session, sql: &str) -> Option<(i32, u8, String)> {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    let mut at = 0;
    while at < tokens.len() {
        let kind = tokens[at];
        at += 1;
        match kind {
            0xfd..=0xff => at += 12,
            0x79 => at += 4,
            0xaa | 0xab | 0xe3 => {
                let length = u16::from_le_bytes([tokens[at], tokens[at + 1]]) as usize;
                let body = &tokens[at + 2..at + 2 + length];
                if kind == 0xaa {
                    let number = i32::from_le_bytes(body[0..4].try_into().unwrap());
                    let units = u16::from_le_bytes([body[6], body[7]]) as usize;
                    let message = String::from_utf16(
                        &body[8..8 + units * 2]
                            .chunks_exact(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                    assert!(!ok, "{sql}");
                    return Some((number, body[4], message));
                }
                at += 2 + length;
            }
            // Result sets are read through the backend below.
            _ => break,
        }
    }
    assert!(ok, "{sql}");
    None
}

fn run(session: &mut Session, sql: &str) {
    assert_eq!(error(session, sql), None, "{sql}");
}

fn fails(session: &mut Session, sql: &str) -> (i32, u8, String) {
    error(session, sql).unwrap_or_else(|| panic!("{sql} succeeded"))
}

/// Rows of a backend query as text; Unicode carriers are decoded.
fn rows(session: &Session, sql: &str) -> Vec<Vec<Option<String>>> {
    let mut statement = session.db.prepare(sql).unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut result = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let columns = row.as_ref().column_count();
        result.push(
            (0..columns)
                .map(|i| row.get::<_, Option<String>>(i).unwrap())
                .collect(),
        );
    }
    result
}

fn text(values: &[Option<&str>]) -> Vec<Option<String>> {
    values.iter().map(|v| v.map(str::to_owned)).collect()
}

fn session(server: &Server) -> Session {
    Session::new(server.connection().unwrap()).unwrap()
}

#[test]
fn json_value_over_nvarchar_max_is_persisted_read_and_indexed() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE items (id int NOT NULL, body nvarchar(max) NOT NULL, value AS (CONVERT(nvarchar(200), JSON_VALUE(body, N'$.value'))) PERSISTED)",
    );
    run(
        &mut s,
        "INSERT items (id, body) VALUES (1, N'{\"value\":\"abc\"}'), (2, N'{\"value\":\"\u{fc}\u{1F986}\"}'), (3, N'{\"other\":1}'), (4, N'{\"value\":12.5}')",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(id AS VARCHAR), value FROM items ORDER BY id"
        ),
        vec![
            text(&[Some("1"), Some("abc")]),
            text(&[Some("2"), Some("\u{fc}\u{1F986}")]),
            text(&[Some("3"), None]),
            text(&[Some("4"), Some("12.5")]),
        ]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT name, CAST(is_computed AS VARCHAR), CAST(max_length AS VARCHAR), (SELECT t.name FROM sys.types t WHERE t.user_type_id = c.user_type_id) FROM sys.columns c WHERE object_id = __msduck_object_id('dbo.items', 'U') ORDER BY column_id"
        ),
        vec![
            text(&[Some("id"), Some("false"), Some("4"), Some("int")]),
            text(&[Some("body"), Some("false"), Some("-1"), Some("nvarchar")]),
            text(&[Some("value"), Some("true"), Some("400"), Some("nvarchar")]),
        ]
    );
    run(&mut s, "CREATE INDEX ix_items_value ON items(value)");
    run(
        &mut s,
        "UPDATE items SET body = N'{\"value\":\"xyz\"}' WHERE id = 1",
    );
    assert_eq!(
        rows(&s, "SELECT value FROM items WHERE id = 1"),
        vec![text(&[Some("xyz")])]
    );
    let write = (
        271,
        1,
        "The column \"value\" cannot be modified because it is either a computed column or is the result of a UNION operator.".to_owned(),
    );
    assert_eq!(
        fails(
            &mut s,
            "INSERT items (id, body, value) VALUES (6, N'{}', N'x')"
        ),
        write
    );
    assert_eq!(fails(&mut s, "UPDATE items SET value = N'x'"), write);
    // The persisted expression is evaluated by the write, as in SQL Server,
    // but the error is DuckDB's (SQL Server: 13609).
    let (number, _, message) = fails(&mut s, "INSERT items (id, body) VALUES (5, N'not json')");
    assert_eq!(number, 50000);
    assert!(
        message.contains("Incorrect value for generated column"),
        "{message}"
    );
    assert_eq!(
        rows(&s, "SELECT CAST(count(*) AS VARCHAR) FROM items"),
        vec![text(&[Some("4")])]
    );
}

#[test]
fn unicode_expressions_bind_their_declared_types() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE items (id int NOT NULL, first nvarchar(50) NULL, last nvarchar(50) NULL, body nvarchar(max) NULL, code nchar(4) NULL, full_name AS (first + N' ' + last), upper_first AS UPPER(first), body_length AS LEN(body), id_text AS CONVERT(nvarchar(10), id), initials AS LEFT(first, 1) + LEFT(last, 1), json_name AS JSON_VALUE(body, N'$.name'), code_trim AS RTRIM(code), copy AS (body + N'!') PERSISTED, head AS CONVERT(nvarchar(20), LEFT(body, 3)) PERSISTED)",
    );
    run(
        &mut s,
        "INSERT items (id, first, last, body, code) VALUES (1, N'Ann', N'Lee', N'{\"name\":\"\u{17e}\u{1F986}\"}', N'ab'), (2, N'b\u{f6}b', NULL, NULL, NULL)",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT full_name, upper_first, CAST(body_length AS VARCHAR), id_text, initials, json_name, code_trim, copy, head FROM items ORDER BY id"
        ),
        vec![
            text(&[
                Some("Ann Lee"),
                Some("ANN"),
                Some("14"),
                Some("1"),
                Some("AL"),
                Some("\u{17e}\u{1F986}"),
                Some("ab"),
                Some("{\"name\":\"\u{17e}\u{1F986}\"}!"),
                Some("{\"n"),
            ]),
            text(&[
                None,
                Some("B\u{d6}B"),
                None,
                Some("2"),
                None,
                None,
                None,
                None,
                None
            ]),
        ]
    );
    // Declarations captured from SQL Server: type, max_length.
    assert_eq!(
        rows(
            &s,
            "SELECT name, (SELECT t.name FROM sys.types t WHERE t.user_type_id = c.user_type_id), CAST(max_length AS VARCHAR) FROM sys.columns c WHERE object_id = __msduck_object_id('dbo.items', 'U') AND is_computed ORDER BY column_id"
        ),
        [
            ("full_name", "nvarchar", "202"),
            ("upper_first", "nvarchar", "100"),
            ("body_length", "bigint", "8"),
            ("id_text", "nvarchar", "20"),
            ("initials", "nvarchar", "4"),
            ("json_name", "nvarchar", "8000"),
            ("code_trim", "nvarchar", "8"),
            ("copy", "nvarchar", "-1"),
            ("head", "nvarchar", "40"),
        ]
        .iter()
        .map(|(a, b, c)| text(&[Some(a), Some(b), Some(c)]))
        .collect::<Vec<_>>()
    );
    run(&mut s, "CREATE INDEX ix_items_upper ON items(upper_first)");
    run(&mut s, "CREATE INDEX ix_items_head ON items(head)");
    assert_eq!(
        fails(&mut s, "CREATE INDEX ix_items_copy ON items(copy)"),
        (
            1919,
            1,
            "Column 'copy' in table 'items' is of a type that is invalid for use as a key column in an index.".into()
        )
    );
}

#[test]
fn computed_columns_over_other_carrier_types() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(
        &mut s,
        "CREATE TABLE items (id int NOT NULL, stamp datetime2(3) NULL, amount money NULL, bin varbinary(8) NULL, code nchar(4) NULL, day AS CONVERT(date, stamp), label AS CONVERT(nvarchar(30), amount), later AS DATEADD(day, 1, stamp), doubled AS amount * 2, padded AS code + N'|', blen AS DATALENGTH(bin))",
    );
    run(
        &mut s,
        "INSERT items (id, stamp, amount, bin, code) VALUES (1, '2024-01-02 03:04:05.678', 12.5, 0x0A0B, N'ab'), (2, NULL, NULL, NULL, NULL)",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(day AS VARCHAR), label, CAST(doubled AS VARCHAR), padded, CAST(blen AS VARCHAR) FROM items ORDER BY id"
        ),
        vec![
            text(&[
                Some("2024-01-02"),
                Some("12.50"),
                Some("25.0000"),
                Some("ab  |"),
                Some("2")
            ]),
            text(&[None, None, None, None, None]),
        ]
    );
}

#[test]
fn computed_column_errors_over_unicode_columns() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    assert_eq!(
        fails(
            &mut s,
            "CREATE TABLE items (a nvarchar(10) NULL, b AS UPPER(a), c AS b + N'x')"
        ),
        (
            1759,
            0,
            "Computed column 'b' in table 'items' is not allowed to be used in another computed-column definition.".into()
        )
    );
    assert_eq!(
        fails(
            &mut s,
            "CREATE TABLE items (a nvarchar(10) NULL, b AS UPPER(missing))"
        ),
        (207, 1, "Invalid column name 'missing'.".into())
    );
    assert_eq!(
        fails(
            &mut s,
            "CREATE TABLE items (a nvarchar(10) NULL, b AS a + CONVERT(nvarchar(36), NEWID()) PERSISTED)"
        ),
        (
            4936,
            1,
            "Computed column 'b' in table 'items' cannot be persisted because the column is non-deterministic.".into()
        )
    );
    // Session functions would be fixed to the creating session.
    for (sql, number) in [
        (
            "CREATE TABLE items (a nvarchar(10) NULL, b AS a + HOST_NAME() PERSISTED)",
            4936,
        ),
        (
            "CREATE TABLE items (a nvarchar(10) NULL, b AS a + N'@' + SUSER_SNAME())",
            40515,
        ),
        ("CREATE TABLE items (a int NULL, b AS a + @@SPID)", 40515),
    ] {
        assert_eq!(fails(&mut s, sql).0, number, "{sql}");
    }
}

#[test]
fn session_defaults_are_evaluated_in_the_inserting_session() {
    let server = Server::open(":memory:").unwrap();
    let mut a = session(&server);
    let mut b = session(&server);
    run(
        &mut a,
        "CREATE TABLE items (id int NOT NULL, value nvarchar(100) NULL DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))), n int NULL DEFAULT (CONVERT(int, SESSION_CONTEXT(N'n'))), who nvarchar(128) NULL DEFAULT (SUSER_SNAME()), short_who nvarchar(1) NULL DEFAULT (CONVERT(nvarchar(1), SYSTEM_USER)))",
    );
    run(&mut a, "INSERT items (id) VALUES (1)");
    run(
        &mut a,
        "EXEC sp_set_session_context N'foo', N'bar'; EXEC sp_set_session_context N'n', 42",
    );
    run(&mut a, "INSERT items (id) VALUES (2)");
    run(
        &mut b,
        "EXEC sp_set_session_context N'fOo', N'other'; INSERT items (id) VALUES (3)",
    );
    run(
        &mut a,
        "EXEC sp_set_session_context N'foo', N'\u{fc}\u{1F986}'; INSERT items (id) SELECT 4; INSERT items (id, value) VALUES (5, DEFAULT); INSERT items (id, value) VALUES (6, N'explicit')",
    );
    assert_eq!(
        rows(
            &a,
            "SELECT CAST(id AS VARCHAR), __msduck_carrier_utf8(value), CAST(n AS VARCHAR), __msduck_carrier_utf8(who), __msduck_carrier_utf8(short_who) FROM items ORDER BY id"
        ),
        vec![
            text(&[Some("1"), None, None, Some("sa"), Some("s")]),
            text(&[Some("2"), Some("bar"), Some("42"), Some("sa"), Some("s")]),
            // Session b's key fOo matches foo (same final character); b has no n.
            text(&[Some("3"), Some("other"), None, Some("sa"), Some("s")]),
            text(&[
                Some("4"),
                Some("\u{fc}\u{1F986}"),
                Some("42"),
                Some("sa"),
                Some("s")
            ]),
            text(&[
                Some("5"),
                Some("\u{fc}\u{1F986}"),
                Some("42"),
                Some("sa"),
                Some("s")
            ]),
            text(&[
                Some("6"),
                Some("explicit"),
                Some("42"),
                Some("sa"),
                Some("s")
            ]),
        ]
    );
    // Conversions apply to the stored base type.
    run(
        &mut a,
        "CREATE TABLE conversions (id int NOT NULL, label varchar(20) NULL DEFAULT (CONVERT(varchar(20), SESSION_CONTEXT(N'k'))), big bigint NULL DEFAULT (CAST(SESSION_CONTEXT(N'k') AS bigint)), flag bit NULL DEFAULT (CONVERT(bit, SESSION_CONTEXT(N'b'))))",
    );
    run(
        &mut a,
        "EXEC sp_set_session_context N'k', 7; EXEC sp_set_session_context N'b', 1; INSERT conversions (id) VALUES (1)",
    );
    run(
        &mut a,
        "EXEC sp_set_session_context N'k', N'12'; INSERT conversions (id) VALUES (2)",
    );
    // SQL Server: 8114. msduck reports its general conversion error.
    run(&mut a, "EXEC sp_set_session_context N'k', N'abc'");
    assert_eq!(fails(&mut a, "INSERT conversions (id) VALUES (3)").0, 245);
    assert_eq!(
        rows(
            &a,
            "SELECT CAST(id AS VARCHAR), label, CAST(big AS VARCHAR), CAST(flag AS VARCHAR) FROM conversions ORDER BY id"
        ),
        vec![
            text(&[Some("1"), Some("7"), Some("7"), Some("true")]),
            text(&[Some("2"), Some("12"), Some("12"), Some("true")]),
        ]
    );
}

#[test]
fn conditional_session_context_defaults_follow_the_stored_base_type() {
    let server = Server::open(":memory:").unwrap();
    let mut a = session(&server);
    let mut b = session(&server);
    run(
        &mut a,
        "CREATE TABLE items (id int NOT NULL, value nvarchar(100) NULL DEFAULT CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo')) ELSE NULL END, base nvarchar(128) NULL DEFAULT (CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType'))), max_length int NULL DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'MaxLength'))), total int NULL DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'TotalBytes'))), present int NULL DEFAULT (CASE WHEN SESSION_CONTEXT(N'foo') IS NULL THEN 0 ELSE 1 END), fallback nvarchar(10) NULL DEFAULT (ISNULL(CONVERT(nvarchar(10), SESSION_CONTEXT(N'foo')), N'none')))",
    );
    run(&mut a, "INSERT items (id) VALUES (1)");
    run(&mut a, "EXEC sp_set_session_context N'foo', N'bar'");
    run(&mut a, "INSERT items (id) VALUES (2)");
    // Each session's own value, row by row in multirow inserts and MERGE.
    run(
        &mut b,
        "DECLARE @v nvarchar(50) = N'\u{fc}\u{1F986}'; EXEC sp_set_session_context N'foo', @v; INSERT items (id) SELECT 3 UNION ALL SELECT 4",
    );
    run(&mut a, "EXEC sp_set_session_context N'foo', 42");
    run(
        &mut a,
        "MERGE items AS t USING (SELECT 5 AS id) AS s ON t.id = s.id WHEN NOT MATCHED THEN INSERT (id) VALUES (s.id);",
    );
    run(&mut b, "INSERT items (id, value) VALUES (6, DEFAULT)");
    assert_eq!(
        rows(
            &a,
            "SELECT CAST(id AS VARCHAR), __msduck_carrier_utf8(value), __msduck_carrier_utf8(base), CAST(max_length AS VARCHAR), CAST(total AS VARCHAR), CAST(present AS VARCHAR), __msduck_carrier_utf8(fallback) FROM items ORDER BY id"
        ),
        vec![
            text(&[Some("1"), None, None, None, None, Some("0"), Some("none")]),
            text(&[
                Some("2"),
                Some("bar"),
                Some("nvarchar"),
                Some("6"),
                Some("14"),
                Some("1"),
                Some("bar")
            ]),
            text(&[
                Some("3"),
                Some("\u{fc}\u{1F986}"),
                Some("nvarchar"),
                Some("100"),
                Some("14"),
                Some("1"),
                Some("\u{fc}\u{1F986}")
            ]),
            text(&[
                Some("4"),
                Some("\u{fc}\u{1F986}"),
                Some("nvarchar"),
                Some("100"),
                Some("14"),
                Some("1"),
                Some("\u{fc}\u{1F986}")
            ]),
            text(&[
                Some("5"),
                None,
                Some("int"),
                Some("4"),
                Some("6"),
                Some("1"),
                Some("42")
            ]),
            text(&[
                Some("6"),
                Some("\u{fc}\u{1F986}"),
                Some("nvarchar"),
                Some("100"),
                Some("14"),
                Some("1"),
                Some("\u{fc}\u{1F986}")
            ]),
        ]
    );
    // A sql_variant result into another type fails as in SQL Server.
    for sql in [
        "CREATE TABLE a (v nvarchar(10) NULL DEFAULT (ISNULL(SESSION_CONTEXT(N'foo'), N'x')))",
        "CREATE TABLE b (v nvarchar(10) NULL DEFAULT (COALESCE(SESSION_CONTEXT(N'foo'), N'x')))",
        "CREATE TABLE c (v nvarchar(10) NULL DEFAULT (CASE WHEN 1 = 1 THEN SESSION_CONTEXT(N'foo') END))",
        "CREATE TABLE d (v nvarchar(128) NULL DEFAULT (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType')))",
    ] {
        assert_eq!(
            fails(&mut a, sql),
            (
                257,
                3,
                "Implicit conversion from data type sql_variant to nvarchar is not allowed. Use the CONVERT function to run this query.".into()
            ),
            "{sql}"
        );
    }
    // Comparing the sql_variant itself stays unsupported.
    let (number, _, message) = fails(
        &mut a,
        "CREATE TABLE e (v int NULL DEFAULT (CASE WHEN SESSION_CONTEXT(N'foo') = 1 THEN 1 END))",
    );
    assert_eq!(
        (number, message.as_str()),
        (
            40515,
            "unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT"
        )
    );
}

#[test]
fn session_context_properties_in_queries() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "EXEC sp_set_session_context N'k', N'bar'");
    run(
        &mut s,
        "CREATE TABLE t (base nvarchar(128), max_length int, value nvarchar(100)); INSERT t SELECT CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')), CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength')), CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'k')) END",
    );
    run(&mut s, "EXEC sp_set_session_context N'k', 7");
    run(
        &mut s,
        "INSERT t SELECT CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')), CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Precision')), CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'k')) END",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT __msduck_carrier_utf8(base), CAST(max_length AS VARCHAR), __msduck_carrier_utf8(value) FROM t"
        ),
        vec![
            text(&[Some("nvarchar"), Some("6"), Some("bar")]),
            text(&[Some("int"), Some("10"), None]),
        ]
    );
    // Selected properties are sql_variant values.
    run(
        &mut s,
        "SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'), (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'missing'), 'MaxLength'))",
    );
    // SQL Server refuses implicit sql_variant writes and assignments with
    // 257; msduck refuses them explicitly instead of storing the base type.
    for sql in [
        "INSERT t (base) VALUES (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))",
        "UPDATE t SET max_length = SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength')",
        "INSERT t (base) SELECT (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))",
        "INSERT t (base) SELECT v FROM (SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS v) s",
        "WITH s AS (SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS v) INSERT t (base) SELECT v FROM s",
        // SQL Server converts these; msduck's sysname carrier would convert
        // to its struct text, so nested selected properties are refused.
        "SELECT CONVERT(nvarchar(128), p) FROM (SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS p) s",
        "INSERT t (base) SELECT CONVERT(nvarchar(128), p) FROM (SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS p) s",
        "SELECT (SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))",
        "DECLARE @v nvarchar(128); SET @v = SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')",
        "DECLARE @v nvarchar(128); SELECT @v = SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')",
    ] {
        assert_eq!(fails(&mut s, sql).0, 40515, "{sql}");
    }
}

#[test]
fn session_default_errors_and_alter_table() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    assert_eq!(
        fails(
            &mut s,
            "CREATE TABLE items (id int NOT NULL, value nvarchar(100) NULL DEFAULT (SESSION_CONTEXT(N'foo')))"
        ),
        (
            257,
            3,
            "Implicit conversion from data type sql_variant to nvarchar is not allowed. Use the CONVERT function to run this query.".into()
        )
    );
    assert_eq!(
        fails(
            &mut s,
            "CREATE TABLE items (id int NOT NULL, value int NULL DEFAULT (CONVERT(int, SESSION_CONTEXT('foo'))))"
        ),
        (
            8116,
            1,
            "Argument data type varchar is invalid for argument 1 of session_context function."
                .into()
        )
    );
    let (number, _, message) = fails(
        &mut s,
        "CREATE TABLE variants (id int NOT NULL, v sql_variant NULL DEFAULT (SESSION_CONTEXT(N'foo')))",
    );
    assert_eq!(number, 40515);
    assert!(
        message.starts_with("unsupported SESSION_CONTEXT"),
        "{message}"
    );

    run(&mut s, "CREATE TABLE items (id int NOT NULL)");
    run(&mut s, "INSERT items VALUES (1)");
    run(&mut s, "EXEC sp_set_session_context N'foo', N'bar'");
    run(
        &mut s,
        "ALTER TABLE items ADD value nvarchar(100) NULL DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))) WITH VALUES",
    );
    run(
        &mut s,
        "ALTER TABLE items ADD who nvarchar(128) NULL DEFAULT (ORIGINAL_LOGIN())",
    );
    run(&mut s, "INSERT items (id) VALUES (2)");
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(id AS VARCHAR), __msduck_carrier_utf8(value), __msduck_carrier_utf8(who) FROM items ORDER BY id"
        ),
        vec![
            text(&[Some("1"), Some("bar"), None]),
            text(&[Some("2"), Some("bar"), Some("sa")]),
        ]
    );
}

#[test]
fn computed_columns_and_session_defaults_survive_restart() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-gaps-computed-{}-{}",
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
        run(
            &mut s,
            "CREATE TABLE items (id int NOT NULL, body nvarchar(max) NULL, value AS (CONVERT(nvarchar(200), JSON_VALUE(body, N'$.v'))) PERSISTED, tenant nvarchar(50) NULL DEFAULT (CONVERT(nvarchar(50), SESSION_CONTEXT(N'tenant'))))",
        );
        run(
            &mut s,
            "EXEC sp_set_session_context N'tenant', N'first'; INSERT items (id, body) VALUES (1, N'{\"v\":\"one\"}')",
        );
    }
    {
        let server = Server::open(path).unwrap();
        let mut s = session(&server);
        run(
            &mut s,
            "INSERT items (id, body) VALUES (2, N'{\"v\":\"two\"}')",
        );
        run(
            &mut s,
            "EXEC sp_set_session_context N'tenant', N'second'; INSERT items (id, body) VALUES (3, NULL)",
        );
        assert_eq!(
            rows(
                &s,
                "SELECT CAST(id AS VARCHAR), value, __msduck_carrier_utf8(tenant) FROM items ORDER BY id"
            ),
            vec![
                text(&[Some("1"), Some("one"), Some("first")]),
                text(&[Some("2"), Some("two"), None]),
                text(&[Some("3"), None, Some("second")]),
            ]
        );
        assert_eq!(
            rows(
                &s,
                "SELECT name FROM sys.columns c WHERE object_id = __msduck_object_id('dbo.items', 'U') AND is_computed"
            ),
            vec![text(&[Some("value")])]
        );
    }
    std::fs::remove_dir_all(&directory).unwrap();
}
