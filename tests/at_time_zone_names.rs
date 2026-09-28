//! SQL-facing checks against the pinned SQL Server named-zone capture.
use msduck::{datetimeoffset::DateTimeOffset, engine::Session, server::Server};

fn first_offset(bytes: &[u8]) -> Option<DateTimeOffset> {
    let row = bytes.iter().position(|byte| *byte == 0xd1)?;
    let len = usize::from(*bytes.get(row + 1)?);
    DateTimeOffset::decode(bytes.get(row + 2..row + 2 + len)?, 7).ok()
}

#[test]
fn captured_names_keep_sql_facing_rows_and_descriptors() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/at-time-zone-names.json")).unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for case in fixture["results"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let reference = &case["reference"];
        let descriptor = &reference["sets"][0]["columns"][0];
        assert_eq!(descriptor["type"], "DateTimeOffset", "{name}");
        assert_eq!(descriptor["scale"], 7, "{name}");
        assert_eq!(descriptor["flags"], 33, "{name}");

        let sql = case["query"].as_str().unwrap();
        let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
        if reference["errors"].as_array().unwrap().is_empty() {
            assert!(ok, "{name}: {bytes:?}");
            assert!(bytes.starts_with(&[0x81]), "{name}: {bytes:?}");
            let flags = u16::from_le_bytes([bytes[7], bytes[8]]);
            assert_eq!(flags, 1, "{name}: reference flags 33; server flags {flags}");
            assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]), "{name}");
            if reference["sets"][0]["rows"][0][0].is_null() {
                assert_eq!(first_offset(&bytes), None, "{name}");
            } else {
                let expected = if name.to_ascii_lowercase().contains("pacific") {
                    "2024-07-01T12:34:56.1234567-07:00"
                } else {
                    "2024-07-01T12:34:56.1234567+00:00"
                };
                assert_eq!(
                    first_offset(&bytes),
                    Some(DateTimeOffset::parse_iso(expected).unwrap()),
                    "{name}"
                );
            }
        } else {
            assert_eq!(reference["errors"][0]["number"], 9820, "{name}");
            assert!(!ok, "{name}: {bytes:?}");
        }
    }
}
