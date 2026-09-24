//! Native boundaries for JSON operations on stored UTF-16 code units.
use msduck::{engine::Session, server::Server};

#[test]
fn isjson_accepts_exact_stored_units_and_preserves_constraints() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE json_units(id INT,j NVARCHAR(MAX) CHECK(ISJSON(j)=1))",
        "INSERT INTO json_units VALUES(1,N'{\"s\":\"'+LEFT(N'🦆',1)+N'\"}'),(2,N'{\"s\":\"'+RIGHT(N'🦆',1)+N'\"}'),(3,NULL)",
        "SELECT id,ISJSON(j) AS valid,ISJSON(j,OBJECT) AS object_type INTO json_validation FROM json_units",
    ] {
        let (_, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}");
    }
    let values: Vec<(i32, Option<i32>, Option<i32>)> = session
        .db
        .prepare("SELECT * FROM json_validation ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        values,
        vec![
            (1, Some(1), Some(1)),
            (2, Some(1), Some(1)),
            (3, None, None)
        ]
    );
    let (_, ok) = session.batch_response(
        "INSERT INTO json_units VALUES(4,N'{}'),(5,N'{bad}')",
        &Default::default(),
        false,
        None,
    );
    assert!(!ok);
    assert_eq!(
        session
            .db
            .query_row("SELECT count(*) FROM json_units", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[test]
fn isjson_unicode_vectors_evaluate_once_and_reject_malformed_storage() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE json_unicode_calls")
        .unwrap();
    let count: i64 = session.db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_isjson_0(__msduck_pack_unicode(printf('[%d]',nextval('json_unicode_calls'))))=1", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 6000);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('json_unicode_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    for payload in ["NULL::BLOB", "from_hex('7b')"] {
        assert!(
            session
                .db
                .query_row(
                    &format!(
                        "SELECT __msduck_isjson_0(struct_pack(__msduck_utf16le := {payload}))"
                    ),
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .is_err()
        );
    }
}

#[test]
fn string_escape_preserves_raw_surrogates_and_control_expansion() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (_, ok) = session.batch_response(
        "CREATE TABLE json_escape_units(s NVARCHAR(MAX)); INSERT INTO json_escape_units VALUES(LEFT(N'🦆',1)+NCHAR(0)+N'/'+RIGHT(N'🦆',1)),(NULL),(N''); SELECT STRING_ESCAPE(s,N'json') AS escaped INTO json_escape_results FROM json_escape_units",
        &Default::default(), false, None,
    );
    assert!(ok);
    let rows: Vec<Option<Vec<u8>>> = session
        .db
        .prepare("SELECT escaped.__msduck_utf16le FROM json_escape_results")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    let units: [u16; 10] = [0xd83e, 92, 117, 48, 48, 48, 48, 92, 47, 0xdd86];
    assert_eq!(
        rows,
        vec![
            Some(units.iter().flat_map(|u| u.to_le_bytes()).collect()),
            None,
            Some(vec![])
        ]
    );
    session
        .db
        .execute_batch("CREATE SEQUENCE escape_unicode_calls")
        .unwrap();
    let calls: (i64,i64) = session.db.query_row("SELECT count(*),CAST(sum(octet_length((__msduck_string_escape(__msduck_pack_unicode(CASE WHEN nextval('escape_unicode_calls')%2=0 THEN '/' ELSE '' END),__msduck_pack_unicode('json'))).__msduck_utf16le)) AS BIGINT) FROM range(6000) t(i)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(calls, (6000, 12000));
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('escape_unicode_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    let error = session.db.query_row("SELECT __msduck_string_escape(__msduck_pack_unicode(repeat(chr(0),1500000)),__msduck_pack_unicode('json'))", [], |r| r.get::<_,String>(0)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("STRING_ESCAPE result exceeds the configured output limit"),
        "{error}"
    );
}

#[test]
fn extraction_preserves_surrogates_and_original_container_spelling() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE extraction_source(id INT,j NVARCHAR(MAX))",
        r#"INSERT INTO extraction_source VALUES(1,N'{"s":"'+LEFT(N'🦆',1)+N'","a":["'+LEFT(N'🦆',1)+N'"]}'),(2,N'{"s":"\ud800","a":["\ud800"]}')"#,
        "SELECT id,JSON_VALUE(j,'$.s') AS s,JSON_QUERY(j,'$.a') AS a,JSON_PATH_EXISTS(j,'$.a[*]') AS present INTO extraction_result FROM extraction_source",
    ] {
        let (_, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}");
    }
    let rows: Vec<(String,String,i32)> = session.db.prepare("SELECT hex(s.__msduck_utf16le),hex(a.__msduck_utf16le),present FROM extraction_result ORDER BY id").unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<duckdb::Result<_>>().unwrap();
    assert_eq!(
        rows,
        vec![
            ("3ED8".into(), "5B0022003ED822005D00".into(), 1),
            (
                "00D8".into(),
                "5B0022005C007500640038003000300022005D00".into(),
                1
            )
        ]
    );
    let (_, ok) = session.batch_response(r#"CREATE TABLE extraction_keys(j NVARCHAR(MAX),p NVARCHAR(20)); INSERT INTO extraction_keys VALUES(N'{"'+LEFT(N'🦆',1)+N'":7}',N'$."'+LEFT(N'🦆',1)+N'"'); SELECT JSON_VALUE(j,p) AS v,JSON_PATH_EXISTS(j,p) AS present INTO extraction_key_result FROM extraction_keys"#, &Default::default(), false, None);
    assert!(ok);
    let row: (String, i32) = session
        .db
        .query_row(
            "SELECT hex(v.__msduck_utf16le),present FROM extraction_key_result",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("3700".into(), 1));
}

#[test]
fn extraction_unicode_inputs_evaluate_once_and_validate_storage() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE extract_source; CREATE SEQUENCE extract_path")
        .unwrap();
    let count: i64 = session.db.query_row(r#"SELECT count(*) FROM range(6000) WHERE octet_length((__msduck_json_value(__msduck_pack_unicode(printf('{"x":%d}',nextval('extract_source'))),__msduck_pack_unicode(CASE WHEN nextval('extract_path')>0 THEN '$.x' ELSE '$' END))).__msduck_utf16le)>0"#,[],|r|r.get(0)).unwrap();
    assert_eq!(count, 6000);
    for name in ["extract_source", "extract_path"] {
        assert_eq!(
            session
                .db
                .query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
    for function in [
        "__msduck_json_value",
        "__msduck_json_query",
        "__msduck_json_path_exists",
    ] {
        for payload in ["NULL::BLOB", "from_hex('7b')"] {
            assert!(session.db.prepare(&format!("SELECT {function}(struct_pack(__msduck_utf16le := {payload}),__msduck_pack_unicode('$'))")).and_then(|mut s|s.query([]).map(|_|())).is_err());
        }
    }
}
