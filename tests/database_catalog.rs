use msduck::database_catalog::Database;
use msduck::server::{Server, serve_connection};
use msduck_core::diagnostic::SqlError;
use std::collections::HashMap;
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

fn database(name: &str, database_id: i32) -> Database {
    Database {
        name: name.into(),
        database_id,
    }
}

fn sql_error(error: anyhow::Error) -> (i32, String) {
    let error = error.downcast::<SqlError>().expect("SQL Server diagnostic");
    (error.number, error.message)
}

fn scalar<T: duckdb::types::FromSql>(db: &duckdb::Connection, sql: &str) -> T {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn master_is_listed_and_user_databases_are_attached_catalogs() {
    let server = Server::open(":memory:").unwrap();
    let a = server.connection().unwrap();
    let catalog = a.databases().clone();
    assert_eq!(catalog.list(&a).unwrap(), [database("master", 1)]);
    assert_eq!(catalog.current(&a).unwrap(), "master");

    assert_eq!(catalog.create(&a, "Sales").unwrap(), database("Sales", 5));
    assert_eq!(
        catalog.create(&a, "archive").unwrap(),
        database("archive", 6)
    );
    assert_eq!(
        catalog.list(&a).unwrap(),
        [
            database("master", 1),
            database("Sales", 5),
            database("archive", 6)
        ]
    );
    // Creating does not change the creator's current database.
    assert_eq!(catalog.current(&a).unwrap(), "master");

    // Attachments are instance-wide: a later connection can select the database.
    let b = server.connection().unwrap();
    assert_eq!(catalog.select(&b, "SALES").unwrap(), "Sales");
    assert_eq!(catalog.current(&b).unwrap(), "Sales");
    assert_eq!(scalar::<String>(&b, "SELECT current_schema()"), "dbo");
    // Engine DDL in the selected database updates that database's catalog.
    let mut session = msduck::engine::Session::new(b).unwrap();
    session.batch(
        "CREATE TABLE items(id INT PRIMARY KEY IDENTITY(1,1), name NVARCHAR(20)); \
         INSERT INTO items(name) VALUES (N'one')",
        &HashMap::new(),
        false,
    );
    let b = &session.db;
    assert_eq!(
        scalar::<String>(
            b,
            "SELECT s.name || '.' || t.name FROM sys.tables t JOIN sys.schemas s USING (schema_id)"
        ),
        "dbo.items"
    );
    assert_eq!(
        scalar::<i64>(
            b,
            "SELECT count(*) FROM sys.columns WHERE object_id = (SELECT object_id FROM sys.tables WHERE name = 'items')"
        ),
        2
    );
    assert_eq!(
        // IDENTITY sequences and defaults resolve inside the user database.
        scalar::<i64>(b, "SELECT count(*) FROM items WHERE id = 1"),
        1
    );
    assert_eq!(
        scalar::<i64>(&a, "SELECT count(*) FROM sys.tables WHERE name='items'"),
        0
    );
    // Each database lists every database and has its own catalog helpers.
    assert_eq!(
        scalar::<String>(
            b,
            "SELECT string_agg(name, ',' ORDER BY database_id) FROM sys.databases"
        ),
        "master,Sales,archive"
    );
    assert_eq!(scalar::<i32>(b, "SELECT __msduck_db_id('sales')"), 5);
    assert_eq!(scalar::<String>(b, "SELECT __msduck_db_name(1)"), "master");
    assert_eq!(
        scalar::<String>(b, "SELECT __msduck_current_db_name()"),
        "Sales"
    );
    assert_eq!(
        scalar::<String>(&a, "SELECT __msduck_current_db_name()"),
        "master"
    );

    assert_eq!(catalog.select(b, "master").unwrap(), "master");
    assert_eq!(scalar::<String>(b, "SELECT current_schema()"), "dbo");
    catalog.remove(&a, "archive").unwrap();
    assert_eq!(
        catalog.list(b).unwrap(),
        [database("master", 1), database("Sales", 5)]
    );
    // IDs are not reused after a drop.
    assert_eq!(
        catalog.create(&a, "archive").unwrap(),
        database("archive", 7)
    );
}

#[test]
fn invalid_database_operations_use_sql_server_diagnostics() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    catalog.create(&db, "app").unwrap();
    for name in ["APP", "master", "TempDB", "model", "msdb"] {
        assert_eq!(
            sql_error(catalog.create(&db, name).unwrap_err()),
            (
                1801,
                format!("Database '{name}' already exists. Choose a different database name.")
            )
        );
    }
    for name in ["memory", "system", "temp"] {
        assert!(catalog.create(&db, name).is_err());
    }
    assert!(catalog.create(&db, "").is_err());
    assert!(catalog.create(&db, &"x".repeat(129)).is_err());
    assert_eq!(
        sql_error(catalog.select(&db, "missing").unwrap_err()),
        (
            911,
            "Database 'missing' does not exist. Make sure that the name is entered correctly."
                .into()
        )
    );
    assert_eq!(
        sql_error(catalog.remove(&db, "missing").unwrap_err()).0,
        3701
    );
    assert_eq!(
        sql_error(catalog.remove(&db, "master").unwrap_err()).0,
        3708
    );
    assert_eq!(catalog.list(&db).unwrap().len(), 2);
}

#[test]
fn file_backed_databases_persist_across_restart() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-database-catalog-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    let file = directory.join("msduck.my%20app.duckdb");
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        let catalog = db.databases().clone();
        catalog.create(&db, "My App").unwrap();
        catalog.create(&db, "scratch").unwrap();
        assert!(file.exists());
        catalog.select(&db, "my app").unwrap();
        db.execute_batch("CREATE TABLE notes(id INT); INSERT INTO notes VALUES (42)")
            .unwrap();
        catalog.select(&db, "master").unwrap();
        catalog.remove(&db, "scratch").unwrap();
        assert!(!directory.join("msduck.scratch.duckdb").exists());
    }
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        let catalog = db.databases().clone();
        assert_eq!(
            catalog.list(&db).unwrap(),
            [database("master", 1), database("My App", 5)]
        );
        catalog.select(&db, "MY APP").unwrap();
        assert_eq!(scalar::<i32>(&db, "SELECT id FROM notes"), 42);
        assert_eq!(
            scalar::<String>(&db, "SELECT __msduck_current_db_name()"),
            "My App"
        );
        catalog.select(&db, "master").unwrap();
        catalog.remove(&db, "My App").unwrap();
        assert!(!file.exists());
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn an_existing_file_is_not_adopted() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-database-adopt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("msduck.stale.duckdb"), b"not a database").unwrap();
    let primary = directory.join("msduck.duckdb");
    let server = Server::open(primary.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    assert!(catalog.create(&db, "stale").is_err());
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    // The failed attempt leaves no registration behind.
    assert!(catalog.create(&db, "fresh").is_ok());
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

async fn connect(server: &Server) -> Client<Compat<TcpStream>> {
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

#[tokio::test]
async fn clients_query_sys_databases() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    db.databases().create(&db, "inventory").unwrap();
    let mut client = connect(&server).await;
    let rows = client
        .simple_query(
            "SELECT name, database_id, state_desc, compatibility_level, collation_name \
             FROM sys.databases WHERE name = N'inventory' OR database_id = 1 ORDER BY database_id",
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    let rows = rows
        .iter()
        .map(|row| {
            (
                row.get::<&str, _>(0).unwrap().to_owned(),
                row.get::<i32, _>(1).unwrap(),
                row.get::<&str, _>(2).unwrap().to_owned(),
                row.get::<u8, _>(3).unwrap(),
                row.get::<&str, _>(4).unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let expected = |name: &str, id| {
        (
            name.to_owned(),
            id,
            "ONLINE".to_owned(),
            160,
            "SQL_Latin1_General_CP1_CI_AS".to_owned(),
        )
    };
    assert_eq!(rows, [expected("master", 1), expected("inventory", 5)]);
}
