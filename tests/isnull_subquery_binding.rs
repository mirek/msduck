//! Native dispatch scope and evaluation controls; SQL Server replay is client-side.
use msduck::{engine::Session, server::Server};

fn session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    (server, session)
}

#[test]
fn scalar_first_and_lazy_replacement_advance_native_sequences_once() {
    let (_server, session) = session();
    session
        .db
        .execute_batch(
            "CREATE SEQUENCE first_counter START 10; CREATE SEQUENCE replacement_counter START 100",
        )
        .unwrap();
    let value: i64=session.db.query_row(
        "SELECT __msduck_isnull_subquery((SELECT nextval('first_counter')), nextval('replacement_counter'))",[],|r|r.get(0)).unwrap();
    assert_eq!(value, 10);
    let counters: (Option<i64>,Option<i64>)=session.db.query_row(
        "SELECT (SELECT last_value FROM duckdb_sequences() WHERE sequence_name='first_counter'),
                (SELECT last_value FROM duckdb_sequences() WHERE sequence_name='replacement_counter')",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(counters, (Some(10), None));
    let value:i64=session.db.query_row(
        "SELECT __msduck_isnull_subquery((SELECT NULL::BIGINT), nextval('replacement_counter'))",[],|r|r.get(0)).unwrap();
    assert_eq!(value, 100);
    let replacement: i64 = session
        .db
        .query_row(
            "SELECT last_value FROM duckdb_sequences() WHERE sequence_name='replacement_counter'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(replacement, 100);
}

#[test]
fn correlated_empty_and_null_subqueries_keep_cardinality_and_types() {
    let (_server, session) = session();
    let mut statement=session.db.prepare(
        "SELECT x.id, __msduck_isnull_subquery((SELECT v FROM (VALUES (1,7),(3,NULL::INTEGER)) t(id,v) WHERE t.id=x.id),99)
         FROM (VALUES (1),(2),(3)) x(id) ORDER BY x.id").unwrap();
    let rows: Vec<(i32, i32)> = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![(1, 7), (2, 99), (3, 99)]);
    for function in ["__msduck_isnull", "__msduck_isnull_subquery"] {
        let result = session.db.query_row::<i32, _, _>(
            &format!("SELECT {function}((SELECT v FROM (VALUES (1),(2)) t(v)),0)"),
            [],
            |r| r.get(0),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("More than one row")
        );
    }
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT 1", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

#[test]
fn scalar_dispatch_keeps_utf16_carriers_and_typed_null_fallbacks() {
    let (_server, session) = session();
    let (units,integer):(Vec<u8>,i32)=session.db.query_row(
        "SELECT (__msduck_isnull_subquery((SELECT __msduck_unicode_from_le(from_hex('00d8'))), 'unused')).__msduck_utf16le,
                __msduck_isnull_subquery((SELECT NULL::INTEGER), '7')",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(units, vec![0, 0xd8]);
    assert_eq!(integer, 7);
}

fn batch(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {response:?}");
}

#[test]
fn public_dispatch_preserves_outer_aggregate_and_window_replacements() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        "SELECT ISNULL((SELECT CAST(NULL AS INT)), SUM(v)) AS v INTO aggregate_replacement FROM (VALUES (1),(2),(3)) t(v)",
    );
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT v FROM aggregate_replacement", [], |r| r.get(0))
            .unwrap(),
        6
    );
    batch(
        &mut session,
        "SELECT id, ISNULL((SELECT CAST(NULL AS INT)), SUM(id) OVER ()) AS v INTO window_replacement FROM (VALUES (1),(2),(3)) t(id)",
    );
    let actual: Vec<(i32, i32)> = session
        .db
        .prepare("SELECT id,v FROM window_replacement ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(actual, vec![(1, 6), (2, 6), (3, 6)]);
}

#[test]
fn public_dispatch_keeps_multirow_volatile_replacements_lazy() {
    let (_server, mut session) = session();
    session
        .db
        .execute_batch(
            "CREATE SEQUENCE public_replacement START 100; CREATE SEQUENCE public_first START 10",
        )
        .unwrap();
    batch(
        &mut session,
        "SELECT id, ISNULL((SELECT CAST(NULL AS BIGINT)), nextval('public_replacement')) AS v INTO volatile_replacements FROM (VALUES (1),(2),(3)) t(id)",
    );
    let mut values: Vec<i64> = session
        .db
        .prepare("SELECT v FROM volatile_replacements")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    values.sort_unstable();
    assert_eq!(values, vec![100, 101, 102]);
    batch(
        &mut session,
        "SELECT id, ISNULL((SELECT CAST(7 AS BIGINT)), nextval('public_replacement')) AS v INTO skipped_replacements FROM (VALUES (1),(2),(3)) t(id)",
    );
    assert_eq!(session.db.query_row::<i64, _, _>("SELECT last_value FROM duckdb_sequences() WHERE sequence_name='public_replacement'", [], |r| r.get(0)).unwrap(), 102);
    batch(
        &mut session,
        "SELECT id, ISNULL((SELECT nextval('public_first')), CAST(0 AS BIGINT)) AS v INTO volatile_first FROM (VALUES (1),(2),(3)) t(id)",
    );
    let values: Vec<i64> = session
        .db
        .prepare("SELECT v FROM volatile_first")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(values, vec![10, 10, 10]);
    assert_eq!(
        session
            .db
            .query_row::<i64, _, _>(
                "SELECT last_value FROM duckdb_sequences() WHERE sequence_name='public_first'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        10
    );
}

#[test]
fn public_correlated_dispatch_preserves_missing_null_and_error_cases() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        "SELECT x.id, ISNULL((SELECT v FROM (VALUES (1,7),(3,NULL)) t(id,v) WHERE t.id=x.id), x.id+90) AS v INTO correlated_results FROM (VALUES (1),(2),(3)) x(id)",
    );
    let actual: Vec<(i32, i32)> = session
        .db
        .prepare("SELECT id,v FROM correlated_results ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(actual, vec![(1, 7), (2, 92), (3, 93)]);
    batch(
        &mut session,
        "SELECT CASE WHEN ISNULL((SELECT MAX(v) FROM (VALUES (N'02')) t(v)), N'')=2 THEN 1 ELSE 0 END AS hit INTO numeric_precedence",
    );
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT hit FROM numeric_precedence", [], |r| r.get(0))
            .unwrap(),
        1
    );
    let (_, ok) = session.batch_response(
        "SELECT ISNULL((SELECT v FROM (VALUES (1),(2)) t(v)), 0)",
        &Default::default(),
        false,
        None,
    );
    assert!(!ok);
    batch(
        &mut session,
        "SELECT ISNULL((SELECT CAST(NULL AS INT)), '7') AS v INTO typed_fallback",
    );
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT v FROM typed_fallback", [], |r| r.get(0))
            .unwrap(),
        7
    );
    let (_, ok) = session.batch_response(
        "SELECT ISNULL((SELECT CAST(NULL AS INT)), 'bad')",
        &Default::default(),
        false,
        None,
    );
    assert!(!ok);
    batch(
        &mut session,
        "SELECT ISNULL((SELECT CAST(7 AS INT)), 'bad') AS v INTO skipped_bad_conversion",
    );
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT v FROM skipped_bad_conversion", [], |r| r.get(0))
            .unwrap(),
        7
    );
}

#[test]
fn public_subquery_carriers_keep_units_through_concat_and_comparison() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"SELECT ISNULL((SELECT j.[value] FROM OPENJSON(N'{"s":"\ud800"}') j), N'') AS v,
        N'<' + ISNULL((SELECT j.[value] FROM OPENJSON(N'{"s":"\ud800"}') j), N'') + N'>' AS c,
        CASE WHEN ISNULL((SELECT j.[value] FROM OPENJSON(N'{"s":"\ud800"}') j), N'') =
            (SELECT j.[value] FROM OPENJSON(N'{"s":"\ud800"}') j) THEN 1 ELSE 0 END AS hit
        INTO subquery_carriers"#,
    );
    let (value, concat, hit): (Vec<u8>, Vec<u8>, i32) = session
        .db
        .query_row(
            "SELECT v.__msduck_utf16le, c.__msduck_utf16le, hit FROM subquery_carriers",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(value, vec![0, 0xd8]);
    assert_eq!(concat, vec![b'<', 0, 0, 0xd8, b'>', 0]);
    assert_eq!(hit, 1);
}

#[test]
fn carrier_width_preserves_isolated_units_padding_and_null_validity() {
    let (_server, session) = session();
    let actual: (Vec<u8>, Vec<u8>, Option<Vec<u8>>) = session.db.query_row(
        "SELECT (__msduck_isnull_nvarchar_width(__msduck_unicode_from_le(from_hex('3dd800de7800')),1)).__msduck_utf16le,
                (__msduck_isnull_nchar_width(__msduck_unicode_from_le(from_hex('00d8')),3)).__msduck_utf16le,
                (__msduck_isnull_nvarchar_width(NULL::STRUCT(__msduck_utf16le BLOB),3)).__msduck_utf16le",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        actual,
        (vec![0x3d, 0xd8], vec![0, 0xd8, 32, 0, 32, 0], None)
    );
    assert!(
        session
            .db
            .query_row::<duckdb::types::Value, _, _>(
                "SELECT __msduck_isnull_nvarchar_width({'__msduck_utf16le':NULL::BLOB},3)",
                [],
                |r| r.get(0)
            )
            .is_err()
    );
    assert!(
        session
            .db
            .query_row::<duckdb::types::Value, _, _>(
                "SELECT __msduck_isnull_nvarchar_width({'__msduck_utf16le':from_hex('00')},3)",
                [],
                |r| r.get(0)
            )
            .is_err()
    );
}

#[test]
fn public_width_comparison_keeps_derived_numeric_peer_coercion() {
    let (_server, mut session) = session();
    for (name, sql) in [
        (
            "derived_numeric_peer",
            "SELECT CASE WHEN ISNULL((SELECT CAST(N'02' AS NVARCHAR(2))),N'') = t.n THEN 1 ELSE 0 END AS hit INTO derived_numeric_peer FROM (SELECT 2 AS n) t",
        ),
        (
            "cte_numeric_peer",
            "WITH t(n) AS (SELECT 2) SELECT CASE WHEN ISNULL((SELECT CAST(N'02' AS NVARCHAR(2))),N'') = t.n THEN 1 ELSE 0 END AS hit INTO cte_numeric_peer FROM t",
        ),
    ] {
        batch(&mut session, sql);
        assert_eq!(
            session
                .db
                .query_row::<i32, _, _>(&format!("SELECT hit FROM {name}"), [], |r| r.get(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn width_adapter_does_not_promote_unknown_integer_marker_to_text() {
    let (_server, mut session) = session();
    // Native adapter/marker entry point. The ordinary public derived/CTE
    // controls above also test any earlier binding that may resolve the peer.
    batch(
        &mut session,
        "SELECT CASE WHEN __msduck_isnull_nvarchar_width((SELECT N'02'),2) = __msduck_unicode_maybe(t.n) THEN 1 ELSE 0 END AS hit INTO unknown_marker_peer FROM (SELECT 2 AS n) t",
    );
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT hit FROM unknown_marker_peer", [], |r| r.get(0))
            .unwrap(),
        1
    );
    batch(
        &mut session,
        "SELECT CASE WHEN __msduck_isnull_nvarchar_width((SELECT N'02'),2) = __msduck_unicode(CHAR(2)) THEN 1 ELSE 0 END AS hit INTO integer_unicode_function_peer",
    );
    assert_eq!(
        session
            .db
            .query_row::<i32, _, _>("SELECT hit FROM integer_unicode_function_peer", [], |r| r
                .get(0))
            .unwrap(),
        1
    );
}
