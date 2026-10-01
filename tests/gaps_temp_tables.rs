//! #temp tables and table variables (gaps-temp-tables-v1): the generic
//! repros through a TDS client, session isolation, and cleanup of tables
//! left behind by a process that ended without closing its sessions.
//! docs/gaps-temp_tables.md describes the behavior.
use msduck::engine::Session;
use msduck::server::{Server, serve_connection};
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type SqlClient = Client<Compat<TcpStream>>;

async fn connect(server: &Server) -> SqlClient {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let db = server.connection().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = serve_connection(stream, db);
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

async fn ints(client: &mut SqlClient, sql: &str) -> Vec<Option<i32>> {
    client
        .simple_query(sql)
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<i32, _>(0))
        .collect()
}

async fn error(client: &mut SqlClient, sql: &str) -> (u32, u8, u8, String) {
    let result = match client.simple_query(sql).await {
        Ok(stream) => stream.into_results().await.map(|_| ()),
        Err(error) => Err(error),
    };
    match result {
        Err(tiberius::error::Error::Server(token)) => (
            token.code(),
            token.state(),
            token.class(),
            token.message().to_owned(),
        ),
        other => panic!("expected a server error for {sql}, got {other:?}"),
    }
}

#[tokio::test]
async fn generic_repros_hold_values_and_stay_private_to_their_session() {
    let server = Server::open(":memory:").unwrap();
    let mut a = connect(&server).await;
    let mut b = connect(&server).await;
    assert_eq!(
        ints(
            &mut a,
            "CREATE TABLE #foo(id int); INSERT INTO #foo VALUES (1), (2); SELECT id FROM #foo ORDER BY id"
        )
        .await,
        [Some(1), Some(2)]
    );
    // Another session neither sees nor collides with the table.
    assert_eq!(
        ints(&mut b, "SELECT OBJECT_ID('tempdb..#foo')").await,
        [None]
    );
    assert_eq!(
        error(&mut b, "SELECT id FROM #foo").await,
        (208, 0, 16, "Invalid object name '#foo'.".into())
    );
    assert_eq!(
        ints(
            &mut b,
            "CREATE TABLE #foo(id int); INSERT INTO #foo VALUES (3); SELECT id FROM #foo"
        )
        .await,
        [Some(3)]
    );
    // The table outlives its batch; OBJECT_ID resolves it through tempdb.
    assert_eq!(
        ints(&mut a, "SELECT id FROM tempdb..#foo ORDER BY id").await,
        [Some(1), Some(2)]
    );
    assert!(ints(&mut a, "SELECT OBJECT_ID('tempdb..#foo')").await[0].is_some());
    assert_eq!(
        error(&mut a, "CREATE TABLE #foo(id int)").await,
        (
            2714,
            6,
            16,
            "There is already an object named '#foo' in the database.".into()
        )
    );
    // Table variables: values, batch scope and per-session isolation.
    assert_eq!(
        ints(
            &mut a,
            "DECLARE @foo TABLE(id int); INSERT INTO @foo VALUES (1), (2); SELECT id FROM @foo ORDER BY id"
        )
        .await,
        [Some(1), Some(2)]
    );
    assert_eq!(
        ints(
            &mut b,
            "DECLARE @foo TABLE(id int); INSERT INTO @foo VALUES (9); SELECT id FROM @foo"
        )
        .await,
        [Some(9)]
    );
    assert_eq!(
        error(&mut a, "SELECT id FROM @foo").await,
        (
            1087,
            2,
            15,
            "Must declare the table variable \"@foo\".".into()
        )
    );
    // Table variable rows survive ROLLBACK; temporary table rows do not.
    assert_eq!(
        ints(
            &mut a,
            "DECLARE @t TABLE(id int); BEGIN TRANSACTION; INSERT INTO @t VALUES (5); INSERT INTO #foo VALUES (5); ROLLBACK; SELECT COUNT(*) FROM @t UNION ALL SELECT COUNT(*) FROM #foo"
        )
        .await,
        [Some(1), Some(2)]
    );
    assert_eq!(
        error(&mut a, "DROP TABLE #missing").await,
        (
            3701,
            5,
            11,
            "Cannot drop the table '#missing', because it does not exist or you do not have permission.".into()
        )
    );
    // Global tables are shared and dropped with their creator.
    a.simple_query("CREATE TABLE ##shared(id int); INSERT INTO ##shared VALUES (40)")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert_eq!(
        ints(
            &mut b,
            "INSERT INTO ##shared VALUES (41); SELECT id FROM ##shared ORDER BY id"
        )
        .await,
        [Some(40), Some(41)]
    );
    drop(a);
    let mut gone = false;
    for _ in 0..50 {
        if ints(&mut b, "SELECT OBJECT_ID('tempdb..##shared')").await[0].is_none() {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(gone, "global table outlived its creator");
    assert_eq!(error(&mut b, "SELECT id FROM ##shared").await.0, 208);
}

fn backends(session: &Session) -> i64 {
    session
        .db
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE table_name LIKE '\\_\\_msduck\\_temp\\_%' ESCAPE '\\' OR table_name LIKE '\\_\\_msduck\\_tv\\_%' ESCAPE '\\' OR table_name LIKE '\\_\\_msduck\\_global\\_%' ESCAPE '\\'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn orphaned_temporary_tables_are_dropped_when_the_database_reopens() {
    let directory = std::env::temp_dir().join(format!("msduck-temp-tables-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("primary.duckdb");
    let path = path.to_str().unwrap();
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (_, ok) = session.batch_response(
            "CREATE DATABASE temp_orphans",
            &Default::default(),
            false,
            None,
        );
        assert!(ok);
        session.use_database("temp_orphans").unwrap();
        let (_, ok) = session.batch_response(
            "CREATE TABLE #kept(id int IDENTITY(1,1) PRIMARY KEY, v int); CREATE TABLE ##kept(id int); INSERT INTO #kept(v) VALUES (1)",
            &Default::default(),
            false,
            None,
        );
        assert!(ok);
        assert_eq!(backends(&session), 2);
        // The process ends without closing the session.
        std::mem::forget(session);
    }
    let server = Server::open(path).unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    session.use_database("temp_orphans").unwrap();
    assert_eq!(backends(&session), 0);
    let tables: i64 = session
        .db
        .query_row("SELECT count(*) FROM sys.tables", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tables, 0);
    // The names are free again.
    let (_, ok) = session.batch_response(
        "CREATE TABLE ##kept(id int); CREATE TABLE #kept(id int IDENTITY(1,1)); INSERT INTO #kept DEFAULT VALUES",
        &Default::default(),
        false,
        None,
    );
    assert!(ok);
    drop(session);
    drop(server);
    let _ = std::fs::remove_dir_all(&directory);
}
