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
