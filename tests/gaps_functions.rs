//! User-defined functions (docs/gaps-functions.md): definitions in the
//! module store, values of scalar and table-valued calls, statement-by-
//! statement execution of loops and recursion, and SQL Server error numbers.
use msduck::engine::Session;
use msduck::server::Server;

fn session(server: &Server) -> Session {
    Session::new(server.connection().unwrap()).unwrap()
}

fn run(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn ok(session: &mut Session, sql: &str) {
    let (_, ok) = run(session, sql);
    assert!(ok, "{sql}");
}

/// The batch fails with an ERROR token (0xAA) carrying `number`.
fn fails(session: &mut Session, sql: &str, number: i32) {
    let (out, ok) = run(session, sql);
    assert!(!ok, "{sql} succeeded");
    let found = (0..out.len().saturating_sub(7))
        .any(|i| out[i] == 0xAA && out[i + 3..i + 7] == number.to_le_bytes());
    assert!(found, "{sql}: no error {number}");
}

/// Rows of a backend query over the session's database, as text.
fn rows(session: &Session, sql: &str) -> Vec<Vec<Option<String>>> {
    let mut statement = session.db.prepare(sql).unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut result = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let count = row.as_ref().column_count();
        result.push(
            (0..count)
                .map(|i| match row.get::<_, duckdb::types::Value>(i).unwrap() {
                    duckdb::types::Value::Null => None,
                    duckdb::types::Value::Text(text) => Some(text),
                    other => Some(format!("{other:?}")),
                })
                .collect(),
        );
    }
    result
}

fn text(values: &[&[Option<&str>]]) -> Vec<Vec<Option<String>>> {
    values
        .iter()
        .map(|row| row.iter().map(|v| v.map(str::to_string)).collect())
        .collect()
}

#[test]
fn scalar_and_table_functions_return_values() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    ok(
        &mut s,
        "CREATE TABLE dbo.t(id int, v varchar(10)); INSERT dbo.t VALUES (1, 'a'), (2, 'b'), (3, NULL)",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.foo(@x int) RETURNS int AS BEGIN RETURN @x+1; END;",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.p0() RETURNS varchar(10) AS BEGIN RETURN 'hi' END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.caller(@x int) RETURNS int WITH EXECUTE AS CALLER AS BEGIN DECLARE @y int = @x * 2; IF @y > 4 RETURN -@y; RETURN @y END",
    );
    ok(
        &mut s,
        "SELECT id, dbo.foo(id) AS n, dbo.caller(id) AS c, dbo.p0() AS p INTO dbo.r1 FROM dbo.t WHERE dbo.foo(id) > 2",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(id AS VARCHAR), CAST(n AS VARCHAR), CAST(c AS VARCHAR), p FROM dbo.r1 ORDER BY id"
        ),
        text(&[
            &[Some("2"), Some("3"), Some("4"), Some("hi")],
            &[Some("3"), Some("4"), Some("-6"), Some("hi")],
        ])
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.rows_for(@id int) RETURNS TABLE AS RETURN SELECT id, v FROM dbo.t WHERE id <= @id",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.mt(@n int) RETURNS @r TABLE (i int NOT NULL, s varchar(5) DEFAULT 'd') AS BEGIN INSERT @r VALUES (@n, 'a'); IF @n > 1 INSERT INTO @r(i) SELECT @n + 10; RETURN END",
    );
    ok(
        &mut s,
        "SELECT t.id, r.id AS rid, m.i, m.s INTO dbo.r2 FROM dbo.t CROSS APPLY dbo.rows_for(t.id) r OUTER APPLY dbo.mt(r.id) m",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(id AS VARCHAR) || '/' || CAST(rid AS VARCHAR) || '/' || CAST(i AS VARCHAR) || s FROM dbo.r2 ORDER BY id, rid, i"
        ),
        text(&[
            &[Some("1/1/1a")],
            &[Some("2/1/1a")],
            &[Some("2/2/2a")],
            &[Some("2/2/12d")],
            &[Some("3/1/1a")],
            &[Some("3/2/2a")],
            &[Some("3/2/12d")],
            &[Some("3/3/3a")],
            &[Some("3/3/13d")],
        ])
    );
}

#[test]
fn loops_and_recursion_run_with_known_arguments() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    ok(
        &mut s,
        "CREATE FUNCTION dbo.fact(@n int) RETURNS bigint AS BEGIN IF @n <= 1 RETURN 1; RETURN @n * dbo.fact(@n - 1) END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.total(@x int) RETURNS int AS BEGIN DECLARE @i int = 0, @s int = 0; WHILE @i < @x BEGIN SET @i += 1; IF @i > 4 BREAK; SET @s = @s + @i END; RETURN @s END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.series(@n int) RETURNS @r TABLE (v int) AS BEGIN DECLARE @i int = 0; WHILE @i < @n BEGIN SET @i += 1; INSERT @r VALUES (@i * @i) END RETURN END",
    );
    ok(
        &mut s,
        "DECLARE @n int = 20; SELECT dbo.fact(@n) AS f, dbo.total(3) AS a, dbo.total(9) AS b INTO dbo.r",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(f AS VARCHAR), CAST(a AS VARCHAR), CAST(b AS VARCHAR) FROM dbo.r"
        ),
        text(&[&[Some("2432902008176640000"), Some("6"), Some("10")]])
    );
    ok(&mut s, "SELECT v INTO dbo.squares FROM dbo.series(4)");
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(sum(v) AS VARCHAR), CAST(count(*) AS VARCHAR) FROM dbo.squares"
        ),
        text(&[&[Some("30"), Some("4")]])
    );
    // Mutual recursion, and a non-recursive caller of a recursive function.
    ok(
        &mut s,
        "CREATE FUNCTION dbo.is_even(@n int) RETURNS bit AS BEGIN IF @n = 0 RETURN 1; RETURN dbo.is_odd(@n - 1) END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.is_odd(@n int) RETURNS bit AS BEGIN IF @n = 0 RETURN 0; RETURN dbo.is_even(@n - 1) END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.wrap(@n int) RETURNS bigint AS BEGIN RETURN dbo.fact(@n) + 1 END",
    );
    ok(
        &mut s,
        "SELECT dbo.is_even(10) AS e, dbo.is_odd(7) AS o, dbo.is_even(7) AS n, dbo.wrap(5) AS w INTO dbo.m",
    );
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(e AS VARCHAR), CAST(o AS VARCHAR), CAST(n AS VARCHAR), CAST(w AS VARCHAR) FROM dbo.m"
        ),
        text(&[&[Some("true"), Some("true"), Some("false"), Some("121")]])
    );
    // SQL Server allows 32 nesting levels.
    fails(&mut s, "SELECT dbo.fact(33)", 217);
    // Per-row loops and recursion are refused rather than approximated.
    ok(
        &mut s,
        "CREATE TABLE dbo.t(id int); INSERT dbo.t VALUES (1)",
    );
    fails(&mut s, "SELECT dbo.fact(id) FROM dbo.t", 40515);
}

#[test]
fn definitions_are_modules_with_procedure_style_properties() {
    let directory =
        std::env::temp_dir().join(format!("msduck-gaps-functions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("functions.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut s = session(&server);
        ok(
            &mut s,
            "CREATE FUNCTION dbo.f(@a int, @b varchar(5) = 'x') RETURNS nvarchar(20) WITH SCHEMABINDING AS BEGIN RETURN @b + CAST(@a AS varchar(5)) END",
        );
        ok(
            &mut s,
            "CREATE FUNCTION dbo.i(@n int) RETURNS TABLE AS RETURN SELECT @n AS n",
        );
        ok(
            &mut s,
            "CREATE FUNCTION dbo.m() RETURNS @t TABLE (a int NOT NULL) AS BEGIN RETURN END",
        );
        assert_eq!(
            rows(
                &s,
                "SELECT name, rtrim(type), type_desc FROM sys.objects WHERE name IN ('f', 'i', 'm') ORDER BY name"
            ),
            text(&[
                &[Some("f"), Some("FN"), Some("SQL_SCALAR_FUNCTION")],
                &[
                    Some("i"),
                    Some("IF"),
                    Some("SQL_INLINE_TABLE_VALUED_FUNCTION")
                ],
                &[Some("m"), Some("TF"), Some("SQL_TABLE_VALUED_FUNCTION")],
            ])
        );
        let properties: serde_json::Value = serde_json::from_str(
            &rows(
                &s,
                "SELECT properties FROM main.__msduck_modules WHERE name = 'f'",
            )[0][0]
                .clone()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            properties["parameters"],
            serde_json::json!([
                {"name": "@a", "type": "int", "output": false, "default": null, "readonly": false},
                {"name": "@b", "type": "varchar(5)", "output": false, "default": "'x'", "readonly": false},
            ])
        );
        assert_eq!(properties["returns"], "nvarchar(20)");
        assert_eq!(properties["options"]["schemabinding"], true);
        let table: serde_json::Value = serde_json::from_str(
            &rows(
                &s,
                "SELECT properties FROM main.__msduck_modules WHERE name = 'm'",
            )[0][0]
                .clone()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            table["returns"],
            serde_json::json!({"variable": "@t", "columns": [{"name": "a", "type": "int", "nullable": false}]})
        );
    }
    // Definitions survive a restart.
    let server = Server::open(path).unwrap();
    let mut s = session(&server);
    ok(&mut s, "SELECT dbo.f(1, DEFAULT) AS v INTO dbo.r");
    assert_eq!(rows(&s, "SELECT v FROM dbo.r"), text(&[&[Some("x1")]]));
    drop(s);
    drop(server);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn errors_match_sql_server_numbers() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    ok(
        &mut s,
        "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x END",
    );
    for (sql, number) in [
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS int AS BEGIN RETURN @y END",
            137,
        ),
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS int AS BEGIN SET @x = 1 END",
            455,
        ),
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS int AS BEGIN SELECT 1; RETURN 1 END",
            444,
        ),
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS int AS BEGIN PRINT 'x'; RETURN 1 END",
            443,
        ),
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS @t TABLE(a int) AS BEGIN RETURN 1 END",
            178,
        ),
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS int AS BEGIN RETURN END",
            1075,
        ),
        (
            "CREATE FUNCTION dbo.e(@x int) RETURNS int AS RETURN @x",
            102,
        ),
        (
            "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x END",
            2714,
        ),
        (
            "ALTER FUNCTION dbo.nope(@x int) RETURNS int AS BEGIN RETURN @x END",
            208,
        ),
        (
            "ALTER FUNCTION dbo.f(@x int) RETURNS TABLE AS RETURN SELECT 1 AS a",
            2010,
        ),
        (
            "SELECT 1 AS one; CREATE FUNCTION dbo.e() RETURNS int AS BEGIN RETURN 1 END",
            111,
        ),
        ("DROP FUNCTION dbo.nope", 3701),
        ("SELECT dbo.f(1, 2)", 8144),
        ("SELECT dbo.f()", 313),
        ("SELECT dbo.nope(1)", 4121),
        ("SELECT * FROM dbo.nope(1)", 208),
    ] {
        fails(&mut s, sql, number);
    }
    ok(&mut s, "CREATE TABLE dbo.g(x int, y AS dbo.f(x))");
    fails(&mut s, "DROP FUNCTION dbo.f", 3729);
    fails(
        &mut s,
        "ALTER FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN 2 END",
        3729,
    );
    fails(&mut s, "DROP FUNCTION dbo.g", 3705);
    fails(
        &mut s,
        "CREATE FUNCTION dbo.sb(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN (SELECT COUNT(*) FROM g) END",
        4512,
    );
    fails(
        &mut s,
        "CREATE FUNCTION dbo.sb(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN dbo.f(@x) END",
        4513,
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.sb(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN (SELECT COUNT(*) FROM dbo.g WHERE x = @x) END",
    );
    fails(&mut s, "DROP TABLE dbo.g", 3729);
    ok(&mut s, "DROP FUNCTION IF EXISTS dbo.nope, dbo.sb");
    ok(&mut s, "DROP TABLE dbo.g");
    ok(&mut s, "DROP FUNCTION dbo.f");
}

#[test]
fn dependencies_follow_what_actually_exists() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    ok(
        &mut s,
        "CREATE FUNCTION dbo.ff(@x int) RETURNS int AS BEGIN RETURN @x END",
    );
    // A dropped computed column and a failed CREATE TABLE leave no reference.
    ok(&mut s, "CREATE TABLE dbo.c(a int, c AS dbo.ff(a))");
    ok(&mut s, "ALTER TABLE dbo.c DROP COLUMN c");
    ok(&mut s, "ALTER TABLE dbo.c ADD c int");
    ok(
        &mut s,
        "ALTER FUNCTION dbo.ff(@x int) RETURNS int AS BEGIN RETURN @x + 1 END",
    );
    let (_, created) = run(
        &mut s,
        "CREATE TABLE dbo.u(a int, c AS dbo.ff(a), d nosuchtype)",
    );
    assert!(!created);
    ok(&mut s, "CREATE TABLE dbo.u(a int, c int)");
    ok(
        &mut s,
        "ALTER FUNCTION dbo.ff(@x int) RETURNS int AS BEGIN RETURN @x + 2 END",
    );
    // A view that cannot take the new definition undoes the ALTER.
    ok(
        &mut s,
        "CREATE FUNCTION dbo.h(@x int) RETURNS int AS BEGIN RETURN @x END",
    );
    ok(&mut s, "CREATE VIEW dbo.vh AS SELECT dbo.h(1) AS x");
    fails(
        &mut s,
        "ALTER FUNCTION dbo.h(@x int, @y int) RETURNS int AS BEGIN RETURN @x + @y END",
        313,
    );
    ok(&mut s, "SELECT dbo.h(7) AS x INTO dbo.r");
    assert_eq!(
        rows(&s, "SELECT CAST(x AS VARCHAR) FROM dbo.r"),
        text(&[&[Some("7")]])
    );
    // Views reach functions through other functions.
    ok(
        &mut s,
        "CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x * 10 END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.g(@x int) RETURNS int AS BEGIN RETURN dbo.f(@x) * 2 END",
    );
    ok(&mut s, "CREATE VIEW dbo.v AS SELECT dbo.g(1) AS x");
    ok(
        &mut s,
        "ALTER FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x * 505 END",
    );
    ok(&mut s, "SELECT x INTO dbo.rv FROM dbo.v");
    assert_eq!(
        rows(&s, "SELECT CAST(x AS VARCHAR) FROM dbo.rv"),
        text(&[&[Some("1010")]])
    );
}

#[test]
fn arguments_are_evaluated_once_and_nesting_stays_linear() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    ok(
        &mut s,
        "CREATE FUNCTION dbo.sq(@x float) RETURNS float AS BEGIN RETURN @x * @x / @x END",
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.same(@x float) RETURNS float AS BEGIN DECLARE @y float = @x; RETURN @y - @y END",
    );
    let nested = (0..16).fold("2.0".to_string(), |inner, _| format!("dbo.sq({inner})"));
    let started = std::time::Instant::now();
    ok(
        &mut s,
        &format!("SELECT {nested} AS a, dbo.same(RAND()) AS b INTO dbo.r"),
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(
        rows(
            &s,
            "SELECT CAST(a AS VARCHAR), CAST(b AS VARCHAR) FROM dbo.r"
        ),
        text(&[&[Some("2.0"), Some("0.0")]])
    );
    ok(
        &mut s,
        "CREATE FUNCTION dbo.rt(@n int) RETURNS @r TABLE (i int) AS BEGIN INSERT @r VALUES (@n); INSERT @r SELECT i * 10 FROM @r; RETURN END",
    );
    fails(&mut s, "SELECT * FROM dbo.rt(2)", 40515);
}
