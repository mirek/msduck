//! SQL-facing coverage of the captured temporal AT TIME ZONE families.
use std::collections::HashMap;

use msduck::{
    datetimeoffset::DateTimeOffset, engine::Session, parameter::Parameter, server::Server,
};
use msduck_core::{
    types::{Scale, Type},
    value::Value,
};

fn session() -> Session {
    let server = Server::open(":memory:").unwrap();
    Session::new(server.connection().unwrap()).unwrap()
}

fn response(session: &mut Session, sql: &str) -> Vec<u8> {
    let (bytes, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {bytes:?}");
    bytes
}

fn first_offset(bytes: &[u8], scale: u8) -> Option<DateTimeOffset> {
    let row = bytes.iter().position(|byte| *byte == 0xd1)?;
    let len = usize::from(*bytes.get(row + 1)?);
    DateTimeOffset::decode(bytes.get(row + 2..row + 2 + len)?, scale).ok()
}

#[test]
fn retained_sql_server_rows_keep_utc_ticks_offset_and_scale() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/at-time-zone.json")).unwrap();
    let mut session = session();
    for name in [
        "datetime2 UTC",
        "datetime2 named zone",
        "datetimeoffset changes zone",
        "chained zone conversion",
        "Central Europe spring 2022-03-27T02:30:00",
        "Central Europe autumn 2022-10-30T02:30:00",
        "Pacific spring gap",
        "Pacific autumn overlap",
    ] {
        let case = fixture["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let sql = case["query"].as_str().unwrap();
        let scale = case["reference"]["sets"][0]["columns"][0]["scale"]
            .as_u64()
            .unwrap() as u8;
        let rendered = case["reference"]["sets"][0]["rows"][0][1].as_str().unwrap();
        let expected = rendered
            .replacen(' ', "T", 1)
            .replace(" +", "+")
            .replace(" -", "-");
        let bytes = response(&mut session, sql);
        assert!(bytes.windows(2).any(|pair| pair == [0x2b, scale]), "{name}");
        let expected_flags = case["reference"]["sets"][0]["columns"][0]["flags"]
            .as_u64()
            .unwrap() as u16;
        assert_eq!(
            u16::from_le_bytes([bytes[7], bytes[8]]),
            expected_flags,
            "{name} flags"
        );
        assert_eq!(
            first_offset(&bytes, scale),
            Some(DateTimeOffset::parse_iso(&expected).unwrap()),
            "{name}"
        );
    }
}

#[test]
fn captured_local_instant_gap_overlap_and_chaining_reach_native_rules() {
    let mut session = session();
    for (sql, expected) in [
        (
            "SELECT CAST('2024-01-02T03:04:05.1234567' AS DATETIME2(7)) AT TIME ZONE 'UTC' AS value",
            "2024-01-02T03:04:05.1234567+00:00",
        ),
        (
            "SELECT CAST('2024-01-02T03:04:05+02:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'UTC' AS value",
            "2024-01-02T01:04:05.0000000+00:00",
        ),
        (
            "SELECT CAST('2022-03-27T02:30:00' AS DATETIME2(7)) AT TIME ZONE 'Central European Standard Time' AS value",
            "2022-03-27T03:30:00.0000000+02:00",
        ),
        (
            "SELECT CAST('2022-10-30T02:30:00' AS DATETIME2(7)) AT TIME ZONE 'Central European Standard Time' AS value",
            "2022-10-30T02:30:00.0000000+02:00",
        ),
        (
            "SELECT CAST('2024-03-10T02:30:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time' AS value",
            "2024-03-10T03:30:00.0000000-07:00",
        ),
        (
            "SELECT CAST('2024-11-03T01:30:00' AS DATETIME2(7)) AT TIME ZONE 'Pacific Standard Time' AS value",
            "2024-11-03T01:30:00.0000000-07:00",
        ),
        (
            "SELECT CAST('2024-01-02T03:04:05+02:00' AS DATETIMEOFFSET(7)) AT TIME ZONE 'UTC' AT TIME ZONE 'Central European Standard Time' AS value",
            "2024-01-02T02:04:05.0000000+01:00",
        ),
    ] {
        let bytes = response(&mut session, sql);
        assert!(bytes.starts_with(&[0x81, 1, 0]), "{sql}: {bytes:?}");
        assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]));
        assert_eq!(
            first_offset(&bytes, 7),
            Some(DateTimeOffset::parse_iso(expected).unwrap()),
            "{sql}"
        );
    }
}

#[test]
fn null_and_empty_results_keep_datetimeoffset_scale() {
    let mut session = session();
    for (sql, row_expected) in [
        (
            "SELECT CAST(NULL AS DATETIME2(3)) AT TIME ZONE 'UTC' AS value",
            true,
        ),
        (
            "SELECT CAST('2024-01-01' AS DATETIME2(3)) AT TIME ZONE NULL AS value",
            true,
        ),
        (
            "SELECT CAST('2024-01-01' AS DATETIME2(3)) AT TIME ZONE 'UTC' AS value WHERE 1=0",
            false,
        ),
    ] {
        let bytes = response(&mut session, sql);
        assert!(
            bytes.windows(2).any(|pair| pair == [0x2b, 3]),
            "{sql}: {bytes:?}"
        );
        assert_eq!(bytes.contains(&0xd1), row_expected, "{sql}");
        if row_expected {
            assert_eq!(first_offset(&bytes, 3), None, "{sql}");
        }
    }
}

#[test]
fn every_scale_is_typed_and_volatile_operands_run_once_per_row() {
    let mut session = session();
    for scale in 0..=7 {
        let sql = format!(
            "SELECT CAST('2024-01-02T03:04:05' AS DATETIME2({scale})) AT TIME ZONE 'UTC' AS value"
        );
        let bytes = response(&mut session, &sql);
        assert!(
            bytes.windows(2).any(|pair| pair == [0x2b, scale]),
            "{sql}: {bytes:?}"
        );
        assert_eq!(
            first_offset(&bytes, scale),
            Some(DateTimeOffset::parse_iso("2024-01-02T03:04:05+00:00").unwrap())
        );
        let sql = format!(
            "SELECT CAST('2024-01-02T03:04:05+02:00' AS DATETIMEOFFSET({scale})) AT TIME ZONE 'UTC' AS value"
        );
        let bytes = response(&mut session, &sql);
        assert!(bytes.windows(2).any(|pair| pair == [0x2b, scale]));
        assert_eq!(
            first_offset(&bytes, scale),
            Some(DateTimeOffset::parse_iso("2024-01-02T01:04:05+00:00").unwrap())
        );
    }

    session
        .db
        .execute_batch("CREATE SEQUENCE zone_source_calls; CREATE SEQUENCE zone_name_calls")
        .unwrap();
    response(
        &mut session,
        "SELECT COUNT(value) FROM (SELECT CAST(printf('2024-01-01T00:00:%02d',nextval('zone_source_calls')%60) AS DATETIME2(7)) AT TIME ZONE CASE WHEN nextval('zone_name_calls')%17=0 THEN NULL ELSE 'UTC' END AS value FROM range(6000)) v",
    );
    for sequence in ["zone_source_calls", "zone_name_calls"] {
        assert_eq!(
            session
                .db
                .query_row("SELECT currval(?)", [sequence], |row| row.get::<_, i64>(0))
                .unwrap(),
            6000,
            "{sequence}"
        );
    }
}

#[test]
fn prepared_bindings_keep_declared_scale_across_values_and_null() {
    let mut session = session();
    let sql = "SELECT @stamp AT TIME ZONE 'UTC' AS value";
    let mut parameters = HashMap::from([(
        "@stamp".to_string(),
        Parameter {
            value: Value::Null,
            data_type: Type::DateTime2(Scale::new(7).unwrap()),
        },
    )]);
    for (value, expected) in [
        (
            Value::Text("2024-01-02T03:04:05.1234567".into()),
            Some("2024-01-02T03:04:05.1234567+00:00"),
        ),
        (
            Value::Text("2024-07-02T05:06:07.7654321".into()),
            Some("2024-07-02T05:06:07.7654321+00:00"),
        ),
        (Value::Null, None),
    ] {
        parameters.get_mut("@stamp").unwrap().value = value;
        let bytes = session.prepared_batch(sql, &parameters);
        assert!(bytes.windows(2).any(|pair| pair == [0x2b, 7]), "{bytes:?}");
        assert_eq!(
            first_offset(&bytes, 7),
            expected.map(|text| DateTimeOffset::parse_iso(text).unwrap())
        );
    }
}

#[test]
fn unknown_zone_and_uncaptured_inputs_fail_explicitly() {
    let mut session = session();
    for sql in [
        "SELECT CAST('2024-01-01' AS DATETIME2(7)) AT TIME ZONE 'Not A Time Zone'",
        "SELECT CAST('2024-01-01' AS DATE) AT TIME ZONE 'UTC'",
        "SELECT 1 AT TIME ZONE 'UTC'",
    ] {
        assert!(
            !session
                .batch_response(sql, &Default::default(), false, None)
                .1,
            "{sql}"
        );
    }
}
