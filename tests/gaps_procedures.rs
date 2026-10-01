//! Stored procedures, EXEC (string) and sp_executesql (issue #719). Token
//! shapes, values, statuses and error numbers follow the SQL Server captures
//! in docs/gaps-procedures.md.
use msduck::engine::Session;
use msduck::server::{Server, serve_connection};
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

fn batch(session: &mut Session, sql: &str) -> (Vec<u8>, bool) {
    session.batch_response(sql, &Default::default(), false, None)
}

fn done(id: u8, status: u16, command: u16, count: u64) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(status.to_le_bytes());
    out.extend(command.to_le_bytes());
    out.extend(count.to_le_bytes());
    out
}

fn status(value: i32) -> Vec<u8> {
    let mut out = vec![0x79];
    out.extend(value.to_le_bytes());
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn module(session: &Session, name: &str) -> (i32, String, String, String) {
    session
        .db
        .query_row(
            "SELECT object_id, type_code, definition, properties FROM main.__msduck_modules WHERE name = ?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

#[test]
fn definitions_live_in_the_module_store_and_survive_restart() {
    let directory = std::env::temp_dir().join(format!("msduck-procedures-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    let definition = "-- leading comment\nCREATE PROCEDURE dbo.p_out @a int, @b nvarchar(10) = N'x', @c int = NULL OUTPUT\nAS\nSELECT @a AS a, @b AS b";
    let id = {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (tokens, ok) = batch(&mut session, definition);
        assert!(ok);
        assert_eq!(tokens, done(0xfd, 0, 222, 0));
        let (id, kind, stored, properties) = module(&session, "p_out");
        assert_eq!((kind.as_str(), stored.as_str()), ("P", definition));
        let properties: serde_json::Value = serde_json::from_str(&properties).unwrap();
        assert_eq!(
            properties,
            serde_json::json!({"parameters": [
                {"name": "@a", "type": "int", "output": false, "default": null},
                {"name": "@b", "type": "nvarchar(10)", "output": false, "default": "N'x'"},
                {"name": "@c", "type": "int", "output": true, "default": "NULL"},
            ]})
        );
        let (kind, description): (String, String) = session
            .db
            .query_row(
                "SELECT type, type_desc FROM sys.objects WHERE object_id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), description.as_str()),
            ("P ", "SQL_STORED_PROCEDURE")
        );
        // ALTER keeps the object id and replaces definition and parameters.
        let altered = "ALTER PROCEDURE p_out @a int AS SELECT @a * 2 AS a";
        assert!(batch(&mut session, altered).1);
        let (same, _, stored, properties) = module(&session, "p_out");
        assert_eq!((same, stored.as_str()), (id, altered));
        assert!(properties.contains("\"@a\"") && !properties.contains("\"@b\""));
        id
    };
    let server = Server::open(path).unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    assert_eq!(module(&session, "p_out").0, id);
    let (tokens, ok) = batch(&mut session, "EXEC p_out 21");
    assert!(ok);
    assert!(
        tokens.ends_with(&[done(0xff, 0x11, 193, 1), status(0), done(0xfe, 0, 224, 0)].concat())
    );
    // DROP removes the definition and its sys.objects row.
    assert!(batch(&mut session, "DROP PROCEDURE p_out").1);
    let remaining: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM sys.objects WHERE object_id = ?",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
    drop(session);
    drop(server);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn calls_report_return_status_and_done_tokens_like_sql_server() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE PROCEDURE foo AS SELECT 7 AS value;",
        "CREATE PROCEDURE p_ret AS RETURN",
        "CREATE PROCEDURE p_three AS RETURN 3",
        "CREATE PROCEDURE p_inner AS SELECT 1 AS one",
        "CREATE PROCEDURE p_outer AS BEGIN EXEC p_inner; SELECT 2 AS two END",
        "CREATE PROCEDURE p_rs AS BEGIN RAISERROR('boom', 16, 1); SELECT 'after' AS after END",
    ] {
        let (tokens, ok) = batch(&mut session, sql);
        assert!(ok, "{sql}");
        assert_eq!(tokens, done(0xfd, 0, 222, 0), "{sql}");
    }
    // Captured: DONEINPROC(0x11, SELECT, 1), RETURNSTATUS 0, DONEPROC.
    let (tokens, _) = batch(&mut session, "EXEC foo");
    assert!(
        tokens.ends_with(&[done(0xff, 0x11, 193, 1), status(0), done(0xfe, 0, 224, 0)].concat())
    );
    // More statements follow: DONEPROC carries DONE_MORE.
    let (tokens, _) = batch(&mut session, "EXEC foo; SELECT 1 AS after");
    assert!(contains(
        &tokens,
        &[status(0), done(0xfe, 1, 224, 0)].concat()
    ));
    // A bare RETURN completes with command 219; RETURN 3 like a SELECT.
    let (tokens, _) = batch(&mut session, "EXEC p_ret");
    assert_eq!(
        tokens,
        [done(0xff, 1, 219, 0), status(0), done(0xfe, 0, 224, 0)].concat()
    );
    let (tokens, _) = batch(&mut session, "EXEC p_three");
    assert_eq!(
        tokens,
        [done(0xff, 0x11, 193, 1), status(3), done(0xfe, 0, 224, 0)].concat()
    );
    // A nested call ends with DONEINPROC (224), without RETURNSTATUS.
    let (tokens, _) = batch(&mut session, "EXEC p_outer");
    assert!(contains(
        &tokens,
        &[done(0xff, 0x11, 193, 1), done(0xff, 1, 224, 0)].concat()
    ));
    // RAISERROR ends its statement (DONEINPROC with DONE_ERROR); severity 16
    // gives status -6.
    let (tokens, _) = batch(&mut session, "EXEC p_rs");
    assert!(contains(&tokens, &done(0xff, 3, 246, 0)));
    assert!(
        tokens.ends_with(&[done(0xff, 0x11, 193, 1), status(-6), done(0xfe, 0, 224, 0)].concat())
    );
    // Under NOCOUNT the nested call's DONEINPROC disappears and the count
    // bit is cleared (the count stays).
    let (tokens, _) = batch(&mut session, "SET NOCOUNT ON; EXEC p_outer");
    assert!(!contains(&tokens, &done(0xff, 1, 224, 0)));
    assert!(contains(&tokens, &done(0xff, 1, 193, 1)));
}

type SqlClient = Client<Compat<TcpStream>>;

async fn connect(server: &Server) -> SqlClient {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let db = server.connection().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve_connection(stream, db).unwrap();
    });
    let mut config = Config::new();
    config.host("127.0.0.1");
    config.port(address.port());
    config.authentication(AuthMethod::sql_server("sa", "development"));
    config.encryption(EncryptionLevel::NotSupported);
    let stream = TcpStream::connect(address).await.unwrap();
    Client::connect(config, stream.compat_write())
        .await
        .unwrap()
}

async fn ints(client: &mut SqlClient, sql: &str) -> Vec<Vec<Option<i32>>> {
    let results = client
        .simple_query(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_results()
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    results
        .iter()
        .flat_map(|rows| rows.iter())
        .map(|row| (0..row.len()).map(|i| row.get::<i32, _>(i)).collect())
        .collect()
}

async fn error(client: &mut SqlClient, sql: &str) -> u32 {
    let result = match client.simple_query(sql).await {
        Ok(stream) => stream.into_results().await.map(|_| ()),
        Err(error) => Err(error),
    };
    result
        .expect_err(sql)
        .code()
        .unwrap_or_else(|| panic!("{sql}: no server error"))
}

#[tokio::test]
async fn parameters_outputs_errors_and_dynamic_sql() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    for sql in [
        "CREATE PROCEDURE p_out @a int, @b int = 5, @c int OUTPUT AS BEGIN SET @c = @a + @b; RETURN @a * 10 END",
        "CREATE PROCEDURE p_inner @v int OUTPUT AS BEGIN SET @v = @v * 2; RETURN 5 END",
        "CREATE PROCEDURE p_outer AS BEGIN DECLARE @v int = 21, @r int; EXEC @r = p_inner @v OUTPUT; SELECT @v AS v, @r AS r END",
        "CREATE PROCEDURE p_rs AS BEGIN RAISERROR('boom', 16, 1); SELECT 1 AS after END",
        "CREATE PROCEDURE p_rec @n int AS BEGIN IF @n > 0 BEGIN DECLARE @m int = @n - 1; EXEC p_rec @m END ELSE SELECT @@NESTLEVEL AS lvl END",
    ] {
        client
            .simple_query(sql)
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap();
    }
    assert_eq!(
        ints(
            &mut client,
            "DECLARE @r int, @x int; EXEC @r = p_out 1, 2, @x OUTPUT; SELECT @r AS r, @x AS x"
        )
        .await,
        [[Some(10), Some(3)]]
    );
    assert_eq!(
        ints(
            &mut client,
            "DECLARE @r int, @x int; EXEC @r = p_out @c = @x OUT, @a = 4; SELECT @r AS r, @x AS x"
        )
        .await,
        [[Some(40), Some(9)]]
    );
    assert_eq!(
        ints(
            &mut client,
            "DECLARE @x int = 1; EXEC p_out 1, DEFAULT, @x; SELECT @x AS x"
        )
        .await,
        [[Some(1)]]
    );
    assert_eq!(
        ints(&mut client, "EXEC p_outer").await,
        [[Some(42), Some(5)]]
    );
    assert_eq!(ints(&mut client, "EXEC p_rec 5").await, [[Some(6)]]);
    assert_eq!(
        ints(&mut client, "BEGIN TRY EXEC p_rs; SELECT -1 AS never END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_SEVERITY() AS s END CATCH").await,
        [[Some(50000), Some(16)]]
    );
    assert_eq!(
        ints(&mut client, "DECLARE @o int; EXEC sp_executesql N'SET @b = @a * 3', N'@a int, @b int OUTPUT', @a = 4, @b = @o OUTPUT; SELECT @o AS o").await,
        [[Some(12)]]
    );
    assert_eq!(
        ints(
            &mut client,
            "EXEC sp_executesql N'SELECT @p + @q AS s', N'@p int, @q int', 1, 2"
        )
        .await,
        [[Some(3)]]
    );
    assert_eq!(
        ints(
            &mut client,
            "DECLARE @n int = 7; EXEC ('SELECT 7 AS seven')"
        )
        .await,
        [[Some(7)]]
    );
    for (sql, number) in [
        ("EXEC p_out 1", 201),
        ("EXEC p_out 1, 2, 3, 4", 8144),
        ("EXEC p_out @a = 1, @zz = 2, @c = 1", 8145),
        ("EXEC p_out @a = 1, @a = 2, @c = 1", 8143),
        ("DECLARE @x int; EXEC p_inner @v = @x, @x OUTPUT", 119),
        ("EXEC p_inner 5 OUTPUT", 179),
        ("DECLARE @x int; EXEC p_rec @x OUTPUT", 8162),
        ("EXEC p_rec 'x'", 8114),
        ("EXEC no_such_proc", 2812),
        ("EXEC p_rec 40", 217),
        ("SELECT 1; CREATE PROCEDURE p_late AS SELECT 1", 111),
        ("CREATE PROCEDURE p_rs AS SELECT 1", 2714),
        ("ALTER PROCEDURE p_none AS SELECT 1", 208),
        ("CREATE PROCEDURE p_bad AS SELECT @undeclared AS u", 137),
        ("CREATE PROCEDURE p_bad @a int AS DECLARE @a int", 134),
        ("CREATE PROCEDURE d.dbo.p_bad AS SELECT 1", 166),
        ("DROP PROCEDURE p_none", 3701),
        ("EXEC ('SELECT 1 AS one; RETURN 4')", 178),
        ("EXEC sp_executesql N'SELECT @p AS p', N'@p int'", 8178),
        ("EXEC sp_executesql 'SELECT 1 AS one'", 214),
        (
            "DECLARE @a int = 1; EXEC sp_executesql N'SELECT @a AS a'",
            137,
        ),
        ("EXEC p_rec 1 + 1", 102),
        ("DECLARE @t tinyint; EXEC p_out 300, 0, @t OUTPUT", 8114),
        ("EXEC sp_executesql N'BEGIN TRAN'", 266),
    ] {
        assert_eq!(error(&mut client, sql).await, number, "{sql}");
    }
    // The connection stays usable; the 266 call left its transaction open.
    assert_eq!(
        ints(&mut client, "SELECT @@TRANCOUNT AS tc; ROLLBACK").await,
        [[Some(1)]]
    );
    assert_eq!(
        ints(
            &mut client,
            "BEGIN TRY EXEC p_rec 40 END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n END CATCH"
        )
        .await,
        [[Some(217)]]
    );
    assert_eq!(
        ints(
            &mut client,
            "EXEC ('SELECT @@NESTLEVEL AS a'); EXEC sp_executesql N'SELECT @@NESTLEVEL AS b'"
        )
        .await,
        [[Some(1)], [Some(2)]]
    );
}
