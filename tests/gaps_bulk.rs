//! Bulk load (gaps-bulk-v1, docs/gaps-bulk.md): INSERT BULK followed by a
//! BulkLoadBCP message. The session-level tests write the wire bytes
//! themselves, so they cover every fragmentation of a message and the
//! diagnostics SQL Server sends (reference/gaps-bulk.json); the client test
//! loads rows through tiberius, an independent TDS client, over a socket.
use msduck::engine::Session;
use msduck::server::{Server, serve_connection};
use std::net::TcpListener;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const COLLATION: [u8; 5] = [0x09, 0x04, 0xd0, 0x00, 0x34];
const PREMATURE_END: &str = "While reading current row from host, a premature end-of-message was encountered--an incoming data stream was interrupted when the server expected to see more data. The host program may have terminated. Ensure that you are using a supported client application programming interface (API).";

/// One COLMETADATA column: its TYPE_INFO bytes and NULLABLE flag.
struct Column {
    name: &'static str,
    type_info: Vec<u8>,
    nullable: bool,
}

fn int4(name: &'static str) -> Column {
    Column {
        name,
        type_info: vec![0x38],
        nullable: false,
    }
}

fn int_n(name: &'static str) -> Column {
    Column {
        name,
        type_info: vec![0x26, 4],
        nullable: true,
    }
}

fn varchar(name: &'static str, length: u16, nullable: bool) -> Column {
    let mut type_info = vec![0xa7];
    type_info.extend(length.to_le_bytes());
    type_info.extend(COLLATION);
    Column {
        name,
        type_info,
        nullable,
    }
}

fn nvarchar_max(name: &'static str) -> Column {
    let mut type_info = vec![0xe7, 0xff, 0xff];
    type_info.extend(COLLATION);
    Column {
        name,
        type_info,
        nullable: true,
    }
}

fn metadata(columns: &[Column]) -> Vec<u8> {
    let mut out = vec![0x81];
    out.extend((columns.len() as u16).to_le_bytes());
    for column in columns {
        out.extend(0u32.to_le_bytes());
        // Updateable read/write, plus NULLABLE.
        out.extend((0x08 | u16::from(column.nullable)).to_le_bytes());
        out.extend(&column.type_info);
        let name: Vec<u16> = column.name.encode_utf16().collect();
        out.push(name.len() as u8);
        out.extend(name.iter().flat_map(|unit| unit.to_le_bytes()));
    }
    out
}

fn fixed_int(value: i32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn nullable_int(value: Option<i32>) -> Vec<u8> {
    match value {
        Some(value) => [vec![4], value.to_le_bytes().to_vec()].concat(),
        None => vec![0],
    }
}

fn short_text(value: Option<&str>) -> Vec<u8> {
    match value {
        Some(value) => {
            let mut out = (value.len() as u16).to_le_bytes().to_vec();
            out.extend(value.as_bytes());
            out
        }
        None => vec![0xff, 0xff],
    }
}

/// A PLP value in chunks of at most `chunk` bytes.
fn plp_unicode(value: &str, chunk: usize) -> Vec<u8> {
    let bytes: Vec<u8> = value.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = (bytes.len() as u64).to_le_bytes().to_vec();
    for part in bytes.chunks(chunk) {
        out.extend((part.len() as u32).to_le_bytes());
        out.extend(part);
    }
    out.extend(0u32.to_le_bytes());
    out
}

fn row(values: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0xd1];
    for value in values {
        out.extend(value);
    }
    out
}

fn done() -> Vec<u8> {
    let mut out = vec![0xfd, 0, 0, 0, 0];
    out.extend(0u64.to_le_bytes());
    out
}

/// A session in a fresh user database; the server keeps it alive.
fn session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk");
    ok(&mut session, "USE bulk");
    (server, session)
}

fn ok(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok && !response.contains(&0xaa), "{sql}: {response:?}");
}

/// The final DONE token's status, command and row count.
fn done_token(response: &[u8]) -> (u16, u16, u64) {
    let done = &response[response.len() - 13..];
    assert_eq!(done[0], 0xfd, "{response:?}");
    (
        u16::from_le_bytes([done[1], done[2]]),
        u16::from_le_bytes([done[3], done[4]]),
        u64::from_le_bytes(done[5..13].try_into().unwrap()),
    )
}

/// The bytes of an ERROR token's number, state, class and message.
fn error_body(number: i32, state: u8, class: u8, message: &str) -> Vec<u8> {
    let units: Vec<u16> = message.encode_utf16().collect();
    let mut body = number.to_le_bytes().to_vec();
    body.extend([state, class]);
    body.extend((units.len() as u16).to_le_bytes());
    body.extend(units.iter().flat_map(|u| u.to_le_bytes()));
    body
}

fn has_error(response: &[u8], number: i32, state: u8, class: u8, message: &str) -> bool {
    let body = error_body(number, state, class, message);
    response.windows(body.len()).any(|w| w == body.as_slice())
}

/// Send INSERT BULK, then the message in packets of `packet` bytes.
fn load(session: &mut Session, statement: &str, message: &[u8], packet: usize) -> Vec<u8> {
    let (response, ok) = session.batch_response(statement, &Default::default(), false, None);
    assert!(ok, "{statement}: {response:?}");
    assert_eq!(done_token(&response), (0, 253, 0));
    assert!(session.bulk_load_expected());
    let packets: Vec<&[u8]> = message.chunks(packet.max(1)).collect();
    for (index, part) in packets.iter().enumerate() {
        session.bulk_load_packet(part, index + 1 == packets.len());
    }
    let response = session.bulk_load_finish(false);
    assert!(!session.bulk_load_expected());
    response
}

fn count(session: &Session, sql: &str) -> i64 {
    session.db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn every_fragmentation_of_a_message_loads_the_same_rows() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE items (id int NOT NULL, n int NULL, name varchar(10) NULL, notes nvarchar(max) NULL)",
    );
    let columns = [
        int4("id"),
        int_n("n"),
        varchar("name", 10, true),
        nvarchar_max("notes"),
    ];
    let mut message = metadata(&columns);
    message.extend(row(&[
        fixed_int(1),
        nullable_int(Some(-5)),
        short_text(Some("abc")),
        plp_unicode("ž🦆 duck", 3),
    ]));
    message.extend(row(&[
        fixed_int(2),
        nullable_int(None),
        short_text(None),
        vec![0xff; 8],
    ]));
    message.extend(done());
    for packet in 1..=message.len() {
        ok(&mut session, "DELETE FROM items");
        let response = load(
            &mut session,
            "INSERT BULK items ([id] int, [n] int, [name] varchar(10), [notes] nvarchar(max))",
            &message,
            packet,
        );
        assert_eq!(
            done_token(&response),
            (0x10, 240, 2),
            "packet size {packet}"
        );
        assert_eq!(session.rowcount, 2);
        assert_eq!(
            count(
                &session,
                "SELECT count(*) FROM dbo.items WHERE id = 1 AND n = -5 AND name = 'abc'"
            ),
            1
        );
        assert_eq!(
            count(
                &session,
                "SELECT count(*) FROM dbo.items WHERE id = 2 AND n IS NULL AND name IS NULL AND notes IS NULL"
            ),
            1
        );
    }
}

#[test]
fn metadata_and_stream_errors_match_sql_server() {
    let (_server, mut session) = session();
    ok(&mut session, "CREATE TABLE n (a int NULL, b int NOT NULL)");
    // The NULLABLE flag must match the column (4816, colid is 1-based).
    let mut message = metadata(&[int4("a"), int4("b")]);
    message.extend(row(&[fixed_int(1), fixed_int(2)]));
    message.extend(done());
    let response = load(
        &mut session,
        "INSERT BULK n ([a] int, [b] int)",
        &message,
        7,
    );
    assert!(has_error(
        &response,
        4816,
        1,
        16,
        "Invalid column type from bcp client for colid 1."
    ));
    assert_eq!(done_token(&response), (2, 253, 0));
    assert_eq!(session.last_error, 4816);
    // The wire type must be the declared type.
    let mut message = metadata(&[varchar("b", 5, false)]);
    message.extend(row(&[short_text(Some("7"))]));
    message.extend(done());
    let response = load(&mut session, "INSERT BULK n ([b] int)", &message, 512);
    assert!(has_error(
        &response,
        4816,
        1,
        16,
        "Invalid column type from bcp client for colid 1."
    ));
    // A message with only DONE (tedious's zero-row load).
    let response = load(&mut session, "INSERT BULK n ([b] int)", &done(), 512);
    assert!(has_error(&response, 4804, 2, 16, PREMATURE_END));
    assert_eq!(done_token(&response), (2, 253, 0));
    // More metadata columns than the statement declares.
    let mut message = metadata(&[int4("b"), int_n("a")]);
    message.extend(done());
    let response = load(&mut session, "INSERT BULK n ([b] int)", &message, 512);
    assert!(has_error(&response, 4804, 3, 16, PREMATURE_END));
    // A row that does not match its metadata.
    let mut message = metadata(&[int4("b")]);
    message.extend([0xd1, 1, 0]);
    message.extend(done());
    let response = load(&mut session, "INSERT BULK n ([b] int)", &message, 512);
    assert!(has_error(&response, 4804, 1, 17, PREMATURE_END));
    // Metadata and DONE without rows load nothing and succeed.
    let mut message = metadata(&[int4("b")]);
    message.extend(done());
    let response = load(&mut session, "INSERT BULK n ([b] int)", &message, 512);
    assert_eq!(done_token(&response), (0x10, 240, 0));
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.n"), 0);
}

#[test]
fn statement_errors_and_protocol_order_match_sql_server() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE t (id int NOT NULL, v AS id * 2)",
    );
    let run = |session: &mut Session, sql: &str| {
        let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(!ok, "{sql}");
        assert_eq!(done_token(&response), (2, 253, 0), "{sql}");
        assert!(!session.bulk_load_expected(), "{sql}");
        response
    };
    let response = run(&mut session, "insert bulk nosuch ([id] int)");
    let missing = error_body(208, 1, 16, "Invalid object name 'nosuch'.");
    assert_eq!(
        response
            .windows(missing.len())
            .filter(|w| *w == missing.as_slice())
            .count(),
        2
    );
    // An unknown column: a failed DONE without a diagnostic.
    let response = run(&mut session, "insert bulk t ([nosuch] int)");
    assert_eq!(response.len(), 13);
    let response = run(&mut session, "insert bulk t ([id] int, [ID] int)");
    assert!(response.windows(4).any(|w| w == 264i32.to_le_bytes()));
    let response = run(&mut session, "insert bulk t ([v] int)");
    assert!(has_error(
        &response,
        271,
        1,
        16,
        "The column \"v\" cannot be modified because it is either a computed column or is the result of a UNION operator."
    ));
    let response = run(&mut session, "insert bulk t ([id] notatype)");
    assert!(has_error(
        &response,
        2715,
        2,
        16,
        "Column, parameter, or variable #1: Cannot find data type notatype."
    ));
    let response = run(
        &mut session,
        "insert bulk t ([id] int) WITH (KEEP_IDENTITY)",
    );
    assert!(has_error(
        &response,
        102,
        1,
        15,
        "Incorrect syntax near 'KEEP_IDENTITY'."
    ));
    // Other captured INSERT BULK syntax errors complete the same way.
    for (sql, message) in [
        (
            "insert bulk t ([id] int) WITH (FOO)",
            "Incorrect syntax near 'FOO'.",
        ),
        ("insert bulk t", "Incorrect syntax near 't'."),
    ] {
        let response = run(&mut session, sql);
        assert!(has_error(&response, 102, 1, 15, message), "{sql}");
    }
    for sql in [
        "SELECT 1; insert bulk t ([id] int)",
        "insert bulk t ([id] int); SELECT 1",
    ] {
        let response = run(&mut session, sql);
        assert!(has_error(
            &response,
            428,
            1,
            16,
            "Insert bulk cannot be used in a multi-statement batch."
        ));
    }
    // Another request instead of the bulk data: 4022, and the INSERT BULK
    // statement is forgotten.
    let (response, ok) =
        session.batch_response("insert bulk t ([id] int)", &Default::default(), false, None);
    assert!(ok, "{response:?}");
    assert!(session.bulk_load_expected());
    let response = session.bulk_load_missing();
    assert!(has_error(
        &response,
        4022,
        1,
        16,
        "Bulk load data was expected but not sent. The batch will be terminated."
    ));
    assert_eq!(done_token(&response), (2, 253, 0));
    assert!(!session.bulk_load_expected());
    // A BulkLoadBCP message without INSERT BULK.
    assert_eq!(done_token(&session.bulk_load_finish(false)), (2, 0, 0));
    // A message the client abandoned (IGNORE) loads nothing.
    let (_, ok) =
        session.batch_response("insert bulk t ([id] int)", &Default::default(), false, None);
    assert!(ok);
    let mut message = metadata(&[int4("id")]);
    message.extend(row(&[fixed_int(1)]));
    session.bulk_load_packet(&message, true);
    assert_eq!(done_token(&session.bulk_load_finish(true)), (2, 253, 0));
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.t"), 0);
}

#[test]
fn options_identity_defaults_constraints_and_triggers() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE items (id int IDENTITY(10,5) NOT NULL, v varchar(10) NULL CONSTRAINT df_v DEFAULT 'dflt', n int NULL CONSTRAINT ck_n CHECK (n > 0))",
    );
    ok(&mut session, "CREATE TABLE audit (rows_seen int NULL)");
    ok(
        &mut session,
        "CREATE TRIGGER tr_items ON items AFTER INSERT AS INSERT audit SELECT count(*) FROM inserted",
    );
    let message = |rows: &[(Option<&str>, Option<i32>)]| {
        let mut message = metadata(&[varchar("v", 10, true), int_n("n")]);
        for (v, n) in rows {
            message.extend(row(&[short_text(*v), nullable_int(*n)]));
        }
        message.extend(done());
        message
    };
    // Without options: NULL takes the default, CHECK and triggers are
    // skipped, and the constraint is no longer trusted.
    let response = load(
        &mut session,
        "INSERT BULK items ([v] varchar(10), [n] int)",
        &message(&[(None, Some(-1)), (Some("x"), Some(2))]),
        512,
    );
    assert_eq!(done_token(&response), (0x10, 240, 2));
    assert_eq!(
        count(
            &session,
            "SELECT count(*) FROM dbo.items WHERE id = 10 AND v = 'dflt' AND n = -1 OR id = 15 AND v = 'x' AND n = 2"
        ),
        2
    );
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.audit"), 0);
    assert_eq!(
        count(
            &session,
            "SELECT count(*) FROM main.__msduck_constraints WHERE name = 'ck_n' AND is_not_trusted AND NOT is_disabled"
        ),
        1
    );
    // KEEP_NULLS keeps NULL; CHECK_CONSTRAINTS checks (547, then 3621);
    // FIRE_TRIGGERS fires once for the load.
    let response = load(
        &mut session,
        "INSERT BULK items ([v] varchar(10), [n] int) WITH (KEEP_NULLS, CHECK_CONSTRAINTS, FIRE_TRIGGERS)",
        &message(&[(None, Some(3)), (None, Some(-3))]),
        512,
    );
    assert!(response.windows(4).any(|w| w == 547i32.to_le_bytes()));
    assert_eq!(done_token(&response), (2, 240, 0));
    assert_eq!(session.last_error, 547);
    let response = load(
        &mut session,
        "INSERT BULK items ([v] varchar(10), [n] int) WITH (KEEP_NULLS, CHECK_CONSTRAINTS, FIRE_TRIGGERS)",
        &message(&[(None, Some(3)), (None, Some(4)), (None, None)]),
        512,
    );
    assert_eq!(done_token(&response), (0x10, 240, 3));
    assert_eq!(
        count(&session, "SELECT count(*) FROM dbo.items WHERE v IS NULL"),
        3
    );
    assert_eq!(count(&session, "SELECT max(rows_seen) FROM dbo.audit"), 3);
    // Listing the identity column keeps its values.
    let mut kept = metadata(&[int4("id"), varchar("v", 10, true)]);
    kept.extend(row(&[fixed_int(100), short_text(Some("kept"))]));
    kept.extend(done());
    let response = load(
        &mut session,
        "INSERT BULK items ([id] int, [v] varchar(10))",
        &kept,
        512,
    );
    assert_eq!(done_token(&response), (0x10, 240, 1));
    assert_eq!(
        count(
            &session,
            "SELECT count(*) FROM dbo.items WHERE id = 100 AND v = 'kept'"
        ),
        1
    );
}

#[test]
fn large_loads_stage_and_stay_atomic() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY, v varchar(10) NULL CONSTRAINT df_v DEFAULT 'dflt')",
    );
    let message = |ids: &mut dyn Iterator<Item = i32>| {
        let mut message = metadata(&[int4("id"), varchar("v", 10, true)]);
        for id in ids {
            let v = (id % 4 != 0).then(|| format!("v{id}"));
            message.extend(row(&[fixed_int(id), short_text(v.as_deref())]));
        }
        message.extend(done());
        message
    };
    let statement = "INSERT BULK items ([id] int, [v] varchar(10))";
    let response = load(&mut session, statement, &message(&mut (0..3000)), 4096);
    assert_eq!(done_token(&response), (0x10, 240, 3000));
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.items"), 3000);
    assert_eq!(
        count(&session, "SELECT count(*) FROM dbo.items WHERE v = 'dflt'"),
        750
    );
    // A duplicate in the last rows fails the whole load.
    let response = load(
        &mut session,
        statement,
        &message(&mut (5000..7000).chain([5])),
        4096,
    );
    assert!(response.windows(4).any(|w| w == 2627i32.to_le_bytes()));
    assert_eq!(done_token(&response), (2, 240, 0));
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.items"), 3000);
    // The staging table was dropped, so the next large load stages again.
    let response = load(&mut session, statement, &message(&mut (5000..7000)), 4096);
    assert_eq!(done_token(&response), (0x10, 240, 2000));
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.items"), 5000);
    // Rows whose every column takes its default, in a staged load.
    ok(
        &mut session,
        "CREATE TABLE defaults (v varchar(10) NULL CONSTRAINT df_defaults DEFAULT 'z')",
    );
    let mut message = metadata(&[varchar("v", 10, true)]);
    for index in 0..1500 {
        let value = (index % 100 == 0).then_some("a");
        message.extend(row(&[short_text(value)]));
    }
    message.extend(done());
    let response = load(
        &mut session,
        "INSERT BULK defaults ([v] varchar(10))",
        &message,
        4096,
    );
    assert_eq!(done_token(&response), (0x10, 240, 1500));
    assert_eq!(
        count(&session, "SELECT count(*) FROM dbo.defaults WHERE v = 'z'"),
        1485
    );
    assert_eq!(
        count(&session, "SELECT count(*) FROM dbo.defaults WHERE v = 'a'"),
        15
    );
}

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

#[tokio::test]
async fn tiberius_bulk_insert_streams_rows_over_the_socket() {
    let server = Server::open(":memory:").unwrap();
    let mut client = connect(&server).await;
    client
        .simple_query(
            "CREATE TABLE items (id int IDENTITY(1,1) NOT NULL, code int NOT NULL, name nvarchar(40) NULL, flag bit NULL, data varbinary(max) NULL)",
        )
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    // tiberius reads the columns with SELECT TOP 0 and leaves out the
    // identity column, as SqlBulkCopy does without KeepIdentity.
    let mut load = client.bulk_insert("items").await.unwrap();
    for code in 0..1500i32 {
        let name = (code % 7 != 0).then(|| format!("name {code} ž"));
        let data = (code % 5 == 0).then(|| vec![code as u8; 300]);
        load.send((code, name, code % 2 == 0, data).into_row())
            .await
            .unwrap();
    }
    let result = load.finalize().await.unwrap();
    assert_eq!(result.total(), 1500);
    let rows = client
        .simple_query(
            "SELECT count(*), min(id), max(id), sum(code), count(name), count(data), sum(CAST(flag AS int)) FROM items",
        )
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    let row = &rows[0];
    assert_eq!(row.get::<i32, _>(0), Some(1500));
    assert_eq!(row.get::<i32, _>(1), Some(1));
    assert_eq!(row.get::<i32, _>(2), Some(1500));
    assert_eq!(row.get::<i32, _>(3), Some((0..1500).sum()));
    assert_eq!(row.get::<i32, _>(4), Some(1500 - 215));
    assert_eq!(row.get::<i32, _>(5), Some(300));
    assert_eq!(row.get::<i32, _>(6), Some(750));
    let rows = client
        .simple_query("SELECT name, data FROM items WHERE code = 10")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<&str, _>(0), Some("name 10 ž"));
    assert_eq!(rows[0].get::<&[u8], _>(1), Some(&[10u8; 300][..]));
    // The connection serves ordinary requests afterwards.
    let rows = client
        .simple_query("SELECT 42")
        .await
        .unwrap()
        .into_first_result()
        .await
        .unwrap();
    assert_eq!(rows[0].get::<i32, _>(0), Some(42));
}

use tiberius::IntoRow;

#[test]
fn a_failing_or_rolled_back_trigger_fails_the_load_and_leaves_no_transaction() {
    let (_server, mut session) = session();
    ok(&mut session, "CREATE TABLE items (id int NOT NULL)");
    ok(
        &mut session,
        "CREATE TRIGGER tr_items ON items AFTER INSERT AS IF EXISTS (SELECT 1 FROM inserted WHERE id < 0) THROW 50001, 'negative id', 1",
    );
    let message = |ids: &[i32]| {
        let mut message = metadata(&[int4("id")]);
        for id in ids {
            message.extend(row(&[fixed_int(*id)]));
        }
        message.extend(done());
        message
    };
    let statement = "INSERT BULK items ([id] int) WITH (FIRE_TRIGGERS)";
    let response = load(&mut session, statement, &message(&[1, -1]), 512);
    assert!(has_error(&response, 50001, 1, 16, "negative id"));
    assert_eq!(done_token(&response).0 & 2, 2);
    // No transaction change reaches the client for the load's own
    // transaction.
    assert!(!response.windows(4).any(|w| w == [0xe3, 11, 0, 10]));
    assert_eq!(session.transactions, 0);
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.items"), 0);
    let response = load(&mut session, statement, &message(&[2, 3]), 512);
    assert_eq!(done_token(&response), (0x10, 240, 2));
    assert_eq!(session.transactions, 0);
    ok(&mut session, "CREATE TABLE other (id int NOT NULL)");
    ok(
        &mut session,
        "CREATE TRIGGER tr_other ON other AFTER INSERT AS ROLLBACK",
    );
    let response = load(
        &mut session,
        "INSERT BULK other ([id] int) WITH (FIRE_TRIGGERS)",
        &message(&[1]),
        512,
    );
    assert_eq!(done_token(&response).0 & 2, 2, "{response:?}");
    assert_eq!(session.transactions, 0);
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.other"), 0);
    // The session still works, inside and outside a transaction.
    ok(&mut session, "BEGIN TRANSACTION");
    let response = load(&mut session, statement, &message(&[4]), 512);
    assert_eq!(done_token(&response), (0x10, 240, 1));
    assert_eq!(session.transactions, 1);
    ok(&mut session, "COMMIT");
    assert_eq!(count(&session, "SELECT count(*) FROM dbo.items"), 3);
}

#[test]
fn the_sessions_identity_insert_setting_survives_a_load() {
    let (_server, mut session) = session();
    ok(
        &mut session,
        "CREATE TABLE items (id int IDENTITY(1,1) NOT NULL, v int NULL)",
    );
    ok(
        &mut session,
        "CREATE TABLE other (id int IDENTITY(1,1) NOT NULL, v int NULL)",
    );
    let message = |id: i32| {
        let mut message = metadata(&[int4("id"), int_n("v")]);
        message.extend(row(&[fixed_int(id), nullable_int(Some(id))]));
        message.extend(done());
        message
    };
    let statement = "INSERT BULK items ([id] int, [v] int)";
    // The target's own setting stays ON.
    ok(&mut session, "SET IDENTITY_INSERT items ON");
    let response = load(&mut session, statement, &message(10), 512);
    assert_eq!(done_token(&response), (0x10, 240, 1));
    ok(&mut session, "INSERT items (id, v) VALUES (11, 11)");
    ok(&mut session, "SET IDENTITY_INSERT items OFF");
    // Another table's setting is suspended for the load, then restored.
    ok(&mut session, "SET IDENTITY_INSERT other ON");
    let response = load(&mut session, statement, &message(20), 512);
    assert_eq!(done_token(&response), (0x10, 240, 1));
    ok(&mut session, "INSERT other (id, v) VALUES (30, 30)");
    ok(&mut session, "SET IDENTITY_INSERT other OFF");
    // Without a setting, none is left behind.
    let response = load(&mut session, statement, &message(40), 512);
    assert_eq!(done_token(&response), (0x10, 240, 1));
    let (response, _) = session.batch_response(
        "INSERT items (id, v) VALUES (50, 50)",
        &Default::default(),
        false,
        None,
    );
    assert!(response.windows(4).any(|w| w == 544i32.to_le_bytes()));
    assert_eq!(
        count(
            &session,
            "SELECT count(*) FROM dbo.items WHERE id IN (10, 11, 20, 40)"
        ),
        4
    );
    assert_eq!(
        count(&session, "SELECT count(*) FROM dbo.other WHERE id = 30"),
        1
    );
}

#[test]
fn names_with_closing_brackets_load() {
    let (_server, mut session) = session();
    ok(&mut session, "CREATE TABLE [we]]ird] ([c]]ol] int NULL)");
    let mut message = metadata(&[int_n("c]ol")]);
    message.extend(row(&[nullable_int(Some(7))]));
    message.extend(done());
    let response = load(
        &mut session,
        "INSERT BULK [dbo].[we]]ird] ([c]]ol] int)",
        &message,
        512,
    );
    assert_eq!(done_token(&response), (0x10, 240, 1));
    assert_eq!(
        count(
            &session,
            "SELECT count(*) FROM dbo.\"we]ird\" WHERE \"c]ol\" = 7"
        ),
        1
    );
}
