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
fn max_metadata_is_checked_even_when_no_rows_are_sent() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/bulk-character-metadata.json")).unwrap();
    let observation = reference["runs"][0]["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["case"]["name"] == "cp1252-varcharmax-wirevarchar8-target8-null-only")
        .unwrap();
    let mut empty = observation.clone();
    for packet in empty["execution"]["packets"].as_array_mut().unwrap() {
        if packet["direction"] == "out" && packet["rawHex"].as_str().unwrap().starts_with("07") {
            let mut bytes = hex(packet["rawHex"].as_str().unwrap());
            let row = bytes
                .windows(6)
                .position(|window| window == [0xd1, 4, 1, 0, 0, 0])
                .unwrap();
            let done = bytes[bytes.len() - 13..].to_vec();
            bytes.truncate(row);
            bytes.extend(done);
            let length = u16::try_from(bytes.len()).unwrap().to_be_bytes();
            bytes[2..4].copy_from_slice(&length);
            packet["rawHex"] = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                .into();
        }
    }
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_admission_empty");
    ok(&mut session, "USE bulk_admission_empty");
    let response = replay(&mut session, &empty);
    assert!(has_error(
        &response,
        &observation["execution"]["result"]["errors"][0]
    ));
    assert_eq!(session.rowcount, 0);
    assert_eq!(session.last_error, 4816);
}

#[test]
fn bounded_wire_overflow_uses_source_length_and_preserves_metadata_precedence() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/bulk-character-metadata.json")).unwrap();
    let observations = reference["runs"][0]["observations"].as_array().unwrap();
    let find = |name: &str| {
        observations
            .iter()
            .find(|o| o["case"]["name"] == name)
            .unwrap()
    };
    let good = find("cp1252-char1-wirechar1-target8-baseline");
    let source_failure = find("cp1252-char1-wirechar8-target8-baseline");
    let metadata_failure = find("cp1252-varchar8-wirechar8-target8-baseline");
    let body = original_bulk(good);
    let first_row = [0xd1, 4, 1, 0, 0, 0];
    let start = body
        .windows(first_row.len())
        .position(|window| window == first_row)
        .unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_admission_wire");
    ok(&mut session, "USE bulk_admission_wire");
    ok(&mut session, good["setup"]["sql"].as_str().unwrap());
    for (declaration, expected_error) in [
        (
            "char(1)",
            Some(&source_failure["execution"]["result"]["errors"][0]),
        ),
        (
            "varchar(8)",
            Some(&metadata_failure["execution"]["result"]["errors"][0]),
        ),
        ("char(8)", None),
    ] {
        ok(
            &mut session,
            &format!(
                "INSERT BULK dbo.character_metadata_probe ([id] int, [value] {declaration}) WITH (KEEP_NULLS)"
            ),
        );
        // A complete length prefix, without allocating/supplying its oversized
        // payload, is enough. Both wire and declared violations are visible.
        let mut prefix = body[..start].to_vec();
        prefix.extend([0xd1, 4, 1, 0, 0, 0, 2, 0]);
        session.bulk_load_packet(&prefix, true);
        let response = session.bulk_load_finish(false);
        if let Some(error) = expected_error {
            assert!(
                has_error(&response, error),
                "{declaration}: {response:02x?}"
            );
        } else {
            // The source declaration fits; wire-only overflow has not been
            // assigned a SQL4815 rule by this reference, so preserve old4804.
            assert_eq!(session.last_error, 4804);
        }
        assert_eq!(session.rowcount, 0);
        let count: i64 = session
            .db
            .query_row(
                "SELECT count(*) FROM dbo.character_metadata_probe",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn declared_overflow_after_staging_preserves_the_callers_transaction() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/bulk-character-metadata.json")).unwrap();
    let observations = reference["runs"][0]["observations"].as_array().unwrap();
    let find = |name: &str| {
        observations
            .iter()
            .find(|o| o["case"]["name"] == name)
            .unwrap()
    };
    let good = find("cp1252-varchar1-wirevarcharmax-target8-baseline");
    let bad = find("cp1252-varchar1-wirevarchar8-target8-declared-over");
    let good_body = original_bulk(good);
    let first_row = [0xd1, 4, 1, 0, 0, 0];
    let start = good_body
        .windows(first_row.len())
        .position(|window| window == first_row)
        .unwrap();
    // Wide valid rows reach the same staging path without hundreds of
    // thousands of tiny rows. The target accepts only trailing-space overflow;
    // the later 8001-byte source fails its declared 8000-byte admission first.
    let row = |payload: &[u8]| {
        let mut row = vec![0xd1, 4, 2, 0, 0, 0];
        row.extend((u64::MAX - 1).to_le_bytes());
        row.extend((payload.len() as u32).to_le_bytes());
        row.extend_from_slice(payload);
        row.extend(0u32.to_le_bytes());
        row
    };
    let mut payload = vec![b' '; 8000];
    payload[0] = b'A';
    let valid_row = row(&payload);
    let mut first_packet = good_body[..start].to_vec();
    for _ in 0..501 {
        first_packet.extend_from_slice(&valid_row);
    }
    assert!(first_packet.len() > 1024 * 1024);
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_admission_staging");
    ok(&mut session, "USE bulk_admission_staging");
    ok(&mut session, good["setup"]["sql"].as_str().unwrap());
    ok(&mut session, "BEGIN TRANSACTION");
    ok(
        &mut session,
        "INSERT dbo.character_metadata_probe VALUES (99, 'P')",
    );
    // Reuse the original SQL shape with an explicit boundary declaration.
    let sql_packet = good["execution"]["packets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|packet| packet["direction"] == "out")
        .unwrap();
    let bytes = hex(sql_packet["rawHex"].as_str().unwrap());
    let header = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let units: Vec<u16> = bytes[8 + header..]
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes(unit.try_into().unwrap()))
        .collect();
    ok(
        &mut session,
        &String::from_utf16(&units)
            .unwrap()
            .replace("varchar(1)", "varchar(8000)"),
    );
    let mut final_packet = row(&vec![b'A'; 8001]);
    final_packet.extend_from_slice(&good_body[good_body.len() - 13..]);
    first_packet.extend(final_packet);
    session.bulk_load_packet(&first_packet, true);
    let staged: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE contains(table_name, '__msduck_bulk_')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(staged > 0, "the test must actually reach staging");
    let response = session.bulk_load_finish(false);
    assert!(has_error(
        &response,
        &bad["execution"]["result"]["errors"][0]
    ));
    let mut done = Vec::new();
    msduck_tds::done(&mut done, 0xfd, 2, 253, 0);
    assert!(response.ends_with(&done));
    assert_eq!(session.last_error, 4815);
    assert_eq!(session.rowcount, 0);
    assert_eq!(session.transactions, 1);
    let count: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM dbo.character_metadata_probe WHERE id=99 AND value='P'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let total: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM dbo.character_metadata_probe",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 1);
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
    let total: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM dbo.character_metadata_probe",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 2);
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
fn admitted_source_still_reports_original_target_capacity_diagnostics() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/bulk-character-metadata.json")).unwrap();
    let observation = reference["runs"][0]["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["case"]["name"] == "cp1252-varchar8-wirevarchar8-target1-target-over")
        .unwrap();
    let error = &observation["execution"]["result"]["errors"][0];
    // Recreate the original database identity instead of normalizing its name
    // out of SQL2628's message to obtain an apparent comparison pass.
    let database = error["message"]
        .as_str()
        .unwrap()
        .split("table '")
        .nth(1)
        .unwrap()
        .split(".dbo.")
        .next()
        .unwrap();
    assert!(
        database
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, &format!("CREATE DATABASE [{database}]"));
    ok(&mut session, &format!("USE [{database}]"));
    let response = replay(&mut session, observation);
    assert!(has_error(&response, error), "{response:02x?}");
    let error_length = usize::from(u16::from_le_bytes([response[1], response[2]]));
    assert!(has_diagnostic(
        &response[3 + error_length..],
        &observation["execution"]["result"]["info"][0],
        0xab
    ));
    let original_response = observation["execution"]["packets"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|packet| packet["direction"] == "in")
        .unwrap();
    let original = hex(original_response["rawHex"].as_str().unwrap());
    assert!(response.ends_with(&original[original.len() - 13..]));
    assert_eq!(session.last_error, 2628);
    assert_eq!(session.rowcount, 0);
    assert_eq!(session.transactions, 0);
    let count: i64 = session
        .db
        .query_row(
            "SELECT count(*) FROM dbo.character_metadata_probe",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    ok(&mut session, "SELECT 1 AS recovered");
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

#[test]
fn original_successes_preserve_native_bytes_and_source_padding() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/bulk-character-metadata.json")).unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_admission_success");
    ok(&mut session, "USE bulk_admission_success");
    let mut mismatches = Vec::new();
    let mut checked = 0;
    for observation in reference["runs"][0]["observations"].as_array().unwrap() {
        if observation["case"]["declared"] != "cp1252"
            || !observation["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .is_empty()
        {
            continue;
        }
        let response = replay(&mut session, observation);
        let original_response = observation["execution"]["packets"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|packet| packet["direction"] == "in")
            .unwrap();
        let original = hex(original_response["rawHex"].as_str().unwrap());
        assert_eq!(response, original[8..], "{}", observation["case"]["name"]);
        let counters = &observation["readback"]["result"]["sets"][0]["rows"][0];
        assert_eq!(session.last_error, counters[0].as_i64().unwrap() as i32);
        assert_eq!(session.rowcount, counters[1].as_u64().unwrap());
        assert_eq!(session.transactions, counters[3].as_u64().unwrap() as u32);
        let unicode = observation["case"]["targetFamily"]
            .as_str()
            .unwrap()
            .starts_with('n');
        // ANSI controls here are exclusively ASCII; encoding the physical
        // VARCHAR preserves these exact bytes. Unicode uses its binary carrier.
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
        let expected: Vec<(i32, Option<Vec<u8>>)> =
            observation["readback"]["result"]["sets"][1]["rows"]
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
        if actual != expected {
            mismatches.push(format!(
                "{}: expected {expected:02x?}, actual {actual:02x?}",
                observation["case"]["name"]
            ));
        }
        drop(query);
        ok(
            &mut session,
            observation["cleanup"]["sql"].as_str().unwrap(),
        );
        checked += 1;
    }
    assert_eq!(checked, 37);
    assert!(
        mismatches.is_empty(),
        "{} original native readback mismatches:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

// Original zero-width requests and observations, acquired in four independent
// databases on pinned SQL Server17.0.4065.4. Full capture/source SHA and limits
// are recorded in docs/bulk-character-admission.md. No candidate expectations.
#[test]
fn original_zero_width_metadata_preserves_family_max_and_fixed_padding() {
    let observations: Value = serde_json::from_str(ZERO_WIDTH_ORIGINALS).unwrap();
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    ok(&mut session, "CREATE DATABASE bulk_zero_width");
    ok(&mut session, "USE bulk_zero_width");
    for observation in observations.as_array().unwrap() {
        let response = replay(&mut session, observation);
        let errors = observation["execution"]["result"]["errors"]
            .as_array()
            .unwrap();
        if let Some(error) = errors.first() {
            assert!(
                has_error(&response, error),
                "{}",
                observation["case"]["name"]
            );
        } else {
            let packet = observation["execution"]["packets"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|p| p["direction"] == "in")
                .unwrap();
            assert_eq!(response, hex(packet["rawHex"].as_str().unwrap())[8..]);
        }
        let counters = &observation["readback"]["result"]["sets"][0]["rows"][0];
        assert_eq!(i64::from(session.last_error), counters[0].as_i64().unwrap());
        assert_eq!(session.rowcount, counters[1].as_u64().unwrap());
        assert_eq!(
            u64::from(session.transactions),
            counters[3].as_u64().unwrap()
        );
        let payload = if observation["case"]["targetFamily"]
            .as_str()
            .unwrap()
            .starts_with('n')
        {
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
        let expected: Vec<(i32, Option<Vec<u8>>)> =
            observation["readback"]["result"]["sets"][1]["rows"]
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
        assert_eq!(actual, expected, "{}", observation["case"]["name"]);
        drop(query);
        ok(&mut session, "DROP TABLE dbo.character_metadata_probe");
        ok(&mut session, "SELECT 1 AS recovered");
    }
}
const ZERO_WIDTH_ORIGINALS: &str = r#"[{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"null","name":"varchar8-wirevarchar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,null,null,null,null]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"null","name":"varchar8-wirevarchar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"empty","name":"varchar8-wirevarchar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"empty","name":"varchar8-wirevarchar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"null","name":"varchar8-wirechar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"null","name":"varchar8-wirechar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"empty","name":"varchar8-wirechar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"varchar","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"empty","name":"varchar8-wirechar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011a000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"null","name":"char8-wirevarchar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"null","name":"char8-wirevarchar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"empty","name":"char8-wirevarchar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"varchar","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"empty","name":"char8-wirevarchar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500a700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"null","name":"char8-wirechar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,null,null,null,null]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"null","name":"char8-wirechar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":8,"category":"empty","name":"char8-wirechar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,"        ",{"kind":"binary","value":"2020202020202020"},"        ",{"kind":"binary","value":"20002000200020002000200020002000"}]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"cp1252","sourceFamily":"char","sourceWidth":8,"wireFamily":"char","wireWidth":0,"targetFamily":"varchar","targetWidth":"max","category":"empty","name":"char8-wirechar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value varchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010114000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500af00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"null","name":"nvarchar8-wirenvarchar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,null,null,null,null]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"null","name":"nvarchar8-wirenvarchar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"empty","name":"nvarchar8-wirenvarchar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"empty","name":"nvarchar8-wirenvarchar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"null","name":"nvarchar8-wirenchar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"null","name":"nvarchar8-wirenchar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"empty","name":"nvarchar8-wirenchar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nvarchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"empty","name":"nvarchar8-wirenchar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"0101011c000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e007600610072006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"null","name":"nchar8-wirenvarchar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"null","name":"nchar8-wirenvarchar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"empty","name":"nchar8-wirenvarchar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nvarchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"empty","name":"nchar8-wirenvarchar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":1,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500e700000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000110300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"null","name":"nchar8-wirenchar0-target8-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,null,null,null,null]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"null","name":"nchar8-wirenchar0-targetmax-null","values":[null]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d10401000000fffffd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":8,"category":"empty","name":"nchar8-wirenchar0-target8-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(8) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[],"info":[],"rowCount":1,"error":null},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401001500430100fd1000f0000100000000000000"}]},"readback":{"result":{"sets":[{"rows":[[0,1,0,0]]},{"rows":[[1,"        ",{"kind":"binary","value":"20002000200020002000200020002000"},"        ",{"kind":"binary","value":"20002000200020002000200020002000"}]]}]}}},{"case":{"declared":"cp1252","wire":"cp1252","target":"unicode","sourceFamily":"nchar","sourceWidth":8,"wireFamily":"nchar","wireWidth":0,"targetFamily":"nvarchar","targetWidth":"max","category":"empty","name":"nchar8-wirenchar0-targetmax-empty","values":[""]},"setup":{"sql":"CREATE TABLE dbo.character_metadata_probe (id int NOT NULL, value nvarchar(max) COLLATE SQL_Latin1_General_CP1_CI_AS NULL)"},"execution":{"result":{"errors":[{"number":4816,"state":2,"class":16,"lineNumber":1,"message":"Invalid column type from bcp client for colid 2.","serverName":"3c95d77b1409","procName":""}],"info":[],"rowCount":0,"error":{"number":4816,"code":"EREQUEST","message":"Invalid column type from bcp client for colid 2."}},"packets":[{"direction":"out","rawHex":"01010116000001001600000012000000020000000000000000000100000049004e0053004500520054002000420055004c004b002000640062006f002e006300680061007200610063007400650072005f006d0065007400610064006100740061005f00700072006f0062006500200028005b00690064005d00200069006e0074002c0020005b00760061006c00750065005d0020006e006300680061007200280038002900200043004f004c004c004100540045002000530051004c005f004c006100740069006e0031005f00470065006e006500720061006c005f004300500031005f00430049005f0041005300290020005700490054004800200028004b004500450050005f004e0055004c004c0053002900"},{"direction":"in","rawHex":"0401001500430100fd0000fd000000000000000000"},{"direction":"out","rawHex":"070100460000010081020000000000040026040269006400000000000500ef00000904d0003405760061006c0075006500d104010000000000fd000000000000000000000000"},{"direction":"in","rawHex":"0401009e00430100aa8600d01200000210300049006e00760061006c0069006400200063006f006c0075006d006e00200074007900700065002000660072006f006d002000620063007000200063006c00690065006e007400200066006f007200200063006f006c0069006400200032002e000c3300630039003500640037003700620031003400300039000001000000fd0200fd000000000000000000"}]},"readback":{"result":{"sets":[{"rows":[[4816,0,0,0]]},{"rows":[]}]}}}]"#;
