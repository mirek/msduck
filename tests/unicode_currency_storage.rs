//! Currency conversions must read the logical text of Unicode carriers.
use msduck::{engine::Session, server::Server};

#[test]
fn unicode_currency_parsing_retains_rounding_nulls_and_diagnostics() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    let db = &session.db;
    for (input, expected) in [
        ("", "0.0000"),
        ("  $2,000.12555  ", "2000.1256"),
        ("€-12.50", "-12.5000"),
        ("214748.3647", "214748.3647"),
    ] {
        for kind in ["money", "smallmoney", "try_money", "try_smallmoney"] {
            let result: String = db
                .query_row(
                    &format!(
                        "SELECT CAST(__msduck_{kind}_convert(__msduck_pack_unicode(?)) AS VARCHAR)"
                    ),
                    [input],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(result, expected, "{kind}: {input}");
        }
    }
    for (input, diagnostic) in [
        ("bad", "Cannot convert a char value to money"),
        ("922337203685477.5808", "out of range"),
    ] {
        let error = db
            .query_row(
                "SELECT __msduck_money_convert(__msduck_pack_unicode(?))",
                [input],
                |r| r.get::<_, String>(0),
            )
            .unwrap_err();
        // Assert the parser's own diagnostic rather than a backend STRUCT cast.
        let expected = msduck_core::money::parse_text(input).unwrap_err();
        assert!(
            error.to_string().contains(&expected.message),
            "{diagnostic}: {error}"
        );
        let result: Option<String> = db
            .query_row(
                "SELECT CAST(__msduck_try_money_convert(__msduck_pack_unicode(?)) AS VARCHAR)",
                [input],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(result, None);
    }
    for input in [
        "CAST(NULL AS STRUCT(__msduck_utf16le BLOB))",
        "__msduck_unicode_from_le(from_hex('00D8'))",
    ] {
        let result: Option<String> = db
            .query_row(
                &format!("SELECT CAST(__msduck_try_money_convert({input}) AS VARCHAR)"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(result, None);
    }
    let overflow: Option<String> = db.query_row("SELECT CAST(__msduck_try_smallmoney_convert(__msduck_pack_unicode('214748.3648')) AS VARCHAR)", [], |r| r.get(0)).unwrap();
    assert_eq!(overflow, None);
}

#[test]
fn unicode_currency_rejects_malformed_carriers_and_unrelated_structs() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    for input in [
        "struct_pack(__msduck_utf16le := NULL::BLOB)",
        "struct_pack(__msduck_utf16le := from_hex('31'))",
    ] {
        assert!(
            session
                .db
                .prepare(&format!("SELECT __msduck_try_money_convert({input})"))
                .and_then(|mut s| s.query([]).map(|_| ()))
                .is_err(),
            "{input}"
        );
    }
    for input in [
        "struct_pack(other := from_hex('3100'))",
        "struct_pack(__msduck_utf16le := from_hex('3100'), extra := 2)",
        "struct_pack(__msduck_utf16le := '1', extra := 2)",
    ] {
        let ordinary = session.db.query_row(
            &format!("SELECT CAST(__msduck_money_convert({input}) AS VARCHAR)"),
            [],
            |r| r.get::<_, Option<String>>(0),
        );
        assert!(ordinary.is_err(), "unrelated struct accepted: {input}");
        let attempted = session.db.query_row(
            &format!("SELECT CAST(__msduck_try_money_convert({input}) AS VARCHAR)"),
            [],
            |r| r.get::<_, Option<String>>(0),
        );
        // DuckDB can reject an unsupported source at binding, or TRY_CAST can
        // return NULL. Neither may reinterpret an unrelated struct as text.
        assert!(
            matches!(attempted, Err(_) | Ok(None)),
            "{input}: {attempted:?}"
        );
    }
}

#[test]
fn unicode_currency_consumes_each_volatile_operand_once_across_chunks() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    let db = &session.db;
    db.execute_batch("CREATE SEQUENCE currency_units").unwrap();
    let counts: (i64, i64) = db.query_row(
        "SELECT count(n),CAST(sum(n) AS BIGINT) FROM (SELECT __msduck_try_money_convert(__msduck_pack_unicode(CASE WHEN nextval('currency_units')%2=0 THEN '$1.25' ELSE 'bad' END)) n FROM range(6000))",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(counts, (3000, 3750));
    let calls: i64 = db
        .query_row("SELECT currval('currency_units')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(calls, 6000);
}
