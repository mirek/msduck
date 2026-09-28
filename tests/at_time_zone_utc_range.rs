//! SQL-facing boundary checks against the pinned SQL Server UTC capture.
use msduck::{datetimeoffset::DateTimeOffset, engine::Session, server::Server};

fn first_offset(bytes: &[u8], scale: u8) -> Option<DateTimeOffset> {
    let row = bytes.iter().position(|byte| *byte == 0xd1)?;
    let len = usize::from(*bytes.get(row + 1)?);
    DateTimeOffset::decode(bytes.get(row + 2..row + 2 + len)?, scale).ok()
}

#[test]
fn utc_uses_the_full_sql_temporal_range_without_extrapolating_other_zones() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/at-time-zone-utc-range.json")).unwrap();
    let expected = [
        "0001-01-01T00:00:00.0000000+00:00",
        "1899-12-31T23:59:59.9999999+00:00",
        "1900-01-01T00:00:00.0000000+00:00",
        "2050-12-31T23:59:59.9999999+00:00",
        "2051-01-01T00:00:00.0000000+00:00",
        "9999-12-31T23:59:59.9999999+00:00",
        "0001-01-01T00:00:00.0000000+00:00",
        "1899-12-31T23:59:59.9999999+00:00",
        "2051-01-01T00:00:00.0000000+00:00",
        "9999-12-31T23:59:59.9999999+00:00",
        "1899-12-31T21:59:59.9999999+00:00",
        "2050-12-31T22:00:00.0000000+00:00",
    ];
    let cases = fixture["results"].as_array().unwrap();
    assert_eq!(cases.len(), expected.len() + 4);
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for (index, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let reference = &case["reference"];
        assert_eq!(
            reference["sets"][0]["columns"][0]["type"], "DateTimeOffset",
            "{name}"
        );
        assert_eq!(reference["sets"][0]["columns"][0]["scale"], 7, "{name}");
        assert_eq!(reference["sets"][0]["columns"][0]["flags"], 33, "{name}");
        assert!(reference["errors"].as_array().unwrap().is_empty(), "{name}");
        let (bytes, ok) = session.batch_response(
            case["query"].as_str().unwrap(),
            &Default::default(),
            false,
            None,
        );
        if let Some(expected) = expected.get(index) {
            assert!(ok, "{name}: {bytes:?}");
            assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]), "{name}");
            assert_eq!(
                u16::from_le_bytes([bytes[7], bytes[8]]),
                1,
                "{name}: reference flags 33"
            );
            assert_eq!(
                first_offset(&bytes, 7),
                Some(DateTimeOffset::parse_iso(expected).unwrap()),
                "{name}"
            );
        } else {
            assert!(
                !ok,
                "uncaptured named-zone rules must remain explicit: {name}"
            );
        }
    }
}

#[test]
fn utc_boundary_nulls_and_scales_remain_typed() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for (sql, scale, expected) in [
        (
            "SELECT CAST('0001-01-01T00:00:00' AS DATETIME2(0)) AT TIME ZONE 'UTC' AS value",
            0,
            Some("0001-01-01T00:00:00+00:00"),
        ),
        (
            "SELECT CAST('9999-12-31T23:59:59' AS DATETIME2(0)) AT TIME ZONE 'UTC' AS value",
            0,
            Some("9999-12-31T23:59:59+00:00"),
        ),
        (
            "SELECT CAST(NULL AS DATETIME2(3)) AT TIME ZONE 'UTC' AS value",
            3,
            None,
        ),
    ] {
        let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {bytes:?}");
        assert!(bytes.windows(2).any(|pair| pair == [0x2b, scale]), "{sql}");
        assert_eq!(
            first_offset(&bytes, scale),
            expected.map(|text| DateTimeOffset::parse_iso(text).unwrap()),
            "{sql}"
        );
    }
}
