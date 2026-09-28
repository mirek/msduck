use msduck::server::{Server, serve_connection};
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, error::Error};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

#[tokio::test]
async fn strict_partial_ranges_preserve_captured_wire_errors_and_connection() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/json-advanced-path.json")).unwrap();
    let server = Server::open(":memory:").unwrap();
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
    let mut client = Client::connect(config, stream.compat_write())
        .await
        .unwrap();

    let mut compared = 0;
    for record in fixture["containers"][0]["runs"][0].as_array().unwrap() {
        let reference = &record["result"]["errors"][0];
        if reference["number"] != 13659 {
            continue;
        }
        assert!(record["result"]["sets"].as_array().unwrap().is_empty());
        let sql = record["sql"].as_str().unwrap();
        let error = match client.simple_query(sql).await {
            Err(error) => error,
            Ok(stream) => stream.into_results().await.unwrap_err(),
        };
        let Error::Server(error) = error else {
            panic!("expected SQL Server error for {sql}: {error}");
        };
        assert_eq!(error.code(), 13659, "{sql}");
        assert_eq!(
            error.state(),
            reference["state"].as_u64().unwrap() as u8,
            "{sql}"
        );
        assert_eq!(
            error.class(),
            reference["class"].as_u64().unwrap() as u8,
            "{sql}"
        );
        assert_eq!(
            error.message(),
            reference["message"].as_str().unwrap(),
            "{sql}"
        );
        compared += 1;
    }
    assert_eq!(compared, 18);

    for sql in [
        "SELECT JSON_VALUE('[1]','strict $[0 to 2]') AS value",
        "SELECT JSON_QUERY('[1]','strict $[0 to 2]') AS value",
    ] {
        let error = match client.simple_query(sql).await {
            Err(error) => error,
            Ok(stream) => stream.into_results().await.unwrap_err(),
        };
        let Error::Server(error) = error else {
            panic!("expected SQL Server error for {sql}: {error}");
        };
        assert_eq!(error.code(), 13659);
        assert_eq!(
            error.message(),
            "Index 0 provided at position 3 is not within the array of size 2."
        );
    }

    let columns = &fixture["containers"][0]["runs"][0][0]["result"]["sets"][0]["columns"];
    let mut stream = client
        .simple_query("SELECT JSON_VALUE(N'[1,2]',N'$[0 to 0]') AS value")
        .await
        .unwrap();
    let actual_columns = stream.columns().await.unwrap().unwrap();
    assert_eq!(actual_columns.len(), 1);
    assert_eq!(
        actual_columns[0].name(),
        columns[0]["name"].as_str().unwrap()
    );
    assert_eq!(columns[0]["type"], "NVarChar");
    assert_eq!(
        actual_columns[0].column_type(),
        tiberius::ColumnType::NVarchar
    );
    let rows = stream.into_first_result().await.unwrap();
    assert_eq!(rows[0].get::<&str, _>(0), Some("1"));
}
