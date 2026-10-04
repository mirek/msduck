//! ISNULL over OPENJSON key and value columns, which are Unicode carriers
//! (issue #900, docs/openjson-isnull.md).
use msduck::{engine::Session, server::Server};

fn session() -> (Server, Session) {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    (server, session)
}

fn batch(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {response:?}");
}

fn rows(session: &Session, width: usize, sql: &str) -> Vec<Vec<Option<String>>> {
    session
        .db
        .prepare(sql)
        .unwrap()
        .query_map([], |row| {
            (0..width)
                .map(|i| row.get::<_, Option<String>>(i))
                .collect::<duckdb::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap()
}

#[test]
fn isnull_packs_text_replacements_for_carriers_and_keeps_code_units() {
    let (_server, session) = session();
    // A carrier first argument takes a text replacement as a carrier; its
    // own code units, including an unpaired surrogate, stay exact.
    let units: Vec<(Option<Vec<u8>>, String)> = session
        .db
        .prepare(
            "SELECT (__msduck_isnull(v, 'é')).__msduck_utf16le, typeof(__msduck_isnull(v, 'é'))
             FROM (VALUES (__msduck_unicode_from_le(from_hex('3ed8'))), (NULL::STRUCT(__msduck_utf16le BLOB))) t(v)",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    let carrier = "STRUCT(__msduck_utf16le BLOB)".to_string();
    assert_eq!(
        units,
        vec![
            (Some(vec![0x3e, 0xd8]), carrier.clone()),
            (Some(vec![0xe9, 0x00]), carrier)
        ]
    );
    // A carrier replacement of text converts through its code units, never
    // its STRUCT display text; other first types keep their behavior.
    assert_eq!(
        rows(
            &session,
            4,
            "SELECT __msduck_isnull(NULL::VARCHAR, __msduck_pack_unicode('🦆x')),
                    __msduck_isnull('a', __msduck_pack_unicode('b')),
                    CAST(__msduck_isnull(NULL::INTEGER, '7') AS VARCHAR),
                    CAST(__msduck_isnull(NULL::DATE, '2024-02-29') AS VARCHAR)"
        ),
        vec![vec![
            Some("🦆x".into()),
            Some("a".into()),
            Some("7".into()),
            Some("2024-02-29".into())
        ]]
    );
}

#[test]
fn isnull_over_openjson_values_reaches_stored_rows() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"DECLARE @d NVARCHAR(MAX) = N'{"a":1,"b":null,"s":"\ud800"}';
        SELECT j.[key] AS k, ISNULL(j.[value], N'') AS v, ISNULL(j.[value], N'-') AS d INTO isnull_rows FROM OPENJSON(@d) j"#,
    );
    let stored: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = session
        .db
        .prepare("SELECT k.__msduck_utf16le, v.__msduck_utf16le, d.__msduck_utf16le FROM isnull_rows ORDER BY k.__msduck_utf16le")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    let bytes = |text: &str| {
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stored,
        vec![
            (bytes("a"), bytes("1"), bytes("1")),
            (bytes("b"), bytes(""), bytes("-")),
            (bytes("s"), vec![0x00, 0xd8], vec![0x00, 0xd8]),
        ]
    );
}

#[test]
fn multirow_trigger_diffs_json_snapshots_through_isnull() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        "CREATE TABLE docs(id INT NOT NULL PRIMARY KEY, name NVARCHAR(20) NULL, qty INT NULL, note NVARCHAR(20) NULL);
        CREATE TABLE audit(id INT NOT NULL, [key] NVARCHAR(4000) NULL, old_value NVARCHAR(MAX) NULL, new_value NVARCHAR(MAX) NULL);
        INSERT INTO docs VALUES (1, N'one', 10, NULL), (2, N'two', 20, N'x'), (3, N'three', 30, N'y');",
    );
    batch(
        &mut session,
        "CREATE FUNCTION dbo.foo(@lhs NVARCHAR(MAX), @rhs NVARCHAR(MAX)) RETURNS TABLE AS RETURN (SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value FROM OPENJSON(@lhs) l FULL OUTER JOIN OPENJSON(@rhs) r ON l.[key] = r.[key])",
    );
    batch(
        &mut session,
        "CREATE TRIGGER docs_audit ON docs AFTER UPDATE AS
        BEGIN
          SET NOCOUNT ON;
          INSERT INTO audit(id, [key], old_value, new_value)
          SELECT i.id, x.[key], x.old_value, x.new_value
          FROM inserted i JOIN deleted d ON d.id = i.id
          CROSS APPLY dbo.foo(
            (SELECT d.name, d.qty, d.note FOR JSON PATH, WITHOUT_ARRAY_WRAPPER),
            (SELECT i.name, i.qty, i.note FOR JSON PATH, WITHOUT_ARRAY_WRAPPER)) x
          WHERE ISNULL(x.old_value, N'') <> ISNULL(x.new_value, N'');
        END",
    );
    batch(
        &mut session,
        "UPDATE docs SET qty = qty + 1, note = CASE id WHEN 2 THEN NULL ELSE N'z' END WHERE id IN (1, 2)",
    );
    assert_eq!(
        rows(
            &session,
            4,
            "SELECT CAST(id AS VARCHAR), __msduck_unicode_text(\"key\"), __msduck_unicode_text(old_value), __msduck_unicode_text(new_value) FROM audit ORDER BY id, 2"
        ),
        [
            ["1", "note", "", "z"],
            ["1", "qty", "10", "11"],
            ["2", "note", "x", ""],
            ["2", "qty", "20", "21"],
        ]
        .map(|row| row.map(|v| (!v.is_empty()).then(|| v.to_string())).to_vec())
        .to_vec()
    );
}

#[test]
fn isnull_widths_convert_carriers_of_aggregated_subqueries() {
    let (_server, mut session) = session();
    // A carrier from an aggregate subquery used to reach the VARCHAR-only
    // NVARCHAR width function and fail to bind; the bounded result is text,
    // so it also compares with literals.
    batch(
        &mut session,
        "CREATE TABLE tn(n NVARCHAR(3) NULL, c NCHAR(3) NULL); INSERT INTO tn VALUES (NULL, NULL), (N'ab', N'ab');
        SELECT N'text' + ISNULL((SELECT MAX(v) FROM (VALUES (N'a'),(N'b')) t(v)), N'') AS s,
               ISNULL(n, N'xyzw') AS a, ISNULL(c, N'q') AS b, N'<' + ISNULL(n, N'') + N'>' AS d
        INTO width_rows FROM tn",
    );
    assert_eq!(
        rows(
            &session,
            4,
            "SELECT __msduck_unicode_text(__msduck_carrier_input(s)), __msduck_unicode_text(__msduck_carrier_input(a)),
                    __msduck_unicode_text(__msduck_carrier_input(b)), __msduck_unicode_text(__msduck_carrier_input(d))
             FROM width_rows ORDER BY 2"
        ),
        [["textb", "ab", "ab ", "<ab>"], ["textb", "xyz", "q  ", "<>"]]
            .map(|row| row.map(|v| Some(v.to_string())).to_vec())
            .to_vec()
    );
    batch(
        &mut session,
        "SELECT CASE WHEN ISNULL((SELECT MAX(v) FROM (VALUES (N'a')) t(v)), N'') = N'a' THEN 1 ELSE 0 END AS hit INTO width_predicate",
    );
    assert_eq!(
        rows(
            &session,
            1,
            "SELECT CAST(hit AS VARCHAR) FROM width_predicate"
        ),
        vec![vec![Some("1".into())]]
    );
    // The carrier overload retains units; decode valid text only for this display assertion.
    assert_eq!(
        rows(
            &session,
            4,
            "SELECT __msduck_isnull_nvarchar_width('a🦆bc', 3),
                    __msduck_unicode_text(__msduck_isnull_nvarchar_width(__msduck_pack_unicode('abcd'), 2)),
                    __msduck_unicode_text(__msduck_isnull_nchar_width(__msduck_pack_unicode('a'), 3)),
                    __msduck_unicode_text(__msduck_isnull_nchar_width(NULL::STRUCT(__msduck_utf16le BLOB), 3))"
        ),
        vec![vec![
            Some("a🦆".into()),
            Some("ab".into()),
            Some("a  ".into()),
            None
        ]]
    );
}

#[test]
fn openjson_isnull_null_fallback_keeps_exact_utf16_storage() {
    let (_server, mut session) = session();
    use msduck::parameter::Parameter;
    use msduck_core::{
        character::{CharacterType, Family, Length},
        types::Type,
        value::Value,
    };
    let parameters = std::collections::HashMap::from([(
        "@fallback".into(),
        Parameter {
            value: Value::Unicode(vec![0xd800]),
            data_type: Type::Character(
                CharacterType::new(Family::Nvarchar, Length::Bounded(1)).unwrap(),
            ),
        },
    )]);
    let sql = r#"SELECT j.[key] AS k, ISNULL(j.[value], @fallback) AS preserved
            INTO isnull_surrogate_fallback
            FROM OPENJSON(N'{"a":null,"b":"\ud800","c":"x"}') j"#;
    let (response, ok) = session.batch_response(sql, &parameters, false, None);
    assert!(ok, "{response:?}");
    let actual: Vec<(Vec<u8>, Vec<u8>)> = session
        .db
        .prepare("SELECT k.__msduck_utf16le, preserved.__msduck_utf16le FROM isnull_surrogate_fallback ORDER BY k.__msduck_utf16le")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        actual,
        vec![
            (vec![b'a', 0], vec![0, 0xd8]),
            (vec![b'b', 0], vec![0, 0xd8]),
            (vec![b'c', 0], vec![b'x', 0]),
        ]
    );
}

#[test]
fn nested_openjson_sources_do_not_poison_outer_null_fallbacks() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let (response, ok) = session.batch_response(
        "CREATE TABLE scoped_values(id INT,value NVARCHAR(10),fallback VARCHAR(10));
         INSERT scoped_values VALUES(1,N'ok','unused'),(2,NULL,'x')",
        &Default::default(),
        false,
        None,
    );
    assert!(ok, "{response:?}");
    for (index, expression) in [
        "COALESCE(value,N'x')",
        "COALESCE(value,fallback)",
        "CASE WHEN value IS NULL THEN N'x' ELSE value END",
        "IIF(value IS NULL,N'x',value)",
    ]
    .iter()
    .enumerate()
    {
        for (source_index, source) in [
            "OPENJSON(N'[1]') j",
            "OPENJSON(N'[1]') WITH(value INT) j",
            "(SELECT j.value FROM OPENJSON(N'[1]') j) d",
        ]
        .iter()
        .enumerate()
        {
            let table = format!("scope_result_{index}_{source_index}");
            let sql = format!(
                "SELECT id,{expression} AS v INTO {table} FROM scoped_values WHERE EXISTS(SELECT 1 FROM {source})"
            );
            let (response, ok) = session.batch_response(&sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {response:?}");
            let values:Vec<String>=session.db.prepare(&format!(
                "SELECT __msduck_carrier_utf8(__msduck_carrier_input(v)) FROM {table} ORDER BY id"))
                .unwrap().query_map([],|r|r.get(0)).unwrap().collect::<duckdb::Result<_>>().unwrap();
            assert_eq!(values, vec!["ok", "x"], "{sql}");
        }
    }
}

#[test]
fn openjson_scope_shadowing_correlation_and_set_branches_stay_distinct() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let setup = "CREATE TABLE scope_outer(value NVARCHAR(10)); INSERT scope_outer VALUES(NULL)";
    assert!(
        session
            .batch_response(setup, &Default::default(), false, None)
            .1
    );
    for (index,sql) in [
        "SELECT COALESCE(value,N'x') AS v FROM scope_outer j WHERE EXISTS(SELECT 1 FROM OPENJSON(N'[1]') WITH(value INT) j WHERE j.value IS NULL)",
        "WITH c AS(SELECT j.value FROM OPENJSON(N'[1]') j) SELECT COALESCE(value,N'x') AS v FROM scope_outer WHERE EXISTS(SELECT 1 FROM c)",
        "SELECT COALESCE(value,N'x') AS v FROM scope_outer UNION ALL SELECT COALESCE(value,N'z') FROM OPENJSON(N'[null]') WITH(value NVARCHAR(10)) j",
        "SELECT ISNULL(j.value,N'x') AS v FROM OPENJSON(N'[null]') j WHERE EXISTS(SELECT 1 WHERE ISNULL(j.value,N'x')=N'x')",
    ].iter().enumerate() {
        let sql=format!("SELECT v INTO scope_control_{index} FROM ({sql}) q");
        let (response,ok)=session.batch_response(&sql,&Default::default(),false,None);
        assert!(ok,"{sql}: {response:?}");
        let values:Vec<String>=session.db.prepare(&format!(
            "SELECT __msduck_carrier_utf8(__msduck_carrier_input(v)) FROM scope_control_{index} ORDER BY v"))
            .unwrap().query_map([],|r|r.get(0)).unwrap().collect::<duckdb::Result<_>>().unwrap();
        assert_eq!(values,if index==2 {vec!["x","z"]} else {vec!["x"]},"{sql}");
    }
}

#[test]
fn scoped_set_pinning_keeps_carrier_and_text_branches_bindable() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE scope_set(value NVARCHAR(10)); INSERT scope_set VALUES(N'a')",
        "SELECT value AS v INTO scoped_union FROM scope_set UNION ALL SELECT N'b'",
    ] {
        let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {response:?}");
    }
    let values: Vec<String> = session
        .db
        .prepare(
            "SELECT __msduck_carrier_utf8(__msduck_carrier_input(v)) FROM scoped_union ORDER BY v",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(values, vec!["a", "b"]);
}

#[test]
fn cte_wrapped_writes_keep_their_carrier_targets_visible() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE scope_writes(id INT,nv NVARCHAR(10)); INSERT scope_writes VALUES(1,NULL),(2,NULL),(3,NULL)",
        "WITH c AS(SELECT 1 AS id) UPDATE scope_writes SET nv=COALESCE(scope_writes.nv,N'x') WHERE id IN(SELECT id FROM c)",
        "WITH c AS(SELECT 2 AS id) DELETE scope_writes WHERE id IN(SELECT id FROM c) AND COALESCE(scope_writes.nv,N'x')=N'x'",
        "WITH c AS(SELECT 3 AS id) MERGE scope_writes AS t USING c AS s ON t.id=s.id WHEN MATCHED THEN UPDATE SET nv=COALESCE(t.nv,N'z');",
    ] {
        let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {response:?}");
    }
    let rows: Vec<(i32, String)> = session
        .db
        .prepare("SELECT id,__msduck_carrier_utf8(nv) FROM scope_writes ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![(1, "x".into()), (3, "z".into())]);
}

#[test]
fn explicit_peer_names_do_not_hide_unqualified_write_targets() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE scope_peers(id INT,nv NVARCHAR(10)); INSERT scope_peers VALUES(1,NULL),(2,NULL),(3,NULL)",
        "WITH c AS(SELECT 1 AS id) UPDATE scope_peers SET nv=COALESCE(nv,N'x') FROM c WHERE scope_peers.id=c.id",
        "UPDATE scope_peers SET nv=COALESCE(nv,N'y') FROM(SELECT 2 AS id) d WHERE scope_peers.id=d.id",
        "WITH c(id) AS(SELECT 3) UPDATE t SET nv=COALESCE(t.nv,N'a') FROM scope_peers AS t JOIN c ON t.id=c.id",
    ] {
        let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {response:?}");
    }
    let rows: Vec<(i32, String)> = session
        .db
        .prepare("SELECT id,__msduck_carrier_utf8(nv) FROM scope_peers ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![(1, "x".into()), (2, "y".into()), (3, "a".into())]
    );
    // A peer that actually has nv still makes a bare nv ambiguous; no type
    // is guessed from either source and neither target row may change.
    let(_,ok)=session.batch_response(
        "WITH c AS(SELECT 1 AS id,N'peer' AS nv) UPDATE scope_peers SET nv=COALESCE(nv,N'z') FROM c WHERE scope_peers.id=c.id",
        &Default::default(),false,None);
    assert!(!ok);
    let value: String = session
        .db
        .query_row(
            "SELECT __msduck_carrier_utf8(nv) FROM scope_peers WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, "x");
}

#[test]
fn write_target_aliases_ignore_unrelated_same_named_tables() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "CREATE TABLE t(nv VARCHAR(10)); INSERT t VALUES('unrelated'); CREATE TABLE alias_writes(id INT,nv NVARCHAR(10)); INSERT alias_writes VALUES(1,NULL),(2,NULL),(3,NULL),(4,NULL)",
        "UPDATE t SET nv=COALESCE(t.nv,N'x') FROM alias_writes AS t WHERE t.id=1",
        "WITH c(id) AS(SELECT 2) UPDATE t SET nv=COALESCE(t.nv,N'y') FROM alias_writes AS t JOIN c ON t.id=c.id",
        "DELETE t FROM alias_writes AS t WHERE t.id=3 AND COALESCE(t.nv,N'z')=N'z'",
        "WITH c(id) AS(SELECT 4) DELETE t FROM alias_writes AS t JOIN c ON t.id=c.id WHERE COALESCE(t.nv,N'z')=N'z'",
    ] {
        let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {response:?}");
    }
    let rows: Vec<(i32, String)> = session
        .db
        .prepare("SELECT id,__msduck_carrier_utf8(nv) FROM alias_writes ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![(1, "x".into()), (2, "y".into())]);
    let unrelated: String = session
        .db
        .query_row("SELECT nv FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(unrelated, "unrelated");
}

#[test]
fn openjson_character_alternatives_preserve_raw_units_and_null_fallbacks() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        "CREATE TABLE alternative_peer(fallback VARCHAR(3)); INSERT alternative_peer VALUES('z')",
    );
    for (index, expression) in [
        "COALESCE(j.[value],N'z')",
        "CASE WHEN j.[value] IS NULL THEN N'z' ELSE j.[value] END",
        "IIF(j.[value] IS NULL,N'z',j.[value])",
        "COALESCE(j.[value],@replacement)",
        "COALESCE(j.[value],p.fallback)",
    ]
    .into_iter()
    .enumerate()
    {
        batch(
            &mut session,
            &format!(
                "DECLARE @replacement NVARCHAR(3)=N'z'; CREATE TABLE alternatives_{index}(k NVARCHAR(10),v NVARCHAR(MAX)); INSERT alternatives_{index} SELECT j.[key],{expression} FROM OPENJSON(N'{{\"a\":\"\\ud800\",\"b\":null,\"c\":\"\\udc00\"}}') j CROSS JOIN alternative_peer p"
            ),
        );
        let units: Vec<Vec<u8>> = session.db.prepare(&format!(
            "SELECT v.__msduck_utf16le FROM alternatives_{index} ORDER BY __msduck_carrier_utf8(k)"
        )).unwrap().query_map([], |r| r.get(0)).unwrap().collect::<duckdb::Result<_>>().unwrap();
        assert_eq!(
            units,
            vec![vec![0, 0xd8], vec![b'z', 0], vec![0, 0xdc]],
            "{expression}"
        );
    }
}

#[test]
fn openjson_character_alternatives_keep_common_widths_and_fixed_padding() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE fixed_alternatives(id INT,v NVARCHAR(MAX));
        INSERT fixed_alternatives SELECT j.id,COALESCE(j.v,CAST(N'q' AS NCHAR(5)))
        FROM OPENJSON(N'[{"id":1,"v":"\ud800"},{"id":2,"v":null}]') WITH(id INT,v NCHAR(3)) j;
        CREATE TABLE bounded_alternatives(id INT,v NVARCHAR(MAX));
        INSERT bounded_alternatives SELECT j.id,COALESCE(j.v,CAST(N'abcde' AS NVARCHAR(5)))
        FROM OPENJSON(N'[{"id":1,"v":"\ud800xy"},{"id":2,"v":null}]') WITH(id INT,v NVARCHAR(2)) j;"#,
    );
    let payloads = |table: &str| -> Vec<Vec<u8>> {
        session
            .db
            .prepare(&format!(
                "SELECT v.__msduck_utf16le FROM {table} ORDER BY id"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap()
    };
    assert_eq!(
        payloads("fixed_alternatives"),
        vec![
            vec![0, 0xd8, b' ', 0, b' ', 0, b' ', 0, b' ', 0],
            vec![b'q', 0, b' ', 0, b' ', 0, b' ', 0, b' ', 0],
        ]
    );
    assert_eq!(
        payloads("bounded_alternatives"),
        vec![
            vec![0, 0xd8, b'x', 0],
            vec![b'a', 0, b'b', 0, b'c', 0, b'd', 0, b'e', 0],
        ]
    );
}

#[test]
fn openjson_character_alternatives_evaluate_selected_volatile_leaves_once() {
    let (_server, mut session) = session();
    session
        .db
        .execute_batch("CREATE SEQUENCE alternative_counter START 10")
        .unwrap();
    batch(
        &mut session,
        r#"CREATE TABLE lazy_alternatives(k NVARCHAR(10),v NVARCHAR(MAX));
        INSERT lazy_alternatives SELECT j.[key],COALESCE(j.[value],CAST(nextval('alternative_counter') AS NVARCHAR(10)))
        FROM OPENJSON(N'{"a":"\ud800","b":null,"c":"\udc00"}') j;"#,
    );
    let counter: i64 = session
        .db
        .query_row(
            "SELECT last_value FROM duckdb_sequences() WHERE sequence_name='alternative_counter'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(counter, 10);
    let units: Vec<Vec<u8>> = session
        .db
        .prepare(
            "SELECT v.__msduck_utf16le FROM lazy_alternatives ORDER BY __msduck_carrier_utf8(k)",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(
        units,
        vec![vec![0, 0xd8], vec![b'1', 0, b'0', 0], vec![0, 0xdc]]
    );
    batch(
        &mut session,
        r#"CREATE TABLE first_alternatives(v NVARCHAR(MAX));
        INSERT first_alternatives SELECT COALESCE(CAST(nextval('alternative_counter') AS NVARCHAR(10)),j.[value])
        FROM OPENJSON(N'{"a":"\ud800","b":null,"c":"\udc00"}') j;"#,
    );
    let counter: i64 = session
        .db
        .query_row(
            "SELECT last_value FROM duckdb_sequences() WHERE sequence_name='alternative_counter'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(counter, 13);
}

#[test]
fn openjson_fixed_carrier_peers_keep_common_padding() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE fixed_peers(id INT,c NVARCHAR(MAX),d NVARCHAR(MAX));
        INSERT fixed_peers SELECT j.id,CASE WHEN j.id=1 THEN j.a ELSE j.b END,COALESCE(j.a,j.b)
        FROM OPENJSON(N'[{"id":1,"a":"\ud800bc","b":"uvwxy"},{"id":2,"a":null,"b":"uvwxy"}]')
        WITH(id INT,a NCHAR(3),b NCHAR(5)) j;"#,
    );
    let values: Vec<(Vec<u8>, Vec<u8>)> = session
        .db
        .prepare("SELECT c.__msduck_utf16le,d.__msduck_utf16le FROM fixed_peers ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    let first = vec![0, 0xd8, b'b', 0, b'c', 0, b' ', 0, b' ', 0];
    let second = vec![b'u', 0, b'v', 0, b'w', 0, b'x', 0, b'y', 0];
    assert_eq!(
        values,
        vec![(first.clone(), first), (second.clone(), second)]
    );
}

#[test]
fn openjson_ansi_peer_alternatives_align_with_text_set_branches() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE set_peer(v VARCHAR(5)); INSERT set_peer VALUES('z');
        CREATE TABLE mixed_set_result(v NVARCHAR(MAX));
        INSERT mixed_set_result SELECT COALESCE(j.[value],p.v) FROM OPENJSON(N'{"a":"\ud800","b":null}') j CROSS JOIN set_peer p
        UNION ALL SELECT v FROM set_peer;"#,
    );
    let mut units: Vec<Vec<u8>> = session
        .db
        .prepare("SELECT v.__msduck_utf16le FROM mixed_set_result")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    units.sort();
    assert_eq!(units, vec![vec![0, 0xd8], vec![b'z', 0], vec![b'z', 0]]);
}

#[test]
fn openjson_character_alternatives_keep_numeric_set_precedence() {
    let (_server, mut session) = session();
    for kind in ["INT", "SMALLINT", "DECIMAL(5,2)"] {
        batch(
            &mut session,
            &format!(
                r#"CREATE TABLE numeric_set_{}(v {});
            INSERT numeric_set_{} SELECT COALESCE(j.[value],N'2') FROM OPENJSON(N'{{"a":"1","b":null}}') j
            UNION ALL SELECT CAST(7 AS {});"#,
                kind.split('(').next().unwrap(),
                kind,
                kind.split('(').next().unwrap(),
                kind
            ),
        );
        let values: Vec<i32> = session
            .db
            .prepare(&format!(
                "SELECT CAST(v AS INTEGER) FROM numeric_set_{} ORDER BY v",
                kind.split('(').next().unwrap()
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        assert_eq!(values, vec![1, 2, 7], "{kind}");
    }
}

#[test]
fn nested_openjson_alternatives_keep_carriers_and_ansi_best_fit() {
    let (_server, mut session) = session();
    for (index, expression) in [
        "COALESCE(COALESCE(j.[value],N'x'),N'y')",
        "CASE WHEN j.[value] IS NULL THEN COALESCE(j.[value],N'x') ELSE N'y' END",
        "COALESCE(IIF(j.[value] IS NULL,N'x',j.[value]),N'y')",
        "COALESCE(j.[value],'漢')",
    ]
    .iter()
    .enumerate()
    {
        batch(
            &mut session,
            &format!(
                r#"CREATE TABLE nested_alt_{index}(v NVARCHAR(MAX)); INSERT nested_alt_{index} SELECT {expression} FROM OPENJSON(N'{{"a":null}}') j;"#
            ),
        );
        let units: Vec<u8> = session
            .db
            .query_row(
                &format!("SELECT v.__msduck_utf16le FROM nested_alt_{index}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            units,
            vec![if index == 3 { b'?' } else { b'x' }, 0],
            "{expression}"
        );
    }
}

#[test]
fn openjson_distinct_sets_use_character_keys_and_null_equality() {
    let (_server, mut session) = session();
    for (index, op) in ["UNION", "INTERSECT", "EXCEPT"].iter().enumerate() {
        batch(
            &mut session,
            &format!(
                r#"CREATE TABLE distinct_set_{index}(v NVARCHAR(MAX)); INSERT distinct_set_{index}
            SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{{"a":null}}') j {op} SELECT N'x ';"#
            ),
        );
        let count: i64 = session
            .db
            .query_row(
                &format!("SELECT count(*) FROM distinct_set_{index}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, if *op == "EXCEPT" { 0 } else { 1 }, "{op}");
        batch(
            &mut session,
            &format!(
                r#"CREATE TABLE null_set_{index}(v NVARCHAR(MAX)); INSERT null_set_{index}
            SELECT COALESCE(j.[value],CAST(NULL AS NVARCHAR(10))) AS v FROM OPENJSON(N'{{"a":null}}') j {op} SELECT CAST(NULL AS NVARCHAR(10));"#
            ),
        );
        let count: i64 = session
            .db
            .query_row(
                &format!("SELECT count(*) FROM null_set_{index} WHERE v IS NULL"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, if *op == "EXCEPT" { 0 } else { 1 }, "NULL {op}");
    }
    session
        .db
        .execute_batch("CREATE SEQUENCE distinct_calls START 10")
        .unwrap();
    batch(
        &mut session,
        r#"CREATE TABLE distinct_effect(v NVARCHAR(MAX)); INSERT distinct_effect
        SELECT COALESCE(j.[value],CAST(nextval('distinct_calls') AS NVARCHAR(10))) AS v FROM OPENJSON(N'{"a":null}') j
        UNION SELECT N'10 ';"#,
    );
    let count: i64 = session
        .db
        .query_row("SELECT count(*) FROM distinct_effect", [], |r| r.get(0))
        .unwrap();
    let calls: i64 = session
        .db
        .query_row(
            "SELECT last_value FROM duckdb_sequences() WHERE sequence_name='distinct_calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((count, calls), (1, 10));
}

#[test]
fn openjson_ansi_supplementary_fallback_counts_best_fit_bytes() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE ansi_units(v NVARCHAR(MAX)); INSERT ansi_units
        SELECT COALESCE(j.v,'🦆') FROM OPENJSON(N'{"v":null}') WITH(v NVARCHAR(1)) j;"#,
    );
    let units: Vec<u8> = session
        .db
        .query_row("SELECT v.__msduck_utf16le FROM ansi_units", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(units, vec![b'?', 0, b'?', 0]);
}

#[test]
fn nested_openjson_alternatives_preserve_isolated_units() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE nested_raw(v NVARCHAR(MAX)); INSERT nested_raw
        SELECT COALESCE(COALESCE(j.[value],N'x'),N'y') FROM OPENJSON(N'{"a":"\ud800"}') j;"#,
    );
    let units: Vec<u8> = session
        .db
        .query_row("SELECT v.__msduck_utf16le FROM nested_raw", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(units, vec![0, 0xd8]);
}

#[test]
fn openjson_set_wrappers_preserve_user_ctes_and_nested_distinct_operators() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE hygienic_set(v NVARCHAR(MAX)); WITH __variant_left_input(v) AS(SELECT N'b')
        INSERT hygienic_set SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{"a":"a"}') j
        INTERSECT SELECT CAST(v AS NVARCHAR(10)) FROM __variant_left_input;
        CREATE TABLE chained_set(v NVARCHAR(MAX)); INSERT chained_set
        SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{"a":null}') j UNION SELECT N'x ' UNION ALL SELECT N'z';
        CREATE TABLE parenthesized_ansi(v NVARCHAR(MAX)); INSERT parenthesized_ansi
        SELECT COALESCE(j.[value],('🦆')) FROM OPENJSON(N'{"a":null}') j;"#,
    );
    let count: i64 = session
        .db
        .query_row("SELECT count(*) FROM hygienic_set", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = session
        .db
        .query_row("SELECT count(*) FROM chained_set", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let units: Vec<u8> = session
        .db
        .query_row(
            "SELECT v.__msduck_utf16le FROM parenthesized_ansi",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(units, vec![b'?', 0, b'?', 0]);
}

#[test]
fn nested_character_distinct_finishes_before_parent_numeric_conversion() {
    let (_server, mut session) = session();
    for (index,source) in [
        r#"SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{"a":"01"}') j UNION SELECT N'1'"#,
        r#"(SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{"a":"01"}') j UNION SELECT N'1')"#,
    ].iter().enumerate() {
        batch(&mut session,&format!("CREATE TABLE boundary_{index}(v INT); INSERT boundary_{index} {source} UNION ALL SELECT 7;"));
        let rows: Vec<i32> = session.db.prepare(&format!("SELECT v FROM boundary_{index} ORDER BY v")).unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<duckdb::Result<_>>().unwrap();
        assert_eq!(rows,vec![1,1,7]);
    }
}

#[test]
fn character_union_retains_the_first_input_representative() {
    let (_server, mut session) = session();
    for (index, (first, second)) in [("x", "X"), ("X", "x"), ("x ", "x")].iter().enumerate() {
        batch(
            &mut session,
            &format!(
                r#"CREATE TABLE representative_{index}(v NVARCHAR(MAX)); INSERT representative_{index}
            SELECT COALESCE(j.[value],N'z') AS v FROM OPENJSON(N'{{"a":"{first}"}}') j UNION SELECT N'{second}';"#
            ),
        );
        let rows: Vec<Vec<u8>> = session
            .db
            .prepare(&format!(
                "SELECT v.__msduck_utf16le FROM representative_{index}"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        let expected = first
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![expected]);
    }
}

#[test]
fn ansi_set_children_convert_before_unicode_promotion() {
    let (_server, mut session) = session();
    for (index, (source, expected)) in [
        (r#"SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{"a":"y"}') j UNION ALL (SELECT CAST('A' AS VARCHAR(2)) UNION SELECT CAST('A ' AS VARCHAR(4)))"#, vec!["y", "A"]),
        (r#"SELECT COALESCE(j.[value],N'x') AS v FROM OPENJSON(N'{"a":"y"}') j UNION ALL SELECT '漢'"#, vec!["y", "?"]),
        (r#"SELECT COALESCE(j.v,N'x') AS v FROM OPENJSON(N'{"v":"y"}') WITH(v NVARCHAR(1)) j UNION ALL SELECT '🦆'"#, vec!["y", "??"]),
    ].iter().enumerate() {
        batch(&mut session, &format!("CREATE TABLE ansi_boundary_{index}(v NVARCHAR(MAX)); INSERT ansi_boundary_{index} {source};"));
        let mut rows: Vec<Vec<u8>> = session.db.prepare(&format!("SELECT v.__msduck_utf16le FROM ansi_boundary_{index}"))
            .unwrap().query_map([], |r| r.get(0)).unwrap().collect::<duckdb::Result<_>>().unwrap();
        let mut expected = expected.iter().map(|value| value.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()).collect::<Vec<_>>();
        rows.sort(); expected.sort(); assert_eq!(rows, expected);
    }
}

#[test]
fn projection_aliases_do_not_shadow_openjson_source_columns() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"CREATE TABLE source_aliases(v NVARCHAR(MAX), k NVARCHAR(4000));
        INSERT source_aliases SELECT COALESCE(value,N'x') AS value,COALESCE([key],N'z') AS [key]
        FROM OPENJSON(N'{"a":"\ud800","b":null}') ORDER BY [key];"#,
    );
    let rows: Vec<(Vec<u8>, Vec<u8>)> = session.db.prepare("SELECT k.__msduck_utf16le, v.__msduck_utf16le FROM source_aliases ORDER BY k.__msduck_utf16le")
        .unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<duckdb::Result<_>>().unwrap();
    assert_eq!(
        rows,
        vec![
            (vec![b'a', 0], vec![0, 0xd8]),
            (vec![b'b', 0], vec![b'x', 0])
        ]
    );
    batch(
        &mut session,
        "INSERT source_aliases(v) SELECT COALESCE(value,N'x') AS value FROM OPENJSON(N'[null]');",
    );
    let count: i64 = session
        .db
        .query_row("SELECT count(*) FROM source_aliases", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn conditional_predicate_collation_does_not_change_set_result_equality() {
    let (_server, mut session) = session();
    for (index, expression) in [
        "CASE WHEN N'a' COLLATE Latin1_General_100_BIN2 = N'a' THEN j.[value] ELSE N'x' END",
        "IIF(N'a' COLLATE Latin1_General_100_BIN2 = N'a',j.[value],N'x')",
    ]
    .iter()
    .enumerate()
    {
        batch(
            &mut session,
            &format!(
                r#"CREATE TABLE predicate_domain_{index}(v NVARCHAR(MAX)); INSERT predicate_domain_{index} SELECT {expression} AS v FROM OPENJSON(N'{{"a":"x"}}') j UNION SELECT N'x ';"#
            ),
        );
        let rows: Vec<Vec<u8>> = session
            .db
            .prepare(&format!(
                "SELECT v.__msduck_utf16le FROM predicate_domain_{index}"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![vec![b'x', 0]]);
    }
}

#[test]
fn query_ordering_alternatives_retain_select_carrier_scope() {
    let (_server, mut session) = session();
    for (index, expression) in [
        "COALESCE(j.[value],N'x')",
        "CASE WHEN j.[value] IS NULL THEN N'x' ELSE j.[value] END",
        "IIF(j.[value] IS NULL,N'x',j.[value])",
    ]
    .iter()
    .enumerate()
    {
        batch(
            &mut session,
            &format!(
                "CREATE TABLE ordered_null_{index}(v NVARCHAR(MAX)); INSERT ordered_null_{index} SELECT j.[value] FROM OPENJSON(N'[null]') j ORDER BY {expression};"
            ),
        );
        let count: i64 = session
            .db
            .query_row(
                &format!("SELECT count(*) FROM ordered_null_{index} WHERE v IS NULL"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn order_predicates_use_select_sources_and_declared_parameter_peers() {
    let (_server, mut session) = session();
    batch(
        &mut session,
        r#"DECLARE @p NVARCHAR(2)=N'x '; CREATE TABLE order_predicate(v NVARCHAR(MAX)); INSERT order_predicate SELECT j.value FROM OPENJSON(N'["z","x"]') j ORDER BY CASE WHEN j.value=@p THEN 0 ELSE 1 END;"#,
    );
    let rows: Vec<Vec<u8>> = session
        .db
        .prepare("SELECT v.__msduck_utf16le FROM order_predicate")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![vec![b'x', 0], vec![b'z', 0]]);
}

#[test]
fn direct_openjson_set_branches_preserve_units_and_binary_type_boundaries() {
    let (_server, mut session) = session();
    for (index, source) in [
        r#"SELECT j.value AS v FROM OPENJSON(N'["\ud800"]') j UNION ALL SELECT N'x'"#,
        r#"SELECT CAST(j.value AS NVARCHAR(2)) AS v FROM OPENJSON(N'["\ud800"]') j UNION ALL SELECT N'x'"#,
    ].iter().enumerate() {
        batch(&mut session, &format!("CREATE TABLE direct_set_{index}(v NVARCHAR(MAX)); INSERT direct_set_{index} {source}"));
        let rows:Vec<Vec<u8>> = session.db.prepare(&format!("SELECT v.__msduck_utf16le FROM direct_set_{index}")).unwrap().query_map([],|r|r.get(0)).unwrap().collect::<duckdb::Result<_>>().unwrap();
        assert_eq!(rows, vec![vec![0,0xd8],vec![b'x',0]], "{index}: {source}");
    }
    batch(
        &mut session,
        r#"CREATE TABLE direct_numeric_boundary(v INT); INSERT direct_numeric_boundary (SELECT j.value AS v FROM OPENJSON(N'["01","1"]') j UNION SELECT N'1') UNION ALL SELECT 7"#,
    );
    let rows: Vec<i32> = session
        .db
        .prepare("SELECT v FROM direct_numeric_boundary")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap();
    assert_eq!(rows, vec![1, 1, 7]);
}
