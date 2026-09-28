//! SQL-facing checks of the pinned 1500–1799 named-zone rule prefix.
use msduck::{datetimeoffset::DateTimeOffset, engine::Session, server::Server};

fn first_offset(bytes: &[u8]) -> Option<DateTimeOffset> {
    let row = bytes.iter().position(|byte| *byte == 0xd1)?;
    let len = usize::from(*bytes.get(row + 1)?);
    DateTimeOffset::decode(bytes.get(row + 2..row + 2 + len)?, 7).ok()
}

#[test]
fn early_named_zone_seasons_match_retained_sql_server_offsets() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../reference/at-time-zone-history-1500-1799.json"
    ))
    .unwrap();
    let dates = [
        "1500-01-15",
        "1500-07-15",
        "1650-01-15",
        "1650-07-15",
        "1799-01-15",
        "1799-07-15",
    ];
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for (sample, date) in fixture["samples"].as_array().unwrap().iter().zip(dates) {
        assert!(sample["query"].as_str().unwrap().contains(date));
        let rows = sample["reference"]["sets"][0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 141);
        for zone in [
            "Pacific Standard Time",
            "Central European Standard Time",
            "Samoa Standard Time",
        ] {
            let row = rows.iter().find(|row| row[0] == zone).unwrap();
            let minutes = row[1].as_i64().unwrap();
            let sign = if minutes < 0 { '-' } else { '+' };
            let absolute = minutes.abs();
            let expected = format!(
                "{date}T12:00:00.0000000{sign}{:02}:{:02}",
                absolute / 60,
                absolute % 60
            );
            let sql = format!(
                "SELECT CAST('{date}T12:00:00' AS DATETIME2(7)) AT TIME ZONE '{zone}' AS value"
            );
            let (bytes, ok) = session.batch_response(&sql, &Default::default(), false, None);
            assert!(ok, "{date} {zone}: {bytes:?}");
            assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]));
            assert_eq!(
                first_offset(&bytes),
                Some(DateTimeOffset::parse_iso(&expected).unwrap()),
                "{date} {zone}"
            );
        }
    }
}

#[test]
fn early_range_crosses_1500_and_1800() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for (sql, expected) in [
        (
            "SELECT CAST('1500-01-01T12:00:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time'",
            "1500-01-01T12:00:00.0000000-08:00",
        ),
        (
            "SELECT CAST('1500-01-01T00:00:00+00:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'Pacific Standard Time'",
            "1499-12-31T16:00:00.0000000-08:00",
        ),
        (
            "SELECT CAST('1799-12-31T12:00:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time'",
            "1799-12-31T12:00:00.0000000-08:00",
        ),
        (
            "SELECT CAST('1800-01-01T12:00:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time'",
            "1800-01-01T12:00:00.0000000-08:00",
        ),
    ] {
        let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {bytes:?}");
        assert_eq!(
            first_offset(&bytes),
            Some(DateTimeOffset::parse_iso(expected).unwrap()),
            "{sql}"
        );
    }
}
