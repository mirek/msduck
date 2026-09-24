//! Aggregate payload conversion is independent of collation ordering. These
//! single-value groups deliberately do not certify linguistic sort weights.
use msduck::{engine::Session, server::Server};

fn execute(session: &mut Session, sql: &str) {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {tokens:?}");
}

#[test]
fn character_extrema_keep_view_payloads_nulls_and_explicit_cast_widths() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE VIEW aggregate_json AS SELECT * FROM OPENJSON(N'[{\"s\":\"🦆x\"},{\"s\":null}]') WITH(s NVARCHAR(8))",
    );
    for (index, (value, filter, expected)) in [
        ("MIN(s)", "", Some(vec![0xd83e, 0xdd86, 120])),
        ("MAX(s)", "", Some(vec![0xd83e, 0xdd86, 120])),
        ("CAST(MAX(s) AS NVARCHAR(1))", "", Some(vec![0xd83e])),
        (
            "CAST(MIN(s) AS NCHAR(5))",
            "",
            Some(vec![0xd83e, 0xdd86, 120, 32, 32]),
        ),
        ("MAX(s)", "WHERE s IS NULL", None),
        ("MIN(s)", "WHERE 1=0", None),
        (
            "(SELECT MAX(s) FROM aggregate_json)",
            "WHERE 1=0",
            Some(vec![0xd83e, 0xdd86, 120]),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let query = if value.starts_with("(SELECT") {
            format!("SELECT {value} AS v INTO aggregate_result_{index}")
        } else {
            format!(
                "SELECT {value} AS v INTO aggregate_result_{index} FROM aggregate_json {filter}"
            )
        };
        execute(&mut session, &query);
        let bytes: Option<Vec<u8>> = session
            .db
            .query_row(
                &format!("SELECT v.__msduck_utf16le FROM aggregate_result_{index}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expected = expected.map(|units| {
            units
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        });
        assert_eq!(bytes, expected, "{query}");
    }
}

#[test]
fn aggregate_character_casts_preserve_numeric_conversion_and_ansi_payloads() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "SELECT CAST(MAX(n) AS NVARCHAR(8)) AS v INTO numeric_aggregate FROM (VALUES(12),(NULL)) d(n)",
    );
    let bytes: Vec<u8> = session
        .db
        .query_row(
            "SELECT v.__msduck_utf16le FROM numeric_aggregate",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bytes, vec![49, 0, 50, 0]);
    execute(
        &mut session,
        "SELECT CAST(MAX(s) AS VARCHAR(3)) AS v INTO ansi_aggregate FROM (SELECT CAST(N'€abc' AS NVARCHAR(8)) AS s) d",
    );
    let value: String = session
        .db
        .query_row("SELECT v FROM ansi_aggregate", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "€ab");
}

#[test]
fn engine_binds_bin2_before_converting_aggregate_operands_to_carriers() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    execute(
        &mut session,
        "CREATE TABLE bin2_aggregate(s NVARCHAR(8) COLLATE Latin1_General_100_BIN2); INSERT INTO bin2_aggregate VALUES(N'ÿ'),(N'Ā'),(NULL)",
    );
    for (index, source) in ["bin2_aggregate", "(SELECT s FROM bin2_aggregate) d"]
        .into_iter()
        .enumerate()
    {
        execute(
            &mut session,
            &format!("SELECT MIN(s) AS lo,MAX(s) AS hi INTO bin2_result_{index} FROM {source}"),
        );
        let values: (Vec<u8>, Vec<u8>) = session
            .db
            .query_row(
                &format!("SELECT lo.__msduck_utf16le,hi.__msduck_utf16le FROM bin2_result_{index}"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(values, (vec![255, 0], vec![0, 1]));
    }
}
