//! SQL-facing checks of the pinned 0001–0499 named-zone rule prefix.
use msduck::{datetimeoffset::DateTimeOffset, engine::Session, server::Server};

fn first_offset(bytes: &[u8]) -> Option<DateTimeOffset> {
    let row = bytes.iter().position(|byte| *byte == 0xd1)?;
    let len = usize::from(*bytes.get(row + 1)?);
    DateTimeOffset::decode(bytes.get(row + 2..row + 2 + len)?, 7).ok()
}

#[test]
fn first_centuries_named_zone_seasons_match_retained_sql_server_offsets() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../reference/at-time-zone-history-0001-0499.json"
    ))
    .unwrap();
    let dates = [
        "0001-01-15",
        "0001-07-15",
        "0250-01-15",
        "0250-07-15",
        "0499-01-15",
        "0499-07-15",
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
fn first_century_utc_instants_and_local_walls_keep_distinct_year_one_rules() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for (sql, expected) in [
        (
            "SELECT CAST('0001-01-01T00:00:00+00:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'Pacific Standard Time'",
            "0001-01-01T00:00:00.0000000+00:00",
        ),
        (
            "SELECT CAST('0001-01-01T08:00:00+00:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'Pacific Standard Time'",
            "0001-01-01T00:00:00.0000000-08:00",
        ),
        (
            "SELECT CAST('0001-01-01T11:00:00+00:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'Samoa Standard Time'",
            "0001-01-01T00:00:00.0000000-11:00",
        ),
        (
            "SELECT CAST('0001-01-01T00:00:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time'",
            "0001-01-01T00:00:00.0000000-08:00",
        ),
        (
            "SELECT CAST('0001-01-01T00:00:00' AS DATETIME2(7)) AT TIME ZONE 'Samoa Standard Time'",
            "0001-01-01T00:00:00.0000000-11:00",
        ),
        (
            "SELECT CAST('0499-12-31T12:00:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time'",
            "0499-12-31T12:00:00.0000000-08:00",
        ),
        (
            "SELECT CAST('0500-01-01T00:00:00+00:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'Pacific Standard Time'",
            "0499-12-31T16:00:00.0000000-08:00",
        ),
    ] {
        let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(ok, "{sql}: {bytes:?}");
        assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]));
        assert_eq!(
            first_offset(&bytes),
            Some(DateTimeOffset::parse_iso(expected).unwrap()),
            "{sql}"
        );
    }
    for sql in [
        "SELECT CAST('0001-01-01T00:00:00' AS DATETIME2(7)) AT TIME ZONE 'Central European Standard Time'",
        "SELECT CAST('0001-01-01T00:00:00' AS DATETIME2(7)) AT TIME ZONE 'Line Islands Standard Time'",
        "SELECT CAST('2501-01-01T12:00:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time'",
    ] {
        let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
        assert!(!ok, "{sql}: {bytes:?}");
        assert!(first_offset(&bytes).is_none(), "{sql}");
    }
}

#[test]
fn every_captured_year_one_local_boundary_matches_its_offset_or_rejection() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../reference/at-time-zone-history-0001-0499.json"
    ))
    .unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let probes = fixture["localBoundaries"].as_array().unwrap();
    assert_eq!(probes.len(), 423);
    for probe in probes {
        let zone = probe["name"].as_str().unwrap();
        let time = probe["time"].as_str().unwrap();
        let sql = format!(
            "SELECT CAST('0001-01-01T{time}' AS DATETIME2(7)) AT TIME ZONE '{}'",
            zone.replace('\'', "''")
        );
        let (bytes, ok) = session.batch_response(&sql, &Default::default(), false, None);
        let reference = &probe["reference"];
        let rows = reference["sets"][0]["rows"].as_array().unwrap();
        if rows.is_empty() {
            assert_eq!(reference["errors"][0]["number"], 9813);
            assert!(!ok, "{zone} {time}: {bytes:?}");
            assert!(first_offset(&bytes).is_none(), "{zone} {time}");
            continue;
        }
        assert!(ok, "{zone} {time}: {bytes:?}");
        let minutes = rows[0][2].as_i64().unwrap();
        let sign = if minutes < 0 { '-' } else { '+' };
        let absolute = minutes.abs();
        let expected = format!(
            "0001-01-01T{time}.0000000{sign}{:02}:{:02}",
            absolute / 60,
            absolute % 60
        );
        assert_eq!(
            first_offset(&bytes),
            Some(DateTimeOffset::parse_iso(&expected).unwrap()),
            "{zone} {time}"
        );
    }
}
