//! FOR JSON AUTO, JSON_MODIFY, ordered STRING_AGG, STRING_SPLIT and
//! HASHBYTES (issue #724), through a TDS client against SQL Server's
//! captured behavior in reference/gaps-json_string.json and the earlier
//! first-party fixtures (json-constructors, string-split, string-agg,
//! hashbytes-checksum).
use msduck::server::{Server, serve_connection};
use serde_json::{Value, json};
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, error::Error};
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

fn cell(data: &ColumnData<'_>) -> Value {
    match data {
        ColumnData::U8(v) => json!(v),
        ColumnData::I16(v) => json!(v),
        ColumnData::I32(v) => json!(v),
        // Fixtures hold BIGINT as tedious strings.
        ColumnData::I64(v) => json!(v.map(|v| v.to_string())),
        ColumnData::Bit(v) => json!(v),
        ColumnData::String(v) => json!(v.as_deref()),
        ColumnData::Binary(v) => json!(v.as_deref().map(|bytes| format!(
            "0x{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ))),
        other => panic!("unexpected cell {other:?}"),
    }
}

/// Rows of every result set, or the first error's (number, state).
async fn run(client: &mut SqlClient, sql: &str) -> Result<Vec<Vec<Vec<Value>>>, (u32, u8)> {
    let results = match client.simple_query(sql).await {
        Ok(stream) => stream.into_results().await,
        Err(error) => Err(error),
    };
    match results {
        Ok(sets) => Ok(sets
            .into_iter()
            .map(|rows| {
                rows.into_iter()
                    .map(|row| row.cells().map(|(_, data)| cell(data)).collect())
                    .collect()
            })
            .collect()),
        Err(Error::Server(error)) => Err((error.code(), error.state())),
        Err(error) => panic!("{sql}: {error}"),
    }
}

async fn rows(client: &mut SqlClient, sql: &str) -> Vec<Vec<Value>> {
    run(client, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
        .pop()
        .unwrap_or_default()
}

async fn scalar(client: &mut SqlClient, sql: &str) -> Value {
    rows(client, sql).await[0][0].clone()
}

async fn error(client: &mut SqlClient, sql: &str) -> (u32, u8) {
    run(client, sql).await.expect_err(sql)
}

async fn column_types(client: &mut SqlClient, sql: &str) -> Vec<ColumnType> {
    let mut stream = client.simple_query(sql).await.unwrap();
    let columns = stream.columns().await.unwrap().unwrap().to_vec();
    stream.into_results().await.unwrap();
    columns.iter().map(|c| c.column_type()).collect()
}

async fn setup() -> (Server, SqlClient) {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    for sql in [
        // The setup of scripts/capture-gaps-json_string.mjs.
        "CREATE TABLE dbo.a(id int PRIMARY KEY, name varchar(10)); CREATE TABLE dbo.b(id int, a_id int, x varchar(10), n int); CREATE TABLE dbo.c(id int, b_id int, y int); CREATE TABLE dbo.d(k int, v int); CREATE TABLE dbo.e(s varchar(10), t int); CREATE TABLE dbo.docs(id int, doc nvarchar(200), csv nvarchar(20), tag varchar(20))",
        "INSERT dbo.a VALUES (1,'one'),(2,'two'),(3,NULL); INSERT dbo.b VALUES (10,1,'p',NULL),(11,1,'q',5),(12,2,'r',6); INSERT dbo.c VALUES (100,10,7),(101,10,8),(102,12,9); INSERT dbo.d VALUES (1,1),(1,1),(1,2); INSERT dbo.e VALUES ('a',1),('A',2),('a ',3); INSERT dbo.docs VALUES (1,N'{\"a\":1}',N'x|y',N'é'),(2,N'{\"a\":2,\"list\":[1]}',N'z','b'),(3,NULL,NULL,NULL)",
    ] {
        run(&mut client, sql).await.unwrap();
    }
    (server, client)
}

#[tokio::test]
async fn workload_repros() {
    let (_server, mut client) = setup().await;
    // SQL Server rejects FOR JSON AUTO without a table (13600).
    assert_eq!(
        error(&mut client, "SELECT 1 AS value FOR JSON AUTO").await,
        (13600, 1)
    );
    assert_eq!(
        scalar(
            &mut client,
            "SELECT JSON_MODIFY(N'{\"a\":1}', N'$.a', 2) AS value"
        )
        .await,
        json!("{\"a\":2}")
    );
    assert_eq!(
        scalar(&mut client, "SELECT STRING_AGG(CAST(id AS varchar(10)), ',') WITHIN GROUP (ORDER BY id DESC) AS value FROM dbo.a").await,
        json!("3,2,1")
    );
    assert_eq!(
        scalar(&mut client, "SELECT HASHBYTES('MD5', N'foo') AS value").await,
        json!("0x76fb6c8507d6cf995f6476646c9ea277")
    );
    assert_eq!(
        rows(&mut client, "SELECT value FROM STRING_SPLIT('a,b', ',')").await,
        [[json!("a")], [json!("b")]]
    );
    assert_eq!(
        column_types(&mut client, "SELECT JSON_MODIFY(N'{}', '$.a', 1) AS j, HASHBYTES('MD5', 'a') AS h, STRING_AGG(name, ',') WITHIN GROUP (ORDER BY id) AS s FROM dbo.a").await,
        [ColumnType::NVarchar, ColumnType::BigVarBin, ColumnType::BigVarChar]
    );
    assert_eq!(
        column_types(
            &mut client,
            "SELECT value, ordinal FROM STRING_SPLIT(N'a', N',', 1)"
        )
        .await,
        [ColumnType::NVarchar, ColumnType::Int8]
    );
}

#[tokio::test]
async fn for_json_auto_nests_like_sql_server() {
    let (_server, mut client) = setup().await;
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/gaps-json_string.json")).unwrap();
    let mut compared = 0;
    for record in fixture["runs"][0].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        // Every captured FOR JSON AUTO observation.
        if !name.starts_with("auto ") {
            continue;
        }
        let sql = record["sql"].as_str().unwrap();
        let result = run(&mut client, sql).await;
        match record["messages"].as_array().unwrap().first() {
            Some(expected) => assert_eq!(
                result,
                Err((
                    expected["number"].as_u64().unwrap() as u32,
                    expected["state"].as_u64().unwrap() as u8
                )),
                "{name}"
            ),
            None => {
                let expected = record["sets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|set| {
                        set["rows"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|row| row.as_array().unwrap().clone())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    result.unwrap_or_else(|e| panic!("{name}: {e:?}")),
                    expected,
                    "{name}"
                );
            }
        }
        compared += 1;
    }
    assert!(compared >= 45, "{compared}");
}

#[tokio::test]
async fn json_modify_replays_the_reference_fixture() {
    let (_server, mut client) = setup().await;
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/json-constructors.json")).unwrap();
    let mut compared = 0;
    for record in fixture["containers"][0]["runs"][0].as_array().unwrap() {
        let sql = record["sql"].as_str().unwrap_or_default();
        // JSON_OBJECT, JSON_ARRAY, the JSON type and sp_describe_first_result_set
        // are outside this task; NCHAR concatenation chains are a separate
        // msduck limitation. RPC records are replayed by the tedious test.
        if !sql.contains("JSON_MODIFY")
            || record.get("parameters").is_some()
            || [
                "JSON_OBJECT",
                "JSON_ARRAY",
                "AS JSON",
                "NCHAR(9)",
                "sp_describe",
            ]
            .iter()
            .any(|skip| sql.contains(skip))
        {
            let name = record["name"].as_str().unwrap();
            if name.starts_with("create ") || name.starts_with("insert ") {
                run(&mut client, sql).await.unwrap();
            }
            continue;
        }
        let result = run(&mut client, sql).await;
        let reference = &record["result"];
        match reference["errors"].as_array().unwrap().first() {
            Some(expected) => assert_eq!(
                result,
                Err((
                    expected["number"].as_u64().unwrap() as u32,
                    expected["state"].as_u64().unwrap() as u8
                )),
                "{sql}"
            ),
            None => {
                let expected = reference["sets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|set| {
                        set["rows"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|row| row.as_array().unwrap().clone())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    result.unwrap_or_else(|e| panic!("{sql}: {e:?}")),
                    expected,
                    "{sql}"
                );
            }
        }
        compared += 1;
    }
    assert!(compared >= 60, "{compared}");
}

#[tokio::test]
async fn json_modify_editing_and_errors() {
    let (_server, mut client) = setup().await;
    for (sql, expected) in [
        (
            "SELECT JSON_MODIFY(N'{ \"a\" : 1 , \"b\" : 2 }','$.a',NULL)",
            "{  \"b\" : 2 }",
        ),
        (
            "SELECT JSON_MODIFY(N'{ \"a\" : 1 }','$.b',2)",
            "{ \"a\" : 1 ,\"b\":2}",
        ),
        (
            "SELECT JSON_MODIFY(N'{\"arr\":[ 1 , 2 ]}','append $.arr',3)",
            "{\"arr\":[ 1 , 2 ,3]}",
        ),
        (
            "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',JSON_QUERY(N'[3]'))",
            "{\"a\":[3]}",
        ),
        (
            "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',CAST(123.456 AS FLOAT))",
            "{\"a\":1.234560000000000e+002}",
        ),
        (
            "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',(SELECT 1 AS x FOR JSON PATH))",
            "{\"a\":[{\"x\":1}]}",
        ),
    ] {
        assert_eq!(scalar(&mut client, sql).await, json!(expected), "{sql}");
    }
    assert_eq!(
        rows(
            &mut client,
            "SELECT id, JSON_MODIFY(doc, 'append $.list', id) FROM dbo.docs ORDER BY id"
        )
        .await,
        [
            [json!(1), json!("{\"a\":1,\"list\":[1]}")],
            [json!(2), json!("{\"a\":2,\"list\":[1,2]}")],
            [json!(3), Value::Null]
        ]
    );
    for (sql, expected) in [
        ("SELECT JSON_MODIFY(N'{}','strict $.a',1)", (13608, 2)),
        (
            "SELECT JSON_MODIFY(N'{\"a\":1}','append strict $.a',1)",
            (13621, 1),
        ),
        ("SELECT JSON_MODIFY(N'{\"a\":1}','$',1)", (13619, 1)),
        ("SELECT JSON_MODIFY(N'not json','$.a',1)", (13609, 7)),
        ("SELECT JSON_MODIFY(N'{}','a',1)", (13607, 22)),
        ("SELECT JSON_MODIFY(N'{}','$.*',1)", (13660, 4)),
        ("SELECT JSON_MODIFY(N'{}','$.a')", (174, 1)),
        (
            "SELECT JSON_MODIFY(N'{}','$.a',CAST(1 AS MONEY))",
            (8116, 1),
        ),
    ] {
        assert_eq!(error(&mut client, sql).await, expected, "{sql}");
    }
    // Scalar contexts and TRY/CATCH see the same identity.
    assert_eq!(
        rows(&mut client, "BEGIN TRY DECLARE @d nvarchar(max) = JSON_MODIFY(N'{}', 'strict $.a', 1) END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_STATE() END CATCH").await,
        [[json!(13608), json!(2)]]
    );
    assert_eq!(
        scalar(&mut client, "DECLARE @d nvarchar(max) = N'{\"a\":1}'; SET @d = JSON_MODIFY(@d, '$.b', N'x'); SET @d = JSON_MODIFY(@d, '$.a', NULL); SELECT @d").await,
        json!("{\"b\":\"x\"}")
    );
}

#[tokio::test]
async fn string_agg_and_string_split() {
    let (_server, mut client) = setup().await;
    assert_eq!(
        rows(&mut client, "SELECT a_id, STRING_AGG(x, '|') WITHIN GROUP (ORDER BY id DESC) FROM dbo.b GROUP BY a_id ORDER BY a_id").await,
        [[json!(1), json!("q|p")], [json!(2), json!("r")]]
    );
    assert_eq!(
        scalar(&mut client, "SELECT STRING_AGG(value, '|') WITHIN GROUP (ORDER BY value DESC) FROM STRING_SPLIT('c,a,b', ',')").await,
        json!("c|b|a")
    );
    assert_eq!(
        rows(&mut client, "SELECT d.id, s.value, s.ordinal FROM dbo.docs d CROSS APPLY STRING_SPLIT(d.csv, N'|', 1) s ORDER BY d.id, s.ordinal").await,
        [
            [json!(1), json!("x"), json!("1")],
            [json!(1), json!("y"), json!("2")],
            [json!(2), json!("z"), json!("1")]
        ]
    );
    for (sql, expected) in [
        (
            "SELECT STRING_AGG(name, CAST(',' AS varchar(5))) FROM dbo.a",
            (8733, 1),
        ),
        ("SELECT STRING_AGG(name, ',') OVER () FROM dbo.a", (4113, 4)),
        (
            "SELECT STRING_AGG(name, ',') WITHIN GROUP (ORDER BY id), STRING_AGG(name, ',') WITHIN GROUP (ORDER BY id DESC) FROM dbo.a",
            (8711, 1),
        ),
        (
            "SELECT STRING_AGG(CAST(REPLICATE('x',3000) AS VARCHAR(3000)),'') FROM (VALUES(1),(2),(3)) d(i)",
            (9829, 0),
        ),
        ("SELECT value FROM STRING_SPLIT('a,b', '')", (214, 11)),
        ("SELECT value FROM STRING_SPLIT('a,b', ',', 2)", (4199, 1)),
        (
            "DECLARE @o bit = 1; SELECT value FROM STRING_SPLIT('a,b', ',', @o)",
            (8748, 1),
        ),
        ("SELECT value FROM STRING_SPLIT(123, ',')", (8116, 1)),
        ("SELECT value FROM STRING_SPLIT('a')", (313, 3)),
    ] {
        assert_eq!(error(&mut client, sql).await, expected, "{sql}");
    }
}

#[tokio::test]
async fn hashbytes_algorithms_and_encodings() {
    let (_server, mut client) = setup().await;
    assert_eq!(
        rows(&mut client, "SELECT HASHBYTES('MD2', 'abc'), HASHBYTES('MD4', 'abc'), HASHBYTES('MD5', 'abc'), HASHBYTES('SHA', 'abc'), HASHBYTES('SHA1', 'abc'), HASHBYTES('SHA2_256', 'abc')").await,
        [[
            Value::Null,
            json!("0xa448017aaf21d8525fc10ae87aa6729d"),
            json!("0x900150983cd24fb0d6963f7d28e17f72"),
            json!("0xa9993e364706816aba3e25717850c26c9cd0d89d"),
            json!("0xa9993e364706816aba3e25717850c26c9cd0d89d"),
            json!("0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        ]]
    );
    // VARCHAR hashes code-page bytes, NVARCHAR UTF-16LE, VARBINARY raw bytes.
    let v = scalar(&mut client, "SELECT HASHBYTES('MD5', 'é')").await;
    assert_eq!(
        v,
        scalar(&mut client, "SELECT HASHBYTES('MD5', 0xE9)").await
    );
    assert_eq!(
        scalar(&mut client, "SELECT HASHBYTES('MD5', N'é')").await,
        scalar(&mut client, "SELECT HASHBYTES('MD5', 0xE900)").await
    );
    assert_eq!(
        rows(
            &mut client,
            "SELECT id, HASHBYTES('SHA1', csv), HASHBYTES('SHA1', tag) FROM dbo.docs WHERE id = 2"
        )
        .await,
        [[
            json!(2),
            scalar(&mut client, "SELECT HASHBYTES('SHA1', N'z')").await,
            scalar(&mut client, "SELECT HASHBYTES('SHA1', 'b')").await,
        ]]
    );
    let digest = scalar(&mut client, "SELECT HASHBYTES('SHA2_512', N'abc')").await;
    assert_eq!(digest.as_str().unwrap().len(), 2 + 128);
    for (sql, expected) in [
        ("SELECT HASHBYTES('MD5')", (174, 1)),
        ("SELECT HASHBYTES('MD5', 1)", (8116, 1)),
        ("SELECT HASHBYTES(NULL, 'a')", (8116, 1)),
    ] {
        assert_eq!(error(&mut client, sql).await, expected, "{sql}");
    }
}
