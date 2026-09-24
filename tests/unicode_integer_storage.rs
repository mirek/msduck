use msduck::{engine::Session, server::Server};

#[test]
fn unicode_integer_vectors_evaluate_once_and_preserve_nulls() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE integer_calls")
        .unwrap();
    let count: i64 = session.db.query_row("SELECT count(*) FROM range(6000) WHERE CAST(__msduck_integer_input(__msduck_pack_unicode(CAST(nextval('integer_calls') AS VARCHAR)),'INT',false) AS INTEGER)>0",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 6000);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('integer_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
    let value: Option<String> = session
        .db
        .query_row(
            "SELECT __msduck_integer_input(NULL::STRUCT(__msduck_utf16le BLOB),'INT',false)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, None);
    for payload in ["NULL::BLOB", "from_hex('37')"] {
        for mode in [false, true] {
            assert!(session.db.query_row(&format!("SELECT __msduck_integer_input(struct_pack(__msduck_utf16le:={payload}),'INT',{mode})"),[],|r|r.get::<_,Option<String>>(0)).is_err());
        }
    }
    assert_eq!(session.db.query_row("SELECT CAST(__msduck_integer_input(__msduck_pack_unicode(' + 007 '),'INT',false) AS INTEGER)",[],|r|r.get::<_,i32>(0)).unwrap(),7);
}

#[test]
fn json_value_arithmetic_and_explicit_schema_integer_consumers() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE integer_json(j NVARCHAR(MAX)); INSERT INTO integer_json VALUES(N'{\"n\":\"7\"}')",
        "SELECT JSON_VALUE(j,'$.n')+1 AS n INTO integer_json_result FROM integer_json",
        "SELECT n INTO integer_schema_result FROM OPENJSON(N'[{\"n\":\"8\"}]') WITH(n INT)",
    ] {
        let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {bytes:?}");
    }
    for table in ["integer_json_result", "integer_schema_result"] {
        assert_eq!(
            session
                .db
                .query_row(&format!("SELECT n FROM {table}"), [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            8
        );
    }
}

#[test]
fn integer_dispatch_keeps_unselected_variant_branches_bindable() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    let value: String = session
        .db
        .query_row(
            "SELECT __msduck_explicit_integer_input(__msduck_identity_variant(56,42),'INT',false)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, "42");
    assert!(
        session
            .db
            .query_row("SELECT __msduck_integer_unicode(42,'INT',false)", [], |r| r
                .get::<_, String>(0))
            .is_err()
    );
}
