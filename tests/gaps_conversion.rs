//! Explicit COLLATE, styled CONVERT, FORMAT, SERVERPROPERTY,
//! DATABASEPROPERTYEX and ROWCOUNT_BIG (issue #725), against the values and
//! error numbers SQL Server returned in reference/gaps-conversion.json and
//! reference/format.json.
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

/// The first row's text columns.
async fn texts(client: &mut SqlClient, sql: &str) -> Vec<Option<String>> {
    let rows = client
        .simple_query(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_first_result()
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let row = &rows[0];
    (0..row.len())
        .map(|i| row.get::<&str, _>(i).map(str::to_owned))
        .collect()
}

async fn ints(client: &mut SqlClient, sql: &str) -> Vec<Option<i32>> {
    let rows = client
        .simple_query(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_first_result()
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let row = &rows[0];
    (0..row.len()).map(|i| row.get::<i32, _>(i)).collect()
}

/// The SQL Server error number a batch fails with.
async fn error(client: &mut SqlClient, sql: &str) -> u32 {
    let result = match client.simple_query(sql).await {
        Ok(stream) => stream.into_results().await.map(|_| ()),
        Err(error) => Err(error),
    };
    match result {
        Err(tiberius::error::Error::Server(token)) => token.code(),
        other => panic!("{sql}: expected a server error, got {other:?}"),
    }
}

fn some(values: &[&str]) -> Vec<Option<String>> {
    values.iter().map(|v| Some((*v).to_owned())).collect()
}

#[tokio::test]
async fn reported_repros_now_succeed() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    assert_eq!(
        ints(
            &mut client,
            "SELECT CASE WHEN N'A' = N'a' COLLATE Latin1_General_CI_AS THEN 1 ELSE 0 END, CASE WHEN N'A' = N'a' COLLATE Latin1_General_CS_AS THEN 1 ELSE 0 END"
        )
        .await,
        vec![Some(1), Some(0)]
    );
    assert_eq!(
        texts(
            &mut client,
            "SELECT CONVERT(nvarchar(40), CAST('2024-01-02 03:04:05.1234567 +05:30' AS datetimeoffset), 127), CONVERT(varchar(30), CAST('2024-01-02 03:04:05.123' AS datetime), 126), CONVERT(varchar(6), 0x010203, 2), FORMAT(CAST('2024-01-01' AS date), 'yyyy-MM-dd')"
        )
        .await,
        some(&["2024-01-01T21:34:05.1234567Z", "2024-01-02T03:04:05.123", "010203", "2024-01-01"])
    );
    assert_eq!(
        texts(
            &mut client,
            "SELECT CONVERT(nvarchar(128), SERVERPROPERTY('Collation')), CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion')), CONVERT(nvarchar(128), DATABASEPROPERTYEX('master', 'Status')), CONVERT(nvarchar(128), DATABASEPROPERTYEX(DB_NAME(), 'Updateability'))"
        )
        .await,
        some(&["Latin1_General_100_BIN2", "16.0.0.0", "ONLINE", "READ_WRITE"])
    );
    let rows = client
        .simple_query("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3; SELECT ROWCOUNT_BIG()")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert_eq!(rows[1][0].get::<i64, _>(0), Some(3));
}

#[tokio::test]
async fn collations_compare_and_sort_with_their_sensitivity() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    client
        .simple_query("CREATE TABLE dbo.names (id int, s nvarchar(20)); INSERT dbo.names VALUES (1,N'b'),(2,N'a'),(3,N'B'),(4,N'A'),(5,N'é'),(6,N'e'),(7,N'f'),(8,N'É')")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    // Comparisons from reference/gaps-conversion.json: equal and less than
    // for (A,a), (é,e), (É,e), ('a ','a'), (abc,ABD).
    let pairs = [
        ("N'A'", "N'a'"),
        ("N'é'", "N'e'"),
        ("N'É'", "N'e'"),
        ("N'a '", "N'a'"),
        ("N'abc'", "N'ABD'"),
    ];
    for (collation, expected) in [
        ("Latin1_General_CI_AS", [1, 0, 0, 0, 0, 0, 1, 0, 0, 1]),
        ("Latin1_General_CS_AS", [0, 0, 0, 0, 0, 0, 1, 0, 0, 1]),
        ("Latin1_General_CI_AI", [1, 0, 1, 0, 1, 0, 1, 0, 0, 1]),
        (
            "SQL_Latin1_General_CP1_CI_AS",
            [1, 0, 0, 0, 0, 0, 1, 0, 0, 1],
        ),
        ("Latin1_General_100_BIN2", [0, 1, 0, 0, 0, 0, 1, 0, 0, 0]),
        ("Latin1_General_BIN", [0, 1, 0, 0, 0, 0, 1, 0, 0, 0]),
    ] {
        let sql = format!(
            "SELECT {}",
            pairs
                .iter()
                .map(|(l, r)| format!(
                    "CASE WHEN {l} = {r} COLLATE {collation} THEN 1 ELSE 0 END, CASE WHEN {l} < {r} COLLATE {collation} THEN 1 ELSE 0 END"
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let got: Vec<i32> = ints(&mut client, &sql)
            .await
            .into_iter()
            .map(Option::unwrap)
            .collect();
        assert_eq!(got, expected, "{collation}");
    }
    for (collation, expected) in [
        (
            "Latin1_General_CI_AS",
            ["A", "a", "B", "b", "e", "É", "é", "f"],
        ),
        (
            "Latin1_General_CS_AS",
            ["a", "A", "b", "B", "e", "é", "É", "f"],
        ),
        (
            "Latin1_General_100_BIN2",
            ["A", "B", "a", "b", "e", "f", "É", "é"],
        ),
    ] {
        let rows = client
            .simple_query(format!(
                "SELECT s FROM dbo.names ORDER BY s COLLATE {collation}, s COLLATE Latin1_General_100_BIN2"
            ))
            .await
            .unwrap()
            .into_first_result()
            .await
            .unwrap();
        let got: Vec<&str> = rows.iter().map(|r| r.get::<&str, _>(0).unwrap()).collect();
        assert_eq!(got, expected, "{collation}");
    }
    assert_eq!(
        ints(
            &mut client,
            "SELECT COUNT(DISTINCT x COLLATE Latin1_General_CI_AS), COUNT(DISTINCT x COLLATE Latin1_General_CI_AI), (SELECT COUNT(*) FROM dbo.names WHERE s COLLATE Latin1_General_CI_AS IN (N'A', N'É')) FROM (VALUES (N'b'),(N'a'),(N'B'),(N'A'),(N'é'),(N'e'),(N'É')) t(x)"
        )
        .await,
        vec![Some(4), Some(3), Some(4)]
    );
    assert_eq!(error(&mut client, "SELECT N'a' COLLATE Foo_Bar").await, 448);
    assert_eq!(
        error(&mut client, "SELECT 1 COLLATE Latin1_General_CI_AS").await,
        447
    );
}

#[tokio::test]
async fn styles_format_and_parse_like_sql_server() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let datetime = "CAST('2024-01-02 03:04:05.123' AS datetime)";
    let mut expected = Vec::new();
    let mut columns = Vec::new();
    for (style, text) in [
        (0, "Jan  2 2024  3:04AM"),
        (1, "01/02/24"),
        (3, "02/01/24"),
        (9, "Jan  2 2024  3:04:05:123AM"),
        (13, "02 Jan 2024 03:04:05:123"),
        (14, "03:04:05:123"),
        (20, "2024-01-02 03:04:05"),
        (22, "01/02/24  3:04:05 AM"),
        (23, "2024-01-02"),
        (101, "01/02/2024"),
        (103, "02/01/2024"),
        (104, "02.01.2024"),
        (107, "Jan 02, 2024"),
        (112, "20240102"),
        (120, "2024-01-02 03:04:05"),
        (121, "2024-01-02 03:04:05.123"),
        (126, "2024-01-02T03:04:05.123"),
        (127, "2024-01-02T03:04:05.123"),
        (131, "21/06/1445  3:04:05:123AM"),
    ] {
        columns.push(format!("CONVERT(varchar(40), {datetime}, {style})"));
        expected.push(text);
    }
    assert_eq!(
        texts(&mut client, &format!("SELECT {}", columns.join(", "))).await,
        some(&expected)
    );
    assert_eq!(
        texts(
            &mut client,
            "SELECT CONVERT(varchar(40), CAST('2024-01-02 03:04:05.1234567' AS datetime2), 121), CONVERT(varchar(40), CAST('2024-01-02 03:04:05.1234567 +05:30' AS datetimeoffset(7)), 126), CONVERT(varchar(40), CAST('03:04:05.123' AS time(3)), 114), CONVERT(varchar(40), CAST('2024-01-02' AS date), 107), CONVERT(char(25), CAST('2024-01-02 03:04:05.123' AS datetime), 121), CONVERT(varchar(10), CAST('2024-01-02 03:04:05.123' AS datetime), 121)"
        )
        .await,
        some(&["2024-01-02 03:04:05.1234567", "2024-01-02T03:04:05.1234567+05:30", "03:04:05.123", "Jan 02, 2024", "2024-01-02 03:04:05.123  ", "2024-01-02"])
    );
    // Character to date/time with styles, read back with style 126.
    assert_eq!(
        texts(
            &mut client,
            "SELECT CONVERT(varchar(30), CONVERT(datetime, '02/01/2024', 103), 126), CONVERT(varchar(30), CONVERT(datetime, '2024-01-02', 103), 126), CONVERT(varchar(30), CONVERT(date, '20240102', 112), 126), CONVERT(varchar(40), CONVERT(datetimeoffset, '2024-01-02T03:04:05.1234567+05:30', 127), 126), CONVERT(varchar(30), CONVERT(datetime, 'Jan  2 2024  3:04:05:123AM', 109), 126), CONVERT(varchar(30), TRY_CONVERT(datetime, 'garbage', 120), 126)"
        )
        .await,
        vec![
            Some("2024-01-02T00:00:00".into()),
            Some("2024-02-01T00:00:00".into()),
            Some("2024-01-02".into()),
            Some("2024-01-02T03:04:05.1234567+05:30".into()),
            Some("2024-01-02T03:04:05.123".into()),
            None
        ]
    );
    // Binary styles in both directions.
    assert_eq!(
        texts(
            &mut client,
            "SELECT CONVERT(varchar(20), 0x0A0B, 1), CONVERT(varchar(20), 0x0A0B, 2), CONVERT(varchar(20), 0x414243, 0), CONVERT(varchar(3), 0x0A0B, 1), CONVERT(varchar(20), CONVERT(varbinary(4), '0x0a0B', 1), 1), CONVERT(varchar(20), CONVERT(binary(4), '0102', 2), 1)"
        )
        .await,
        some(&["0x0A0B", "0A0B", "ABC", "0x", "0x0A0B", "0x01020000"])
    );
    for (sql, number) in [
        (
            "SELECT CONVERT(varchar(30), CAST('2024-01-02' AS datetime), 99)",
            281,
        ),
        (
            "SELECT CONVERT(varchar(30), CAST('2024-01-02' AS date), 108)",
            8114,
        ),
        (
            "SELECT CONVERT(varchar(30), CAST('2024-01-02' AS date), 114)",
            281,
        ),
        ("SELECT CONVERT(varbinary(4), 'abc', 3)", 9809),
        ("SELECT CONVERT(varbinary(4), '0x123', 1)", 8114),
        ("SELECT CONVERT(datetime, 'garbage', 120)", 241),
        ("SELECT CONVERT(datetime, '2024-13-02', 120)", 242),
        ("SELECT CONVERT(smalldatetime, '2024-01-02', 23)", 295),
    ] {
        assert_eq!(error(&mut client, sql).await, number, "{sql}");
    }
    // TRY_CONVERT returns NULL for an invalid style.
    assert_eq!(
        texts(
            &mut client,
            "SELECT TRY_CONVERT(varchar(30), CAST('2024-01-02' AS datetime), 99)"
        )
        .await,
        vec![None]
    );
}

#[tokio::test]
async fn format_follows_dotnet_for_en_us_and_invariant() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    let decimal = "CAST(-1234567.8951 AS DECIMAL(19,4))";
    let dt2 = "CAST('2024-03-05 14:07:09.1234567' AS DATETIME2(7))";
    let dto = "CAST('2024-03-05 14:07:09.1234567 -05:30' AS DATETIMEOFFSET(7))";
    let cases = [
        (format!("FORMAT({decimal}, 'N')"), "-1,234,567.90"),
        (
            format!("FORMAT({decimal}, 'C', 'en-US')"),
            "($1,234,567.90)",
        ),
        (
            format!("FORMAT({decimal}, 'C', 'iv')"),
            "(\u{a4}1,234,567.90)",
        ),
        (format!("FORMAT({decimal}, 'P', 'iv')"), "-123,456,789.51 %"),
        (format!("FORMAT({decimal}, 'E')"), "-1.234568E+006"),
        (format!("FORMAT({decimal}, '#,##0.00')"), "-1,234,567.90"),
        ("FORMAT(CAST(1234 AS INT), 'X8')".into(), "000004D2"),
        ("FORMAT(CAST(0.1 AS FLOAT), 'R')".into(), "0.1"),
        (
            "FORMAT(CAST(1234.5 AS DECIMAL(10,2)), '0.00;(0.00);zero')".into(),
            "1234.50",
        ),
        (format!("FORMAT({dt2}, 'D')"), "Tuesday, March 5, 2024"),
        (format!("FORMAT({dt2}, 'G', 'iv')"), "03/05/2024 14:07:09"),
        (
            format!("FORMAT({dt2}, 'yyyy-MM-dd HH:mm:ss.fffffff')"),
            "2024-03-05 14:07:09.1234567",
        ),
        (
            format!("FORMAT({dto}, 'O')"),
            "2024-03-05T14:07:09.1234567-05:30",
        ),
        (format!("FORMAT({dto}, 'u')"), "2024-03-05 19:37:09Z"),
        (
            "FORMAT(CAST('14:07:09.1234567' AS TIME(7)), 'G')".into(),
            "0:14:07:09.1234567",
        ),
        (
            "FORMAT(CAST('2024-03-05' AS DATE), 'xx', 'xx-XX')".into(),
            "xx",
        ),
    ];
    let sql = format!(
        "SELECT {}",
        cases
            .iter()
            .map(|(sql, _)| sql.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let expected: Vec<&str> = cases.iter().map(|(_, value)| *value).collect();
    assert_eq!(texts(&mut client, &sql).await, some(&expected));
    // An invalid format string gives NULL.
    assert_eq!(
        texts(&mut client, "SELECT FORMAT(1, 'Q')").await,
        vec![None]
    );
    for (sql, number) in [
        ("SELECT FORMAT(1)", 189),
        ("SELECT FORMAT('1234', 'N')", 8116),
        ("SELECT FORMAT(CAST(1 AS BIT), 'N')", 8116),
        ("SELECT FORMAT(1, 'N', 'Klingon')", 9818),
        ("SELECT FORMAT(1, 'N', NULL)", 9818),
        ("SELECT FORMAT(1, 1)", 8116),
    ] {
        assert_eq!(error(&mut client, sql).await, number, "{sql}");
    }
}

#[tokio::test]
async fn properties_are_typed_like_sql_server() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    // sql_variant base types, as SQL_VARIANT_PROPERTY reports them.
    assert_eq!(
        texts(
            &mut client,
            "SELECT CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SERVERPROPERTY('Edition'), 'BaseType')), CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SERVERPROPERTY('EngineEdition'), 'BaseType')), CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SERVERPROPERTY('SqlCharSet'), 'BaseType')), CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(DATABASEPROPERTYEX('master', 'Version'), 'BaseType'))"
        )
        .await,
        some(&["nvarchar", "int", "tinyint", "int"])
    );
    assert_eq!(
        ints(
            &mut client,
            "SELECT CAST(SERVERPROPERTY('EngineEdition') AS int), CAST(DATABASEPROPERTYEX('master', 'Version') AS int), CASE WHEN SERVERPROPERTY('EngineEdition') = 3 THEN 1 ELSE 0 END, CASE WHEN SERVERPROPERTY('InstanceName') IS NULL THEN 1 ELSE 0 END, CASE WHEN SERVERPROPERTY('IsCaseSensitive') IS NULL THEN 1 ELSE 0 END, CASE WHEN DATABASEPROPERTYEX('no_such_database', 'Status') IS NULL THEN 1 ELSE 0 END, CASE WHEN SERVERPROPERTY('ServerName') = SERVERPROPERTY('MachineName') THEN 1 ELSE 0 END"
        )
        .await,
        vec![Some(3), Some(957), Some(1), Some(1), Some(1), Some(1), Some(1)]
    );
    assert_eq!(
        texts(
            &mut client,
            "DECLARE @p nvarchar(128) = N'ProductLevel'; SELECT CONVERT(nvarchar(128), SERVERPROPERTY(@p)), CONVERT(nvarchar(128), DATABASEPROPERTYEX(N'MASTER', N'Recovery')), CONVERT(nvarchar(128), DATABASEPROPERTYEX('master', 'UserAccess')), CONVERT(nvarchar(128), DATABASEPROPERTYEX('master', 'Collation'))"
        )
        .await,
        some(&["RTM", "SIMPLE", "MULTI_USER", "Latin1_General_100_BIN2"])
    );
    for (sql, number) in [
        ("SELECT SERVERPROPERTY()", 174),
        ("SELECT DATABASEPROPERTYEX('master')", 174),
        ("SELECT ROWCOUNT_BIG(1)", 174),
    ] {
        assert_eq!(error(&mut client, sql).await, number, "{sql}");
    }
    client
        .simple_query("CREATE TABLE dbo.counted (i int); INSERT dbo.counted VALUES (1),(2)")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let rows = client
        .simple_query("INSERT dbo.counted VALUES (3),(4),(5); SELECT ROWCOUNT_BIG(), @@ROWCOUNT")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let last = rows.last().unwrap();
    assert_eq!(last[0].get::<i64, _>(0), Some(3));
}
