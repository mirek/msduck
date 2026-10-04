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
    response.windows(body.len()).any(|window| window == body)
}

#[test]
fn original_metadata_and_declared_row_failures_are_atomic_and_recoverable() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/bulk-character-metadata.json")).unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_admission");
    ok(&mut session, "USE bulk_admission");
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for observation in reference["runs"][0]["observations"].as_array().unwrap() {
        // CP1251/UTF8 column declarations remain rejected by the endpoint.
        // Their unchanged originals must be covered by the pure admission
        // replay; do not substitute another collation to claim endpoint parity.
        if observation["case"]["declared"] != "cp1252" {
            continue;
        }
        // These diagnostics do not contain ephemeral server/database identity.
        let errors = observation["execution"]["result"]["errors"]
            .as_array()
            .unwrap();
        let Some(error) = errors.first() else {
            continue;
        };
        if !matches!(error["number"].as_i64(), Some(4815 | 4816)) {
            continue;
        }
        let response = replay(&mut session, observation);
        let mut expected_done = Vec::new();
        msduck_tds::done(&mut expected_done, 0xfd, 2, 253, 0);
        let count: i64 = session
            .db
            .query_row(
                "SELECT count(*) FROM dbo.character_metadata_probe",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if !has_error(&response, error)
            || !response.ends_with(&expected_done)
            || session.last_error != error["number"].as_i64().unwrap() as i32
            || session.rowcount != 0
            || session.transactions != 0
            || count != 0
        {
            mismatches.push(format!("{}: expected {} state {} class {}, actual last_error={} rowcount={} rows={} response={response:02x?}",
                observation["case"]["name"].as_str().unwrap(), error["number"], error["state"], error["class"],
                session.last_error, session.rowcount, count));
        }
        assert!(!session.bulk_load_expected());
        ok(
            &mut session,
            observation["cleanup"]["sql"].as_str().unwrap(),
        );
        ok(&mut session, "SELECT 1 AS recovered");
        checked += 1;
    }
    assert_eq!(checked, 24);
    assert!(
        mismatches.is_empty(),
        "{} original admission mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
