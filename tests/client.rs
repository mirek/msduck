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
async fn login_batches_types_and_empty_metadata() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let results=client.simple_query("SELECT 42 AS answer, N'hello 🦆' AS message, CAST(NULL AS INT) AS absent; SELECT CAST(1 AS BIGINT) AS big").await.unwrap().into_results().await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0][0].get::<i32, _>(0), Some(42));
    assert_eq!(results[0][0].get::<&str, _>(1), Some("hello 🦆"));
    assert_eq!(results[0][0].get::<i32, _>(2), None);
    assert_eq!(results[1][0].get::<i64, _>(0), Some(1));
    let mut stream = client
        .simple_query("SELECT CAST(NULL AS INT) AS absent WHERE 1=0")
        .await
        .unwrap();
    let columns = stream.columns().await.unwrap().unwrap();
    assert_eq!(columns[0].name(), "absent");
    assert_eq!(columns[0].column_type(), tiberius::ColumnType::Int4);
    assert!(stream.into_results().await.unwrap()[0].is_empty());
}
#[tokio::test]
async fn persistence_dml_top_and_sessions() {
    let server = Server::open(":memory:").unwrap();
    let mut a = connect(&server).await;
    let mut b = connect(&server).await;
    a.simple_query("CREATE TABLE [dbo].[items] ([id] INT PRIMARY KEY, [name] NVARCHAR(50)); INSERT INTO dbo.items VALUES (1,N'one'),(2,N'two')").await.unwrap().into_results().await.unwrap();
    let rows = b
        .simple_query("SELECT TOP (1) [id], [name] FROM [dbo].[items] ORDER BY id DESC")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(2));
    a.simple_query("BEGIN TRANSACTION; INSERT INTO dbo.items VALUES (3,N'three'); ROLLBACK")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let rows = b
        .simple_query("SELECT COUNT(*) AS n FROM dbo.items")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(2));
}
#[tokio::test]
async fn rpc_binds_values_and_preserves_connection_after_errors() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    let attack = "'; DROP TABLE dbo.items; -- 🦆";
    let rows = c
        .query(
            "SELECT @P1 AS n, @P2 AS text, @P3 AS flag",
            &[&123i32, &attack, &true],
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(123));
    assert_eq!(rows[0].get::<&str, _>(1), Some(attack));
    assert_eq!(rows[0].get::<bool, _>(2), Some(true));
    let error = match c.simple_query("SELECT * FROM nonexistent_table").await {
        Err(error) => error,
        Ok(stream) => stream.into_results().await.unwrap_err(),
    };
    assert_eq!(error.code(), Some(208));
    let rows = c
        .simple_query("SELECT 7 AS alive")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(7));
}
#[tokio::test]
async fn long_unicode_and_binary_plp_roundtrip() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    let large = "a🦆'".repeat(6000);
    let blob = vec![0xab; 12000];
    let rows = c
        .query("SELECT @P1 AS text, @P2 AS bytes", &[&large, &blob])
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<&str, _>(0), Some(large.as_str()));
    assert_eq!(rows[0].get::<&[u8], _>(1), Some(blob.as_slice()));
}

#[tokio::test]
async fn null_rpc_parameters_keep_declared_types() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let text: Option<&str> = None;
    let number: Option<i64> = None;
    let mut stream = client
        .query("SELECT @P1 AS text, @P2 AS number", &[&text, &number])
        .await
        .unwrap();
    let columns = stream.columns().await.unwrap().unwrap();
    assert_eq!(columns[0].column_type(), tiberius::ColumnType::NVarchar);
    assert_eq!(columns[1].column_type(), tiberius::ColumnType::Int8);
    let rows = stream.into_first_result().await.unwrap();
    assert_eq!(rows[0].get::<&str, _>(0), None);
    assert_eq!(rows[0].get::<i64, _>(1), None);
}

#[tokio::test]
async fn null_ordering_and_correlated_apply() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    for (direction, expected) in [
        ("ASC", vec![None, Some(1), Some(2)]),
        ("DESC", vec![Some(2), Some(1), None]),
    ] {
        let rows=client.simple_query(format!("SELECT value FROM (VALUES (1),(NULL),(2)) AS source(value) ORDER BY value {direction}")).await.unwrap().into_first_result().await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.get::<i32, _>(0)).collect::<Vec<_>>(),
            expected
        );
    }
    let rows=client.simple_query("SELECT p.id, q.n FROM (VALUES (1),(2)) AS p(id) CROSS APPLY (SELECT p.id + 1 AS n WHERE p.id=1) AS q").await.unwrap().into_first_result().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>(1), Some(2));
    let rows=client.simple_query("SELECT p.id, q.n FROM (VALUES (1),(2)) AS p(id) OUTER APPLY (SELECT p.id + 1 AS n WHERE p.id=1) AS q ORDER BY p.id").await.unwrap().into_first_result().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i32, _>(1), Some(2));
    assert_eq!(rows[1].get::<i32, _>(1), None);
}

#[test]
fn file_storage_survives_restart() {
    use std::{
        collections::HashMap,
        time::{SystemTime, UNIX_EPOCH},
    };
    let path = std::env::temp_dir().join(format!(
        "msduck-test-{}-{}.duckdb",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = msduck::engine::Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response("CREATE SCHEMA storage", &HashMap::new(), false, None)
                .1
        );
        session.batch(
            "CREATE TABLE storage.persisted (id INT); INSERT INTO storage.persisted VALUES (42)",
            &HashMap::new(),
            false,
        );
        assert!(session.batch_response(
            "SELECT CAST(id AS SMALLINT) AS id INTO storage.persisted_into FROM storage.persisted",
            &HashMap::new(), false, None).1);
        assert!(
            session
                .batch_response(
                    "ALTER TABLE storage.persisted ADD extra INT NULL DEFAULT 7",
                    &HashMap::new(),
                    false,
                    None
                )
                .1
        );
        assert!(session.batch_response("CREATE VIEW storage.aggregate_view AS SELECT SUM(id) AS total,AVG(id) AS mean FROM storage.persisted", &HashMap::new(), false, None).1);
        assert!(session.batch_response(
            "CREATE VIEW storage.persisted_view AS SELECT TOP (1) [id] FROM storage.persisted ORDER BY id",
            &HashMap::new(), false, None).1);
        assert!(session.batch_response(
            "ALTER VIEW storage.persisted_view AS SELECT TOP (1) [id] + 1 AS id FROM storage.persisted ORDER BY id",
            &HashMap::new(), false, None).1);
        assert!(
            session
                .batch_response(
                    "CREATE TABLE storage.date_defaults (d DATE DEFAULT DATEFROMPARTS(2024,2,29), n INT DEFAULT LEN(N'🦆  '), i INT DEFAULT CAST(10.9 AS INT), e DATE DEFAULT EOMONTH('2024-01-31',1), y INT DEFAULT YEAR('2024-02-29'))",
                    &HashMap::new(),
                    false,
                    None
                )
                .1
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT id FROM storage.persisted_view", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            43
        );
        assert!(session.batch_response(
            "CREATE TABLE storage.altered_integer (n DECIMAL(38,1), f FLOAT DEFAULT 16777217); INSERT INTO storage.altered_integer (n) VALUES (9223372036854775807.9); ALTER TABLE storage.altered_integer ALTER COLUMN n BIGINT NOT NULL",
            &HashMap::new(), false, None).1);
    }
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let db = server.connection().unwrap();
        let converted: i64 = db
            .query_row("SELECT n FROM storage.altered_integer", [], |r| r.get(0))
            .unwrap();
        assert_eq!(converted, i64::MAX);
        let wide: f64 = db
            .query_row("SELECT f FROM storage.altered_integer", [], |r| r.get(0))
            .unwrap();
        assert_eq!(wide, 16777217.0);
        db.execute("INSERT INTO storage.date_defaults DEFAULT VALUES", [])
            .unwrap();
        let month_end: String = db
            .query_row(
                "SELECT CAST(e AS VARCHAR) FROM storage.date_defaults",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(month_end, "2024-02-29");
        let year: i32 = db
            .query_row("SELECT y FROM storage.date_defaults", [], |r| r.get(0))
            .unwrap();
        assert_eq!(year, 2024);
        let aggregates: (i32, i32) = db
            .query_row("SELECT total, mean FROM storage.aggregate_view", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(aggregates, (42, 42));
        let date: String = db
            .query_row(
                "SELECT CAST(d AS VARCHAR) FROM storage.date_defaults",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(date, "2024-02-29");
        let length: i32 = db
            .query_row("SELECT n FROM storage.date_defaults", [], |r| r.get(0))
            .unwrap();
        assert_eq!(length, 2);
        let integer: i32 = db
            .query_row("SELECT i FROM storage.date_defaults", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integer, 10);
        let old: Option<i32> = db
            .query_row("SELECT extra FROM storage.persisted", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old, None);
        db.execute("INSERT INTO storage.persisted (id) VALUES (99)", [])
            .unwrap();
        let new: i32 = db
            .query_row("SELECT extra FROM storage.persisted WHERE id=99", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(new, 7);
        let copied: i16 = db
            .query_row("SELECT id FROM storage.persisted_into", [], |r| r.get(0))
            .unwrap();
        assert_eq!(copied, 42);

        assert_eq!(
            server
                .connection()
                .unwrap()
                .query_row("SELECT id FROM storage.persisted_view", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            43
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn exact_decimal_rpc_roundtrips_beyond_float_precision() {
    use tiberius::numeric::Numeric;
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    for (scaled, scale) in [
        (12345i128, 2),
        (-12345, 2),
        (999999999, 0),
        (9999999999999999999, 0),
        (-9999999999999999999999999999, 9),
        (99999999999999999999999999999999999999, 18),
    ] {
        let value = Numeric::new_with_scale(scaled, scale);
        let rows = client
            .query("SELECT @P1 AS exact", &[&value])
            .await
            .unwrap()
            .into_first_result()
            .await
            .unwrap();
        let returned = rows[0].get::<Numeric, _>(0).unwrap();
        assert_eq!(returned.value(), scaled);
        assert_eq!(returned.scale(), scale);
    }
}

#[tokio::test]
async fn sql_batch_local_variables_are_typed_and_isolated() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client.simple_query("DECLARE @a INT = 3; SET @a = @a + 4; DECLARE @b TINYINT = 255; SELECT @a AS a, @b AS b;").await.unwrap().into_first_result().await.unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(7));
    assert_eq!(rows[0].get::<u8, _>(1), Some(255));
    match client.simple_query("SELECT @a").await {
        Ok(stream) => assert!(stream.into_results().await.is_err()),
        Err(error) => assert!(
            error
                .to_string()
                .contains("Must declare the scalar variable")
        ),
    }
    let rows = client
        .simple_query("DECLARE @a INT = 9; SELECT @a;")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(9));
}

#[tokio::test]
async fn conditional_sql_batches_finish_when_final_branch_is_skipped() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client
        .simple_query("BEGIN SELECT 1; END IF 1=0 SELECT 2;")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].get::<i32, _>(0), Some(1));
    let rows = client
        .simple_query("IF 1=0 SELECT 2;")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert!(rows.iter().all(Vec::is_empty));
    let rows = client
        .simple_query("SELECT @@TRANCOUNT;")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(0));
}

#[tokio::test]
async fn loops_end_sql_batches_with_final_completion() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let results = client
        .simple_query("DECLARE @i INT=0; WHILE @i<2 BEGIN SET @i=@i+1; SELECT @i; END")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0][0].get::<i32, _>(0), Some(1));
    assert_eq!(results[1][0].get::<i32, _>(0), Some(2));
    assert!(
        client
            .simple_query("WHILE 1=0 SELECT 1;")
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap()
            .iter()
            .all(Vec::is_empty)
    );
}

#[tokio::test]
async fn bare_return_finishes_sql_batch_and_skips_remaining_results() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client
        .simple_query("SELECT 1; BEGIN RETURN END SELECT 2;")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].get::<i32, _>(0), Some(1));
    assert!(
        client
            .simple_query("RETURN;")
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap()
            .iter()
            .all(Vec::is_empty)
    );
    let rows = client
        .simple_query("SELECT 3;")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(3));
}

#[tokio::test]
async fn caught_errors_finish_sql_batches_without_leaking_error_tokens() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client
        .simple_query("BEGIN TRY SELECT 1; THROW 51001, N'handled', 9; END TRY BEGIN CATCH SELECT ERROR_NUMBER(); END CATCH SELECT 3;")
        .await.unwrap().into_results().await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0].get::<i32, _>(0), Some(1));
    assert_eq!(rows[1][0].get::<i32, _>(0), Some(51001));
    assert_eq!(rows[2][0].get::<i32, _>(0), Some(3));
    let rows = client.simple_query("CREATE TABLE dbo.recover_catch (id INT PRIMARY KEY); INSERT INTO dbo.recover_catch VALUES (1); BEGIN TRAN; BEGIN TRY INSERT INTO dbo.recover_catch VALUES (1); END TRY BEGIN CATCH ROLLBACK; SELECT ERROR_NUMBER(), @@TRANCOUNT; END CATCH;")
        .await.unwrap().into_first_result().await.unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(2627));
    assert_eq!(rows[0].get::<i32, _>(1), Some(0));
}

#[tokio::test]
async fn guid_rpc_and_sql_batch_keep_native_uuid_values() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let id = tiberius::Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
    let null: Option<tiberius::Uuid> = None;
    let rows = client
        .query("SELECT @P1, @P2", &[&id, &null])
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<tiberius::Uuid, _>(0), Some(id));
    assert_eq!(rows[0].get::<tiberius::Uuid, _>(1), None);
    let rows = client
        .simple_query("SELECT CAST('00112233-4455-6677-8899-aabbccddeeff' AS UNIQUEIDENTIFIER)")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<tiberius::Uuid, _>(0), Some(id));
}

#[test]
fn newid_default_stays_volatile_after_database_reopen() {
    use std::collections::HashMap;
    let path = std::env::temp_dir().join(format!(
        "msduck-newid-{}-{}.duckdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let first;
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = msduck::engine::Session::new(server.connection().unwrap()).unwrap();
        let (_, success) = session.batch_response("CREATE TABLE dbo.ids (id UNIQUEIDENTIFIER DEFAULT NEWID()); INSERT INTO dbo.ids DEFAULT VALUES", &HashMap::new(), false, None);
        assert!(success);
        first = session
            .db
            .query_row("SELECT CAST(id AS VARCHAR) FROM dbo.ids", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        uuid::Uuid::parse_str(&first).unwrap();
    }
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = msduck::engine::Session::new(server.connection().unwrap()).unwrap();
        let (_, success) = session.batch_response(
            "INSERT INTO dbo.ids DEFAULT VALUES",
            &HashMap::new(),
            false,
            None,
        );
        assert!(success);
        let counts: (i64, i64) = session
            .db
            .query_row(
                "SELECT COUNT(*), COUNT(DISTINCT id) FROM dbo.ids",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (2, 2));
        let preserved: i64 = session
            .db
            .query_row(
                "SELECT COUNT(*) FROM dbo.ids WHERE CAST(id AS VARCHAR)=?",
                [&first],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(preserved, 1);
    }
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn print_only_sql_batches_finish_and_preserve_connection() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let results = client
        .simple_query("PRINT N'hello'; PRINT NULL; PRINT '';")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert!(results.iter().all(Vec::is_empty));
    let rows = client
        .simple_query("SELECT 7 AS n; PRINT N'done';")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(7));
}

#[tokio::test]
async fn len_sql_batches_return_int_and_max_bigint_metadata() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client.simple_query("SELECT LEN(N'🦆  '), LEN(CAST(N'hello ' AS NVARCHAR(MAX))), LEN(CAST(NULL AS NVARCHAR(MAX)))")
        .await.unwrap().into_first_result().await.unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(2));
    assert_eq!(rows[0].get::<i64, _>(1), Some(5));
    assert_eq!(rows[0].get::<i64, _>(2), None);
}

#[tokio::test]
async fn trim_sql_batch_preserves_nonbreaking_spaces() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client
        .simple_query("SELECT TRIM(N'  x  '), LTRIM(N'  x'), RTRIM(N'x  '), TRIM(N'  ')")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<&str, _>(0), Some(" x "));
    assert_eq!(rows[0].get::<&str, _>(1), Some(" x"));
    assert_eq!(rows[0].get::<&str, _>(2), Some("x "));
    assert_eq!(rows[0].get::<&str, _>(3), Some(""));
}

#[tokio::test]
async fn datefromparts_sql_batch_has_date_metadata() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let mut stream = client
        .simple_query("SELECT DATEFROMPARTS(2024,2,29) AS d, DATEFROMPARTS(NULL,1,1) AS absent")
        .await
        .unwrap();
    let columns = stream.columns().await.unwrap().unwrap();
    assert_eq!(columns[0].column_type(), tiberius::ColumnType::Daten);
    assert_eq!(columns[1].column_type(), tiberius::ColumnType::Daten);
    assert_eq!(stream.into_first_result().await.unwrap().len(), 1);
}

#[tokio::test]
async fn integer_overflow_error_and_try_cast_recover() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let error = match client
        .simple_query("SELECT CAST(2147483648.9 AS INT)")
        .await
    {
        Err(error) => error,
        Ok(stream) => stream.into_results().await.unwrap_err(),
    };
    assert_eq!(error.code(), Some(8115));
    let rows = client
        .simple_query("SELECT TRY_CAST(2147483648.9 AS INT), CAST(2147483647.9 AS INT)")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), None);
    assert_eq!(rows[0].get::<i32, _>(1), Some(i32::MAX));
}

#[tokio::test]
async fn float_precision_preserves_wire_widths() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let rows = client
        .simple_query(
            "SELECT CAST(16777217 AS FLOAT), CAST(16777217 AS FLOAT(24)), CAST(NULL AS FLOAT(25))",
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<f64, _>(0), Some(16777217.0));
    assert_eq!(rows[0].get::<f32, _>(1), Some(16777216.0));
    assert_eq!(rows[0].get::<f64, _>(2), None);
}

#[tokio::test]
async fn cte_dml_converts_integer_values_without_a_result_set() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let results = client.simple_query("CREATE TABLE dbo.cte_insert (n INT, d INT DEFAULT 11.9); WITH s AS (SELECT 10.9 AS n) INSERT INTO dbo.cte_insert (n) SELECT n FROM s; SELECT n,d FROM dbo.cte_insert")
        .await.unwrap().into_results().await.unwrap();
    let rows = results.iter().flatten().collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>(0), Some(10));
    assert_eq!(rows[0].get::<i32, _>(1), Some(11));
    let results = client.simple_query("WITH s AS (SELECT -12.9 AS n) UPDATE t SET n=s.n FROM dbo.cte_insert AS t CROSS JOIN s; SELECT n,d FROM dbo.cte_insert")
        .await.unwrap().into_results().await.unwrap();
    let rows = results.iter().flatten().collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>(0), Some(-12));
    assert_eq!(rows[0].get::<i32, _>(1), Some(11));
    let rows = client
        .simple_query("UPDATE dbo.cte_insert SET n *= 2+3; SELECT n FROM dbo.cte_insert")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(-60));
}

#[tokio::test]
async fn cte_delete_reports_affected_rows_without_result_metadata() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let results = client.simple_query("CREATE TABLE dbo.delete_counts (id INT); INSERT INTO dbo.delete_counts VALUES (1),(2),(3); WITH ids AS (SELECT 2 AS id UNION ALL SELECT 3) DELETE t FROM dbo.delete_counts t JOIN ids ON t.id=ids.id; SELECT @@ROWCOUNT, id FROM dbo.delete_counts")
        .await.unwrap().into_results().await.unwrap();
    let rows = results.iter().flatten().collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>(0), Some(2));
    assert_eq!(rows[0].get::<i32, _>(1), Some(1));
}

#[tokio::test]
async fn compound_select_assignments_emit_only_explicit_results() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let results = client.simple_query("DECLARE @n INT=7,@s NVARCHAR(20)=N'a'; SELECT @n*=2+3,@s+=N'b'; SELECT @n,@s; SELECT @n+=1 WHERE 1=0; SELECT @n,@@ROWCOUNT")
        .await.unwrap().into_results().await.unwrap();
    let rows = results.iter().flatten().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i32, _>(0), Some(35));
    assert_eq!(rows[0].get::<&str, _>(1), Some("ab"));
    assert_eq!(rows[1].get::<i32, _>(0), Some(35));
    assert_eq!(rows[1].get::<i32, _>(1), Some(0));
}

#[tokio::test]
async fn money_results_decode_with_currency_metadata_and_nulls() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let mut stream = client.simple_query("SELECT CAST(429496.7296 AS MONEY) AS m,CAST(-214748.3648 AS SMALLMONEY) AS s,CAST(NULL AS MONEY) AS absent").await.unwrap();
    let columns = stream.columns().await.unwrap().unwrap();
    assert!(
        columns
            .iter()
            .all(|c| c.column_type() == tiberius::ColumnType::Money)
    );
    let rows = stream.into_first_result().await.unwrap();
    assert_eq!(rows[0].get::<f64, _>(0), Some(429496.7296));
    assert_eq!(rows[0].get::<f64, _>(1), Some(-214748.3648));
    assert_eq!(rows[0].get::<f64, _>(2), None);
    let mut empty = client
        .simple_query("SELECT CAST(NULL AS SMALLMONEY) AS s WHERE 1=0")
        .await
        .unwrap();
    assert_eq!(
        empty.columns().await.unwrap().unwrap()[0].column_type(),
        tiberius::ColumnType::Money
    );
    assert!(empty.into_first_result().await.unwrap().is_empty());
}
