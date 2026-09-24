use msduck::{engine::Session, server::Server};
#[test]
fn binary_native_conversion_preserves_units_padding_styles_and_nulls() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    for (f, width, style, raw, expected) in [
        ("nvarchar", -1, 0, "410042", "41004200"),
        ("nvarchar", 1, 0, "3dd800de", "3dd8"),
        ("nchar", 3, 0, "00dc", "00dc20002000"),
        ("nvarchar", 5, 1, "abcd01", "3000780041004200"),
        ("nchar", 3, 2, "abcd01", "410042002000"),
    ] {
        let sql = format!(
            "SELECT hex((__msduck_binary_{f}(from_hex('{raw}'),{width},{style},165)).__msduck_utf16le)"
        );
        let actual: String = session.db.query_row(&sql, [], |r| r.get(0)).unwrap();
        assert_eq!(actual.to_lowercase(), expected);
    }
    for sql in [
        "SELECT (__msduck_binary_nvarchar(NULL::BLOB,3,0,165)).__msduck_utf16le",
        "SELECT (__msduck_binary_nvarchar(from_hex('41'),3,NULL,165)).__msduck_utf16le",
        "SELECT (__msduck_try_binary_nvarchar(from_hex('41'),3,3,165)).__msduck_utf16le",
    ] {
        assert_eq!(
            session
                .db
                .query_row(sql, [], |r| r.get::<_, Option<Vec<u8>>>(0))
                .unwrap(),
            None
        );
    }
    for sql in [
        "SELECT hex((__msduck_binary_nvarchar(from_hex('41'),3,3,165)).__msduck_utf16le)",
        "SELECT hex((__msduck_try_binary_nchar(from_hex('41'),-1,0,165)).__msduck_utf16le)",
    ] {
        assert!(
            session
                .db
                .query_row(sql, [], |r| r.get::<_, String>(0))
                .is_err(),
            "{sql}"
        );
    }
}
#[test]
fn binary_native_conversion_evaluates_each_vector_operand_once() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE binary_unicode_calls")
        .unwrap();
    let wrong:i64=session.db.query_row("SELECT count(*) FROM (SELECT i,(__msduck_binary_nvarchar(CASE WHEN nextval('binary_unicode_calls')%17=0 THEN NULL ELSE from_hex('410042') END,-1,0,165)).__msduck_utf16le b FROM range(6000) t(i)) WHERE b IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE from_hex('41004200') END",[],|r|r.get(0)).unwrap();
    assert_eq!(wrong, 0);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('binary_unicode_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
}

#[test]
fn nested_public_casts_keep_each_width_and_padding_step() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let sql = "SELECT CONVERT(VARBINARY(MAX),CAST(CONVERT(NVARCHAR(MAX),0x410042) AS CHAR(3))) a,CONVERT(VARBINARY(MAX),CAST(CONVERT(NVARCHAR(MAX),0x410042) AS NVARCHAR(1))) b,CONVERT(VARBINARY(MAX),CAST(CONVERT(NVARCHAR(MAX),0x410042) AS NCHAR)) c INTO dbo.binary_nested";
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{tokens:?}");
    let actual: (Vec<u8>, Vec<u8>, Vec<u8>) = session
        .db
        .query_row("SELECT a,b,c FROM dbo.binary_nested", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(actual.0, vec![0x41, 0x42, 0x20]);
    assert_eq!(actual.1, vec![0x41, 0]);
    let mut padded = vec![0x41, 0, 0x42, 0];
    padded.extend(std::iter::repeat_n([0x20, 0], 28).flatten());
    assert_eq!(actual.2, padded);
}
