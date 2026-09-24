//! Unicode cast annotations preserve conversion boundaries and evaluation counts.
use msduck::{engine::Session, server::Server};

#[test]
fn nested_unicode_casts_preserve_distinct_widths_and_fixed_padding() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (response, ok) = session.batch_response(
        "CREATE TABLE cast_chain(s NVARCHAR(24)); INSERT INTO cast_chain VALUES(N'🦆x'),(NULL); SELECT STRING_ESCAPE(CAST(CAST(s AS NVARCHAR(1)) AS NVARCHAR(4)),'json') AS varying,STRING_ESCAPE(CAST(CAST(s AS NCHAR(1)) AS NCHAR(4)),'json') AS fixed INTO cast_chain_result FROM cast_chain",
        &Default::default(),false,None,
    );
    assert!(ok, "{response:?}");
    type ByteColumns = (Option<Vec<u8>>, Option<Vec<u8>>);
    let rows: Vec<ByteColumns> = session.db
        .prepare("SELECT varying.__msduck_utf16le,fixed.__msduck_utf16le FROM cast_chain_result ORDER BY varying NULLS FIRST").unwrap()
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap()
        .collect::<duckdb::Result<_>>().unwrap();
    assert_eq!(
        rows,
        vec![
            (None, None),
            (
                Some(vec![0x3e, 0xd8]),
                Some(vec![0x3e, 0xd8, 32, 0, 32, 0, 32, 0])
            ),
        ]
    );
}

#[test]
fn repeated_unicode_cast_annotations_evaluate_volatile_source_once() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE cast_chain_calls")
        .unwrap();
    let mut value = "nextval('cast_chain_calls')".to_owned();
    for _ in 0..8 {
        value = format!("CAST({value} AS NVARCHAR(24))");
    }
    let sql = format!(
        "SELECT STRING_ESCAPE({value},'json') AS v INTO cast_chain_values FROM GENERATE_SERIES(1,6000)"
    );
    let (response, ok) = session.batch_response(&sql, &Default::default(), false, None);
    assert!(ok, "{response:?}");
    let values: Vec<Vec<u8>> = session
        .db
        .prepare("SELECT v.__msduck_utf16le FROM cast_chain_values")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    let mut numbers: Vec<usize> = values
        .iter()
        .map(|bytes| {
            let units: Vec<_> = bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            String::from_utf16(&units).unwrap().parse().unwrap()
        })
        .collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=6000).collect::<Vec<_>>());
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('cast_chain_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
}

#[test]
fn for_json_subquery_casts_keep_each_explicit_width_before_storage() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (response,ok) = session.batch_response(
        "SELECT CAST(CAST((SELECT LEFT(N'🦆',1) AS s FOR JSON PATH,WITHOUT_ARRAY_WRAPPER) AS NVARCHAR(7)) AS NCHAR(9)) AS s INTO cast_json_result",
        &Default::default(),false,None,
    );
    assert!(ok, "{response:?}");
    let bytes: Vec<u8> = session
        .db
        .query_row("SELECT s.__msduck_utf16le FROM cast_json_result", [], |r| {
            r.get(0)
        })
        .unwrap();
    let units: [u16; 9] = [123, 34, 115, 34, 58, 34, 0xd83e, 32, 32];
    assert_eq!(
        bytes,
        units
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
}
