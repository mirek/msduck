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
    for name in ["main", "memory", "system", "temp"] {
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
    let file = directory.join("msduck.duckdb.5.my%20app.duckdb");
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
        assert!(!directory.join("msduck.duckdb.6.scratch.duckdb").exists());
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
    std::fs::write(
        directory.join("msduck.duckdb.5.stale.duckdb"),
        b"not a database",
    )
    .unwrap();
    let primary = directory.join("msduck.duckdb");
    let server = Server::open(primary.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    // The occupied name is skipped, and the file is left alone.
    assert_eq!(catalog.create(&db, "stale").unwrap(), database("stale", 6));
    assert_eq!(
        std::fs::read(directory.join("msduck.duckdb.5.stale.duckdb")).unwrap(),
        b"not a database"
    );
    assert!(directory.join("msduck.duckdb.6.stale.duckdb").exists());
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

#[test]
fn unavailable_registered_databases_are_not_recreated_and_can_be_dropped() {
    let directory = std::env::temp_dir().join(format!(
        "msduck-database-unavailable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    let lost = directory.join("msduck.duckdb.5.lost.duckdb");
    let corrupt = directory.join("msduck.duckdb.6.corrupt.duckdb");
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "lost").unwrap();
        db.databases().create(&db, "corrupt").unwrap();
    }
    std::fs::remove_file(&lost).unwrap();
    std::fs::write(&corrupt, b"not a database").unwrap();
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    // Missing data is not replaced by an empty database.
    assert!(!lost.exists());
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    assert_eq!(sql_error(catalog.select(&db, "lost").unwrap_err()).0, 911);
    // The registration still holds the name until it is dropped.
    assert_eq!(sql_error(catalog.create(&db, "lost").unwrap_err()).0, 1801);
    catalog.remove(&db, "lost").unwrap();
    catalog.remove(&db, "corrupt").unwrap();
    assert!(!corrupt.exists());
    assert_eq!(catalog.create(&db, "lost").unwrap(), database("lost", 7));
    assert_eq!(
        catalog.create(&db, "corrupt").unwrap(),
        database("corrupt", 8)
    );
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

fn scratch_directory(label: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "msduck-database-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn a_partially_recovered_database_is_detached() {
    let directory = scratch_directory("partial");
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "old").unwrap();
    }
    {
        // A conflicting object makes publication fail after the attach.
        let db = duckdb::Connection::open(directory.join("msduck.duckdb.5.old.duckdb")).unwrap();
        db.execute_batch("DROP VIEW sys.databases; CREATE TABLE sys.databases(x INT)")
            .unwrap();
    }
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    assert_eq!(sql_error(catalog.select(&db, "old").unwrap_err()).0, 911);
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT count(*) FROM duckdb_databases() WHERE database_name = 'old'"
        ),
        0
    );
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_failed_file_deletion_keeps_the_registration() {
    use std::os::unix::fs::PermissionsExt;
    let directory = scratch_directory("undeletable");
    let primary = directory.join("msduck.duckdb");
    let server = Server::open(primary.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    catalog.create(&db, "kept").unwrap();
    // Checkpoint the primary so no WAL write needs the directory meanwhile.
    db.execute_batch("CHECKPOINT").unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe = directory.join("probe");
    let enforced = std::fs::write(&probe, b"").is_err();
    let _ = std::fs::remove_file(&probe);
    let result = catalog.remove(&db, "kept");
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    if enforced {
        // Permissions do not apply to a privileged user.
        assert!(result.is_err());
        assert!(directory.join("msduck.duckdb.5.kept.duckdb").exists());
        // The failed DROP leaves the database attached and listed.
        assert_eq!(
            catalog.list(&db).unwrap(),
            [database("master", 1), database("kept", 5)]
        );
        catalog.select(&db, "kept").unwrap();
        catalog.select(&db, "master").unwrap();
        assert_eq!(sql_error(catalog.create(&db, "kept").unwrap_err()).0, 1801);
        catalog.remove(&db, "kept").unwrap();
    } else {
        result.unwrap();
    }
    assert!(!directory.join("msduck.duckdb.5.kept.duckdb").exists());
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn registered_file_names_cannot_leave_the_data_directory() {
    let directory = scratch_directory("escape");
    let outside = scratch_directory("escape-target");
    let victim = outside.join("victim.duckdb");
    duckdb::Connection::open(&victim).unwrap();
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "app").unwrap();
        db.databases().create(&db, "other").unwrap();
        // The registry is reachable through ordinary SQL.
        db.execute(
            "UPDATE main.__msduck_databases SET file = ? WHERE name_key = 'app'",
            [victim.to_str().unwrap()],
        )
        .unwrap();
        db.execute(
            "UPDATE main.__msduck_databases SET file = '../escape-target/victim.duckdb' WHERE name_key = 'other'",
            [],
        )
        .unwrap();
    }
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    assert!(catalog.remove(&db, "app").is_err());
    assert!(catalog.remove(&db, "other").is_err());
    assert!(victim.exists());
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn registered_file_names_are_bound_to_their_rows() {
    let directory = scratch_directory("rebind");
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    let keep = directory.join("msduck.duckdb.6.keep.duckdb");
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    catalog.create(&db, "a").unwrap();
    catalog.create(&db, "keep").unwrap();
    catalog.create(&db, "b").unwrap();
    catalog.create(&db, "c").unwrap();
    // Point rows at master's file and at another database's file.
    db.execute(
        "UPDATE main.__msduck_databases SET file = 'msduck.duckdb' WHERE name_key = 'a'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE main.__msduck_databases SET file = 'msduck.duckdb.6.keep.duckdb' WHERE name_key = 'b'",
        [],
    )
    .unwrap();
    assert!(catalog.remove(&db, "a").is_err());
    assert!(catalog.remove(&db, "b").is_err());
    assert!(directory.join("msduck.duckdb").exists());
    assert!(keep.exists());
    // Refused drops leave the databases attached.
    assert_eq!(catalog.list(&db).unwrap().len(), 5);
    catalog.remove(&db, "keep").unwrap();
    assert!(!keep.exists());
    drop((db, server));
    // A renamed primary keeps its databases: any stem is accepted.
    let renamed = directory.join("renamed.duckdb");
    std::fs::rename(directory.join("msduck.duckdb"), &renamed).unwrap();
    let _ = std::fs::rename(
        directory.join("msduck.duckdb.wal"),
        directory.join("renamed.duckdb.wal"),
    );
    let server = Server::open(renamed.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    assert_eq!(db.databases().select(&db, "c").unwrap(), "c");
    assert_eq!(
        sql_error(db.databases().select(&db, "a").unwrap_err()).0,
        911
    );
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn primaries_sharing_a_stem_keep_separate_database_files() {
    let directory = scratch_directory("shared-stem");
    let first = Server::open(directory.join("tenant.db").to_str().unwrap()).unwrap();
    let second = Server::open(directory.join("tenant.duckdb").to_str().unwrap()).unwrap();
    for server in [&first, &second] {
        let db = server.connection().unwrap();
        assert_eq!(
            db.databases().create(&db, "sales").unwrap(),
            database("sales", 5)
        );
    }
    assert!(directory.join("tenant.db.5.sales.duckdb").exists());
    assert!(directory.join("tenant.duckdb.5.sales.duckdb").exists());
    drop((first, second));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn concurrent_create_and_drop_leave_a_consistent_catalog() {
    let server = Server::open(":memory:").unwrap();
    let mut workers = vec![];
    for worker in 0..4 {
        let db = server.connection().unwrap();
        workers.push(std::thread::spawn(move || {
            for _ in 0..10 {
                let catalog = db.databases().clone();
                if worker % 2 == 0 {
                    let _ = catalog.create(&db, "race");
                } else {
                    let _ = catalog.remove(&db, "race");
                }
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    // The registry, the attachment and the file agree afterwards.
    let listed = catalog.list(&db).unwrap().iter().any(|d| d.name == "race");
    let attached: i64 = db
        .query_row(
            "SELECT count(*) FROM duckdb_databases() WHERE database_name = 'race'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(listed, attached == 1);
    if listed {
        catalog.remove(&db, "race").unwrap();
    }
    catalog.create(&db, "race").unwrap();
}

#[test]
fn readers_never_select_a_database_that_is_still_being_created() {
    let server = Server::open(":memory:").unwrap();
    let creator = server.connection().unwrap();
    let reader = server.connection().unwrap();
    let catalog = creator.databases().clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watching = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let catalog = reader.databases().clone();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if catalog.select(&reader, "fresh").is_ok() {
                    // Once selectable, the database is fully published.
                    let helper: i32 = reader
                        .query_row("SELECT __msduck_db_id('fresh')", [], |row| row.get(0))
                        .unwrap();
                    assert_eq!(helper, 5);
                    catalog.select(&reader, "master").unwrap();
                    return true;
                }
            }
            false
        })
    };
    catalog.create(&creator, "fresh").unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watching.join().unwrap();
}

#[cfg(unix)]
#[test]
fn registered_files_replaced_by_symbolic_links_are_not_opened() {
    let directory = scratch_directory("symlink");
    let outside = scratch_directory("symlink-target");
    let victim = outside.join("victim.duckdb");
    duckdb::Connection::open(&victim)
        .unwrap()
        .execute_batch("CREATE TABLE kept(x INT)")
        .unwrap();
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "app").unwrap();
    }
    let file = directory.join("msduck.duckdb.5.app.duckdb");
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&victim, &file).unwrap();
    // A dangling link must not become the file of a new database either.
    std::os::unix::fs::symlink(
        outside.join("created.duckdb"),
        directory.join("msduck.duckdb.6.next.duckdb"),
    )
    .unwrap();
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    // The link's name is skipped, never followed.
    assert_eq!(catalog.create(&db, "next").unwrap(), database("next", 7));
    assert!(!outside.join("created.duckdb").exists());
    let victim_db = duckdb::Connection::open(&victim).unwrap();
    let schemas: i64 = victim_db
        .query_row(
            "SELECT count(*) FROM duckdb_schemas() WHERE database_name='victim' AND schema_name IN ('dbo','sys')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schemas, 0, "the linked file was bootstrapped");
    drop(victim_db);
    // Dropping removes the link, not its target.
    catalog.remove(&db, "app").unwrap();
    assert!(victim.exists());
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn unpublished_databases_are_neither_listed_nor_selectable() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    catalog.create(&db, "pending").unwrap();
    // A registration whose publication has not finished stays hidden.
    db.execute_batch("UPDATE main.__msduck_databases SET published=false WHERE name_key='pending'")
        .unwrap();
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    assert_eq!(scalar::<i64>(&db, "SELECT count(*) FROM sys.databases"), 1);
    assert_eq!(
        sql_error(catalog.select(&db, "pending").unwrap_err()).0,
        911
    );
}

#[test]
fn database_names_compare_under_the_server_collation() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    catalog.create(&db, "İstanbul").unwrap();
    assert_eq!(catalog.select(&db, "istanbul").unwrap(), "İstanbul");
    // The SQL helpers use the same mapping as the catalog: the Kelvin sign
    // is not a case variant of k under this collation.
    catalog.create(&db, "\u{212A}").unwrap();
    let id = |name: &str| -> Option<i32> {
        db.query_row("SELECT __msduck_db_id(?)", [name], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(id("ISTANBUL"), Some(5));
    assert_eq!(id("istanbul"), Some(5));
    assert_eq!(id("MASTER"), Some(1));
    assert_eq!(id("\u{212A}"), Some(6));
    assert_eq!(id("k"), None);
    // Trailing spaces are not significant.
    assert_eq!(id("istanbul  "), Some(5));
    assert_eq!(catalog.select(&db, "İstanbul ").unwrap(), "İstanbul");
    assert_eq!(
        sql_error(catalog.create(&db, "istanbul ").unwrap_err()).0,
        1801
    );
    assert_eq!(
        sql_error(catalog.create(&db, "ISTANBUL").unwrap_err()).0,
        1801
    );
}

#[test]
fn a_leftover_wal_is_skipped_and_kept() {
    let directory = scratch_directory("stale-wal");
    let primary = directory.join("msduck.duckdb");
    let server = Server::open(primary.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    let wal = directory.join("msduck.duckdb.5.app.duckdb.wal");
    std::fs::write(&wal, b"stale").unwrap();
    assert_eq!(catalog.create(&db, "app").unwrap(), database("app", 6));
    assert_eq!(std::fs::read(&wal).unwrap(), b"stale");
    assert!(!directory.join("msduck.duckdb.5.app.duckdb").exists());
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn stale_registrations_without_files_are_forgotten() {
    let directory = scratch_directory("stale");
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "gone").unwrap();
        db.databases().create(&db, "later").unwrap();
    }
    // An interrupted DROP hid both databases and removed their files.
    for file in [
        "msduck.duckdb.5.gone.duckdb",
        "msduck.duckdb.6.later.duckdb",
    ] {
        std::fs::remove_file(directory.join(file)).unwrap();
    }
    {
        let db = duckdb::Connection::open(primary).unwrap();
        db.execute_batch(
            "UPDATE main.__msduck_databases SET published=false WHERE name_key='gone'",
        )
        .unwrap();
    }
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    // Startup forgets the hidden registration; a published one whose file
    // is lost stays registered.
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM main.__msduck_databases"),
        1
    );
    assert_eq!(catalog.create(&db, "gone").unwrap(), database("gone", 7));
    assert_eq!(sql_error(catalog.create(&db, "later").unwrap_err()).0, 1801);
    // CREATE forgets a stale registration found at run time too.
    db.execute_batch("UPDATE main.__msduck_databases SET published=false WHERE name_key='later'")
        .unwrap();
    assert_eq!(catalog.create(&db, "later").unwrap(), database("later", 8));
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn recovery_does_not_publish_hidden_databases() {
    let directory = scratch_directory("hidden");
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "failed").unwrap();
        // As left by a CREATE whose cleanup could not detach the catalog.
        db.execute_batch(
            "UPDATE main.__msduck_databases SET published=false WHERE name_key='failed'",
        )
        .unwrap();
    }
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    assert_eq!(sql_error(catalog.select(&db, "failed").unwrap_err()).0, 911);
    assert_eq!(
        sql_error(catalog.create(&db, "failed").unwrap_err()).0,
        1801
    );
    catalog.remove(&db, "failed").unwrap();
    assert!(!directory.join("msduck.duckdb.5.failed.duckdb").exists());
    assert_eq!(
        catalog.create(&db, "failed").unwrap(),
        database("failed", 6)
    );
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_linked_primary_names_databases_after_its_target() {
    let directory = scratch_directory("linked-primary");
    let target = directory.join("real.duckdb");
    duckdb::Connection::open(&target).unwrap();
    let link = directory.join("link.duckdb");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let server = Server::open(link.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    db.databases().create(&db, "sales").unwrap();
    assert!(directory.join("real.duckdb.5.sales.duckdb").exists());
    assert!(!directory.join("link.duckdb.5.sales.duckdb").exists());
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}

#[cfg(unix)]
#[test]
fn a_hidden_registration_with_a_linked_file_does_not_stop_startup() {
    let directory = scratch_directory("hidden-link");
    let primary = directory.join("msduck.duckdb");
    let primary = primary.to_str().unwrap();
    {
        let server = Server::open(primary).unwrap();
        let db = server.connection().unwrap();
        db.databases().create(&db, "odd").unwrap();
        db.execute_batch("UPDATE main.__msduck_databases SET published=false WHERE name_key='odd'")
            .unwrap();
    }
    let file = directory.join("msduck.duckdb.5.odd.duckdb");
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(directory.join("elsewhere"), &file).unwrap();
    let server = Server::open(primary).unwrap();
    let db = server.connection().unwrap();
    let catalog = db.databases().clone();
    assert_eq!(catalog.list(&db).unwrap(), [database("master", 1)]);
    assert_eq!(sql_error(catalog.create(&db, "odd").unwrap_err()).0, 1801);
    catalog.remove(&db, "odd").unwrap();
    assert!(std::fs::symlink_metadata(&file).is_err());
    drop((db, server));
    std::fs::remove_dir_all(&directory).unwrap();
}
