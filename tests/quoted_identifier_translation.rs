//! Delimited parameter-like column names retain column values through root lowering.
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
async fn batch(client: &mut SqlClient, sql: &str) -> tiberius::Result<Vec<tiberius::Row>> {
    client.simple_query(sql).await?.into_first_result().await
}

#[tokio::test]
async fn quoted_columns_keep_nulls_despite_conflicting_rpc_parameter() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    batch(&mut c, "CREATE TABLE dbo.quoted_width(id INT NOT NULL, [@P1] INT NULL); INSERT INTO dbo.quoted_width VALUES(1,NULL),(2,7)").await.unwrap();
    for quoted in ["[@P1]", "\"@P1\""] {
        let sql = format!(
            "SELECT DATALENGTH({quoted}) AS width, {quoted} AS value, @P1 AS parameter FROM dbo.quoted_width ORDER BY id"
        );
        for value in [Some(42i64), None] {
            let rows = c
                .query(sql.as_str(), &[&value])
                .await
                .unwrap()
                .into_first_result()
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get::<i32, _>("width"), None);
            assert_eq!(rows[1].get::<i32, _>("width"), Some(4));
            assert_eq!(rows[0].get::<i32, _>("value"), None);
            assert_eq!(rows[1].get::<i32, _>("value"), Some(7));
            assert_eq!(rows[0].get::<i64, _>("parameter"), value);
        }
    }
}

#[tokio::test]
async fn quoted_counters_are_columns_in_batches_and_rpcs() {
    let server = Server::open(":memory:").unwrap();
    let mut c = connect(&server).await;
    batch(&mut c, "CREATE TABLE dbo.quoted_counter([@@ROWCOUNT] INT NULL, [@@ERROR] INT NULL); INSERT INTO dbo.quoted_counter VALUES(NULL,17),(9,NULL)").await.unwrap();
    let sql =
        "SELECT [@@ROWCOUNT] AS r, \"@@ERROR\" AS e FROM dbo.quoted_counter ORDER BY [@@ROWCOUNT]";
    for rows in [
        batch(&mut c, sql).await.unwrap(),
        c.query(sql, &[])
            .await
            .unwrap()
            .into_first_result()
            .await
            .unwrap(),
    ] {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<i32, _>("r"), None);
        assert_eq!(rows[0].get::<i32, _>("e"), Some(17));
        assert_eq!(rows[1].get::<i32, _>("r"), Some(9));
        assert_eq!(rows[1].get::<i32, _>("e"), None);
    }
    let rows = batch(&mut c, "SELECT @@ROWCOUNT AS r, @@ERROR AS e")
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>("e"), Some(0));
}
