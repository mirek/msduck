//! The owner's "Sql wrapper" statements and session context state over TDS,
//! as batches and as sp_executesql RPCs. Expected values come from
//! reference/session-property-context.json.
use msduck::server::{Server, serve_connection};
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, ColumnType, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

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
async fn batch(client: &mut SqlClient, sql: &str) -> tiberius::Result<Vec<tiberius::Row>> {
    client.simple_query(sql).await?.into_first_result().await
}
fn error_code(result: tiberius::Result<Vec<tiberius::Row>>) -> Option<u32> {
    result.err().and_then(|error| error.code())
}

const OWNER_PROPERTIES: &str = "select
  cast(sessionproperty('ANSI_NULLS') as int) as ansiNulls,
  cast(sessionproperty('ANSI_PADDING') as int) as ansiPadding,
  cast(sessionproperty('ANSI_WARNINGS') as int) as ansiWarnings,
  cast(sessionproperty('ARITHABORT') as int) as arithabort,
  cast(sessionproperty('CONCAT_NULL_YIELDS_NULL') as int) as concatNullYieldsNull,
  cast(sessionproperty('QUOTED_IDENTIFIER') as int) as quotedIdentifier,
  cast(sessionproperty('NUMERIC_ROUNDABORT') as int) as numericRoundabort;";

#[tokio::test]
async fn owner_statements_match_sql_server_rows_and_types() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    // tiberius sends parameterized queries as sp_executesql RPCs, like
    // tedious execSql; simple_query sends batches.
    let rows = c
        .query("select 42", &[])
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(42));
    for rows in [
        c.query(OWNER_PROPERTIES, &[])
            .await
            .unwrap()
            .into_first_result()
            .await
            .unwrap(),
        batch(&mut c, OWNER_PROPERTIES).await.unwrap(),
    ] {
        let columns = rows[0].columns();
        assert_eq!(columns[0].name(), "ansiNulls");
        // tiberius reports IntN(4) as Int4; the tedious fixture replay pins
        // the exact nullable IntN descriptor.
        assert!(columns.iter().all(|c| c.column_type() == ColumnType::Int4));
        let values: Vec<_> = (0..7).map(|i| rows[0].get::<i32, _>(i)).collect();
        assert_eq!(
            values,
            [1, 1, 1, 1, 1, 1, 0].map(Some),
            "SET state at login"
        );
    }
    let set = "exec sys.sp_set_session_context @key = N'email', @value = null";
    c.execute(set, &[]).await.unwrap();
    batch(&mut c, set).await.unwrap();
    let rows = c
        .query("select @P1 as v", &[&42i32])
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>("v"), Some(42));
    assert_eq!(rows[0].columns()[0].column_type(), ColumnType::Int4);
}

#[tokio::test]
async fn sessionproperty_follows_the_session_set_state() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    let warnings = "SELECT CAST(SESSIONPROPERTY('ANSI_WARNINGS') AS INT) AS w";
    batch(&mut c, "SET ANSI_WARNINGS OFF").await.unwrap();
    assert_eq!(
        batch(&mut c, warnings).await.unwrap()[0].get::<i32, _>(0),
        Some(0)
    );
    batch(&mut c, "SET ANSI_WARNINGS ON").await.unwrap();
    assert_eq!(
        batch(&mut c, warnings).await.unwrap()[0].get::<i32, _>(0),
        Some(1)
    );
    // A SET inside sp_executesql is reverted when the call returns.
    c.execute("SET ANSI_WARNINGS OFF", &[]).await.unwrap();
    assert_eq!(
        batch(&mut c, warnings).await.unwrap()[0].get::<i32, _>(0),
        Some(1)
    );
    let rows = batch(
        &mut c,
        "DECLARE @n NVARCHAR(128) = N'arithabort'; SELECT CAST(SESSIONPROPERTY(@n) AS INT) AS a, CAST(SESSIONPROPERTY('NOPE') AS INT) AS u, CAST(SESSIONPROPERTY(NULL) AS INT) AS n",
    )
    .await
    .unwrap();
    assert_eq!(
        (0..3).map(|i| rows[0].get::<i32, _>(i)).collect::<Vec<_>>(),
        [Some(1), None, None]
    );
    assert_eq!(
        error_code(batch(&mut c, "SELECT SESSIONPROPERTY()").await),
        Some(174)
    );
}

#[tokio::test]
async fn session_context_is_typed_session_state() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    let mut other = connect(&server).await;
    batch(
        &mut c,
        "EXEC sp_set_session_context N'tenant', 42; exec sys.sp_set_session_context @key = N'email', @value = N'owner@example.com', @read_only = 1",
    )
    .await
    .unwrap();
    let rows = batch(
        &mut c,
        "SELECT CAST(SESSION_CONTEXT(N'Tenant') AS INT) AS tenant, CAST(SESSION_CONTEXT(N'email') AS NVARCHAR(100)) AS email, CAST(SESSION_CONTEXT(N'EMAIL') AS NVARCHAR(100)) AS other_key",
    )
    .await
    .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(42));
    assert_eq!(rows[0].get::<&str, _>(1), Some("owner@example.com"));
    assert_eq!(rows[0].get::<&str, _>(2), None);
    // Values survive batches and are bound, not spliced into SQL text.
    c.execute(
        "exec sys.sp_set_session_context @key = N'quote', @value = @P1",
        &[&"O'Brien'); DROP TABLE t; --"],
    )
    .await
    .unwrap();
    let rows = batch(
        &mut c,
        "SELECT CAST(SESSION_CONTEXT(N'quote') AS NVARCHAR(50)) AS q",
    )
    .await
    .unwrap();
    assert_eq!(
        rows[0].get::<&str, _>(0),
        Some("O'Brien'); DROP TABLE t; --")
    );
    // read_only: 15664, the batch continues and @@ERROR reports it.
    let rows = batch(
        &mut c,
        "EXEC sys.sp_set_session_context N'email', N'x'; SELECT @@ERROR AS e, CAST(SESSION_CONTEXT(N'email') AS NVARCHAR(100)) AS email",
    )
    .await;
    assert_eq!(error_code(rows), Some(15664));
    let rows = batch(
        &mut c,
        "SELECT CAST(SESSION_CONTEXT(N'email') AS NVARCHAR(100)) AS email",
    )
    .await
    .unwrap();
    assert_eq!(rows[0].get::<&str, _>(0), Some("owner@example.com"));
    // Other connections have their own store.
    let rows = batch(
        &mut other,
        "SELECT CAST(SESSION_CONTEXT(N'tenant') AS INT) AS t",
    )
    .await
    .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), None);
    // Captured argument diagnostics.
    for (sql, code) in [
        ("SELECT SESSION_CONTEXT('email')", 8116),
        ("SELECT SESSION_CONTEXT(NULL)", 8116),
        ("SELECT SESSION_CONTEXT()", 174),
        ("EXEC sys.sp_set_session_context NULL, 1", 225),
        ("EXEC sys.sp_set_session_context N'only'", 16903),
        ("EXEC sys.sp_set_session_context N'expr', 1+1", 102),
    ] {
        assert_eq!(error_code(batch(&mut c, sql).await), Some(code), "{sql}");
    }
    // A bare nvarchar sql_variant is refused explicitly, never fabricated.
    assert!(
        batch(&mut c, "SELECT SESSION_CONTEXT(N'email') AS v")
            .await
            .is_err()
    );
    assert!(
        batch(
            &mut c,
            "CREATE VIEW ctx AS SELECT SESSION_CONTEXT(N'tenant') AS t"
        )
        .await
        .is_err()
    );
}
