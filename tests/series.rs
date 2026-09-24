use msduck::{engine::Session, server::Server};

#[test]
fn series_arguments_are_evaluated_once_without_private_name_capture() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    // Observe sequence state through a second connection to the same database.
    let db = server.connection().unwrap();
    db.execute_batch("CREATE SEQUENCE dbo.series_calls START 1")
        .unwrap();
    for (sql, count) in [
        (
            "SELECT * FROM GENERATE_SERIES(CAST(nextval('dbo.series_calls') AS INT),CAST(nextval('dbo.series_calls') AS INT))",
            2_i64,
        ),
        (
            "SELECT * FROM GENERATE_SERIES(CAST(nextval('dbo.series_calls') AS DECIMAL(10,0)),CAST(nextval('dbo.series_calls') AS DECIMAL(10,0)),CAST(1 AS DECIMAL(10,0)))",
            4,
        ),
    ] {
        let (_, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}");
        assert_eq!(
            db.query_row(
                "SELECT last_value FROM duckdb_sequences() WHERE sequence_name='series_calls'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            count
        );
    }
    let (_, ok) = session.batch_response(
        "SELECT g.value INTO dbo.series_capture FROM (VALUES(3)) __msduck_series_input(__msduck_series_stop) CROSS APPLY GENERATE_SERIES(1,__msduck_series_input.__msduck_series_stop) g",
        &Default::default(), false, None,
    );
    assert!(ok);
    assert_eq!(
        db.query_row(
            "SELECT count(*),CAST(sum(value) AS BIGINT) FROM dbo.series_capture",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (3, 6)
    );
}
