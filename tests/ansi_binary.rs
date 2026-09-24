use msduck::{engine::Session, server::Server};

#[test]
fn public_binary_casts_bind_before_character_cast_translation() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    // Fresh pinned SQL Server capture: 8041, 80, 8041000000, 804120200000.
    let sql = "DECLARE @s NVARCHAR(8)=N'€Ā'; SELECT CONVERT(VARBINARY(MAX),CAST(@s AS VARCHAR(8))) AS raw,CONVERT(VARBINARY(1),CAST(@s AS VARCHAR(8)),0) AS prefix,CAST(CAST(@s AS VARCHAR(8)) AS BINARY(5)) AS padded,CAST(CAST(@s AS CHAR(4)) AS BINARY(6)) AS fixed_source,CAST(CAST(@s AS VARCHAR(8)) AS BINARY) AS default_fixed INTO dbo.ansi_binary_results";
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{tokens:?}");
    let row: (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) = session
        .db
        .query_row(
            "SELECT raw,prefix,padded,fixed_source FROM dbo.ansi_binary_results",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            vec![0x80, 0x41],
            vec![0x80],
            vec![0x80, 0x41, 0, 0, 0],
            vec![0x80, 0x41, 0x20, 0x20, 0, 0]
        )
    );
    let default: Vec<u8> = session
        .db
        .query_row(
            "SELECT default_fixed FROM dbo.ansi_binary_results",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut expected = vec![0x80, 0x41];
    expected.resize(30, 0);
    assert_eq!(default, expected);
}

#[test]
fn ansi_binary_uses_cp1252_bytes_with_bounds_and_nulls() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    for (function, width, wanted) in [
        ("__msduck_ansi_varbinary", -1, vec![0x80, 0xa0, 0xff]),
        ("__msduck_ansi_varbinary", 2, vec![0x80, 0xa0]),
        ("__msduck_ansi_binary", 5, vec![0x80, 0xa0, 0xff, 0, 0]),
    ] {
        let sql = format!("SELECT {function}(__msduck_pack_unicode('€ ÿ'),{width})");
        let value: Vec<u8> = session.db.query_row(&sql, [], |r| r.get(0)).unwrap();
        assert_eq!(value, wanted);
    }
    for sql in [
        "SELECT __msduck_ansi_binary(NULL::STRUCT(__msduck_utf16le BLOB),2)",
        "SELECT __msduck_ansi_varbinary(__msduck_pack_unicode('a'),NULL)",
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
        "SELECT __msduck_ansi_binary(__msduck_pack_unicode('a'),-1)",
        "SELECT __msduck_ansi_varbinary(__msduck_pack_unicode('a'),0)",
        "SELECT __msduck_ansi_varbinary(struct_pack(__msduck_utf16le := from_hex('01')),-1)",
        "SELECT __msduck_ansi_varbinary(struct_pack(__msduck_utf16le := NULL::BLOB),-1)",
    ] {
        assert!(
            session
                .db
                .query_row(sql, [], |r| r.get::<_, Vec<u8>>(0))
                .is_err(),
            "{sql}"
        );
    }
}

#[test]
fn ansi_binary_vector_conversion_evaluates_each_operand_once() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    session
        .db
        .execute_batch("CREATE SEQUENCE ansi_binary_calls")
        .unwrap();
    let wrong:i64=session.db.query_row("SELECT count(*) FROM (SELECT i,__msduck_ansi_binary(__msduck_pack_unicode(CASE WHEN nextval('ansi_binary_calls')%17=0 THEN NULL ELSE '€ ' END),3) b FROM range(6000) t(i)) WHERE b IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE from_hex('80a000') END",[],|r|r.get(0)).unwrap();
    assert_eq!(wrong, 0);
    assert_eq!(
        session
            .db
            .query_row("SELECT currval('ansi_binary_calls')", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        6000
    );
}
