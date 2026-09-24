use msduck::{engine::Session, server::Server};

#[test]
fn bin2_extrema_preserve_payloads_and_compare_logical_units() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    // Captured BIN2 group 4: U+00FF sorts before U+0100 despite LE byte order.
    let result:(String,String)=session.db.query_row("SELECT hex((__msduck_min_bin2_unicode(v)).__msduck_utf16le),hex((__msduck_max_bin2_unicode(v)).__msduck_utf16le) FROM (VALUES (__msduck_unicode_from_le(from_hex('FF00'))),(__msduck_unicode_from_le(from_hex('0001')))) t(v)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(result, ("FF00".into(), "0001".into()));
    // Original spelling survives comparison padding, including an isolated unit.
    let result:(String,String)=session.db.query_row("SELECT hex((__msduck_min_bin2_unicode(v)).__msduck_utf16le),hex((__msduck_max_bin2_unicode(v)).__msduck_utf16le) FROM (VALUES (__msduck_unicode_from_le(from_hex('3ED82000'))),(__msduck_unicode_from_le(from_hex('3ED8'))),(__msduck_unicode_from_le(from_hex('86DD')))) t(v)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(result, ("3ED82000".into(), "86DD".into()));
    let result:(String,String)=session.db.query_row("SELECT hex((__msduck_min_bin2_ansi(v)).__msduck_utf16le),hex((__msduck_min_bin2_unicode(v)).__msduck_utf16le) FROM (VALUES (__msduck_pack_unicode('€')),(__msduck_pack_unicode(' '))) t(v)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(result, ("AC20".into(), "A000".into()));
    for condition in ["true", "false"] {
        let count:i64=session.db.query_row(&format!("SELECT count(v) FROM (SELECT __msduck_min_bin2_unicode(NULL::STRUCT(__msduck_utf16le BLOB)) v WHERE {condition})"),[],|r|r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn bin2_group_window_vectors_and_single_evaluation() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE extrema_calls; SET threads=4")
        .unwrap();
    let count:i64=session.db.query_row("SELECT count(*) FROM (SELECT i%17 g,__msduck_max_bin2_unicode(__msduck_pack_unicode(lpad(CAST(nextval('extrema_calls') AS VARCHAR),6,'0'))) v FROM range(6000) t(i) GROUP BY i%17) WHERE v IS NOT NULL",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 17);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('extrema_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    let wrong:i64=session.db.query_row("SELECT count(*) FROM (SELECT i,__msduck_min_bin2_unicode(__msduck_pack_unicode(CASE WHEN i%3=0 THEN 'Ā' WHEN i%3=1 THEN 'ÿ' ELSE NULL END)) OVER(ORDER BY i ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) v FROM range(6000) t(i)) WHERE hex(v.__msduck_utf16le) <> CASE WHEN i%3=0 THEN '0001' ELSE 'FF00' END",[],|r|r.get(0)).unwrap();
    assert_eq!(wrong, 0);
    let result:String=session.db.query_row("SELECT hex((__msduck_max_bin2_unicode(__msduck_pack_unicode(CAST(i%10 AS VARCHAR)))).__msduck_utf16le) FROM range(100000) t(i)",[],|r|r.get(0)).unwrap();
    assert_eq!(result, "3900");
}

#[test]
fn malformed_inputs_fail_and_repeated_aggregates_release_retained_memory() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    for payload in ["NULL::BLOB", "from_hex('01')"] {
        assert!(
            session
                .db
                .query_row(
                    &format!(
                        "SELECT __msduck_min_bin2_unicode(struct_pack(__msduck_utf16le:={payload}))"
                    ),
                    [],
                    |r| r.get::<_, duckdb::types::Value>(0)
                )
                .is_err()
        );
    }
    assert!(
        session
            .db
            .query_row(
                "SELECT __msduck_min_bin2_ansi(__msduck_unicode_from_le(from_hex('3ED8')))",
                [],
                |r| r.get::<_, duckdb::types::Value>(0)
            )
            .is_err()
    );
    // Distinct groups exceed the shared 64 MiB retained-payload budget. Repeat
    // and then succeed, proving failed execution releases states as well.
    for _ in 0..2 {
        assert!(session.db.query_row("SELECT count(*) FROM (SELECT i,__msduck_min_bin2_unicode(__msduck_pack_unicode(repeat('x',1024))) v FROM range(40000) t(i) GROUP BY i) WHERE v IS NOT NULL",[],|r|r.get::<_,i64>(0)).is_err());
        let count:i64=session.db.query_row("SELECT count(*) FROM (SELECT i,__msduck_min_bin2_unicode(__msduck_pack_unicode(repeat('x',1024))) v FROM range(10000) t(i) GROUP BY i) WHERE v IS NOT NULL",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 10000);
    }
}
