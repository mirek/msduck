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
