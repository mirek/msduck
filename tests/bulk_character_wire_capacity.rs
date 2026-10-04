//! Replay original SQL Server requests, including the source declaration and
//! BulkLoadBCP framing, rather than constructing candidate-dependent inputs.
use msduck::{engine::Session, server::Server};
use serde_json::Value;

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn ok(session: &mut Session, sql: &str) {
    let (response, success) = session.batch_response(sql, &Default::default(), false, None);
    assert!(success, "{sql}: {response:?}");
}

fn replay(session: &mut Session, observation: &Value) -> Vec<u8> {
    ok(session, observation["setup"]["sql"].as_str().unwrap());
    replay_requests(session, observation)
}

fn replay_requests(session: &mut Session, observation: &Value) -> Vec<u8> {
    let packets = observation["execution"]["packets"].as_array().unwrap();
    let mut sql = Vec::new();
    for packet in packets {
        if packet["direction"] != "out" {
            continue;
        }
        let bytes = hex(packet["rawHex"].as_str().unwrap());
        assert_eq!(
            usize::from(u16::from_be_bytes([bytes[2], bytes[3]])),
            bytes.len()
        );
        match bytes[0] {
            1 => {
                sql.extend_from_slice(&bytes[8..]);
                if bytes[1] & 1 != 0 {
                    let header = u32::from_le_bytes(sql[..4].try_into().unwrap()) as usize;
                    let units: Vec<u16> = sql[header..]
                        .chunks_exact(2)
                        .map(|unit| u16::from_le_bytes(unit.try_into().unwrap()))
                        .collect();
                    ok(session, &String::from_utf16(&units).unwrap());
                    assert!(session.bulk_load_expected());
                    sql.clear();
                }
            }
            7 => session.bulk_load_packet(&bytes[8..], bytes[1] & 1 != 0),
            other => panic!("unexpected original request type {other}"),
        }
    }
    session.bulk_load_finish(false)
}

fn has_error(response: &[u8], error: &Value) -> bool {
    has_diagnostic(response, error, 0xaa)
}

fn has_diagnostic(response: &[u8], error: &Value, kind: u8) -> bool {
    let mut body = (error["number"].as_i64().unwrap() as i32)
        .to_le_bytes()
        .to_vec();
    body.extend([
        error["state"].as_u64().unwrap() as u8,
        error["class"].as_u64().unwrap() as u8,
    ]);
    let units: Vec<u16> = error["message"].as_str().unwrap().encode_utf16().collect();
    body.extend((units.len() as u16).to_le_bytes());
    body.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    if response.first() != Some(&kind) || response.len() < 3 + body.len() {
        return false;
    }
    let length = usize::from(u16::from_le_bytes([response[1], response[2]]));
    let Some(token) = response.get(3..3 + length) else {
        return false;
    };
    if !token.starts_with(&body) {
        return false;
    }
    // Preserve the known server-identity gap while checking procedure and line.
    let mut at = body.len();
    let Some(&server_units) = token.get(at) else {
        return false;
    };
    at += 1 + usize::from(server_units) * 2;
    let Some(&procedure_units) = token.get(at) else {
        return false;
    };
    at += 1;
    let Some(procedure) = token.get(at..at + usize::from(procedure_units) * 2) else {
        return false;
    };
    let expected_procedure: Vec<u8> = error["procName"]
        .as_str()
        .unwrap()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    at += procedure.len();
    let Some(line) = token.get(at..at + 4) else {
        return false;
    };
    procedure == expected_procedure
        && u32::from_le_bytes(line.try_into().unwrap())
            == error["lineNumber"].as_u64().unwrap() as u32
        && at + 4 == token.len()
}

#[test]
fn original_rows_use_declared_source_capacity_independently_of_wire_width() {
    let reference: Value = serde_json::from_str(include_str!(
        "../reference/bulk-character-wire-capacity.json"
    ))
    .unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_wire_capacity");
    ok(&mut session, "USE bulk_wire_capacity");
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for run in reference["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap() {
            let response = replay(&mut session, observation);
            let errors = observation["execution"]["result"]["errors"]
                .as_array()
                .unwrap();
            let original = observation["execution"]["packets"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|packet| packet["direction"] == "in")
                .unwrap();
            let original = hex(original["rawHex"].as_str().unwrap());
            let diagnostics_match = match errors.first() {
                Some(error) => {
                    has_error(&response, error)
                        && response.ends_with(&original[original.len() - 13..])
                }
                None => response == original[8..],
            };
            let counters = &observation["readback"]["result"]["sets"][0]["rows"][0];
            let counters_match = session.last_error == counters[0].as_i64().unwrap() as i32
                && session.rowcount == counters[1].as_u64().unwrap()
                && session.transactions == counters[3].as_u64().unwrap() as u32;
            let unicode = observation["case"]["targetFamily"]
                .as_str()
                .unwrap()
                .starts_with('n');
            // The ANSI reference controls contain ASCII only. Preserve original
            // Unicode bytes through the physical carrier, never display strings.
            let payload = if unicode {
                "value.__msduck_utf16le"
            } else {
                "encode(value)"
            };
            let mut query = session
                .db
                .prepare(&format!(
                    "SELECT id, {payload} FROM dbo.character_metadata_probe ORDER BY id"
                ))
                .unwrap();
            let actual: Vec<(i32, Option<Vec<u8>>)> = query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let expected: Vec<(i32, Option<Vec<u8>>)> = observation["readback"]["result"]["sets"]
                [1]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    (
                        row[0].as_i64().unwrap() as i32,
                        row[2]["value"].as_str().map(hex),
                    )
                })
                .collect();
            if !diagnostics_match || !counters_match || actual != expected {
                mismatches.push(format!("{}: expected rows={expected:02x?}, actual={actual:02x?}; error={} rowcount={} transactions={} response={response:02x?}", observation["case"]["name"], session.last_error, session.rowcount, session.transactions));
            }
            drop(query);
            assert!(!session.bulk_load_expected());
            ok(
                &mut session,
                observation["cleanup"]["sql"].as_str().unwrap(),
            );
            ok(&mut session, "SELECT 1 AS recovered");
            checked += 1;
        }
    }
    assert_eq!(checked, 256);
    assert!(
        mismatches.is_empty(),
        "{} original ROW capacity mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

fn original_bulk(observation: &Value) -> Vec<u8> {
    let mut body = Vec::new();
    for packet in observation["execution"]["packets"].as_array().unwrap() {
        if packet["direction"] == "out" {
            let bytes = hex(packet["rawHex"].as_str().unwrap());
            if bytes[0] == 7 {
                body.extend_from_slice(&bytes[8..]);
            }
        }
    }
    body
}

#[test]
fn source_overflow_after_modern_zero_width_staging_is_atomic() {
    let reference: Value = serde_json::from_str(include_str!(
        "../reference/bulk-character-wire-capacity.json"
    ))
    .unwrap();
    let observations = reference["runs"][0]["observations"].as_array().unwrap();
    let good = observations
        .iter()
        .find(|o| o["case"]["name"] == "varchar8-wire0-one")
        .unwrap();
    let bad = observations
        .iter()
        .find(|o| o["case"]["name"] == "varchar1-wire0-two")
        .unwrap();
    let body = original_bulk(good);
    let start = body
        .windows(6)
        .position(|w| w == [0xd1, 4, 1, 0, 0, 0])
        .unwrap();
    let row = |payload: &[u8]| {
        let mut row = vec![0xd1, 4, 2, 0, 0, 0];
        row.extend((payload.len() as u16).to_le_bytes());
        row.extend(payload);
        row
    };
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_wire_staging");
    ok(&mut session, "USE bulk_wire_staging");
    ok(&mut session, good["setup"]["sql"].as_str().unwrap());
    ok(&mut session, "BEGIN TRANSACTION");
    ok(
        &mut session,
        "INSERT dbo.character_metadata_probe VALUES (99, 'P')",
    );
    ok(
        &mut session,
        "INSERT BULK dbo.character_metadata_probe ([id] int, [value] varchar(8000) COLLATE SQL_Latin1_General_CP1_CI_AS) WITH (KEEP_NULLS)",
    );
    let mut payload = vec![b' '; 8000];
    payload[0] = b'A';
    let valid = row(&payload);
    let mut first = body[..start].to_vec();
    for _ in 0..501 {
        first.extend(&valid);
    }
    session.bulk_load_packet(&first, false);
    payload.push(b' ');
    let mut last = row(&payload);
    last.extend(&body[body.len() - 13..]);
    session.bulk_load_packet(&last, true);
    let response = session.bulk_load_finish(false);
    assert!(
        has_error(&response, &bad["execution"]["result"]["errors"][0]),
        "{response:02x?}"
    );
    assert_eq!(session.last_error, 4815);
    assert_eq!(session.rowcount, 0);
    assert_eq!(session.transactions, 1);
    let remaining: Vec<i32> = session
        .db
        .prepare("SELECT id FROM dbo.character_metadata_probe ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(remaining, vec![99]);
    let staged: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE contains(table_name, '__msduck_bulk_')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(staged, 0);
    ok(
        &mut session,
        "INSERT dbo.character_metadata_probe VALUES (100, 'R')",
    );
    ok(&mut session, "COMMIT");
    assert_eq!(session.transactions, 0);
    let count: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM dbo.character_metadata_probe",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}
