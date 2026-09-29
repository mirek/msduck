use msduck_tds::error;
use serde_json::Value;

fn decoded_error(bytes: &[u8]) -> (i32, u8, u8, String) {
    assert!(bytes.len() >= 11, "truncated ERROR token");
    assert_eq!(bytes[0], 0xaa);
    let body_len = u16::from_le_bytes(bytes[1..3].try_into().unwrap()) as usize;
    assert_eq!(body_len + 3, bytes.len(), "incorrect ERROR token length");
    let number = i32::from_le_bytes(bytes[3..7].try_into().unwrap());
    let state = bytes[7];
    let severity = bytes[8];
    let units = u16::from_le_bytes(bytes[9..11].try_into().unwrap()) as usize;
    let end = 11 + units * 2;
    assert!(end <= bytes.len(), "truncated ERROR message");
    let message = String::from_utf16(
        &bytes[11..end]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes(pair.try_into().unwrap()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    (number, state, severity, message)
}

#[test]
fn distribution_errors_match_both_raw_sql_server_runs() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/distribution-reference.json"
    ))
    .unwrap();
    assert_eq!(fixture["runs"].as_array().unwrap().len(), 2);
    for run in fixture["runs"].as_array().unwrap() {
        let records = run
            .as_array()
            .unwrap()
            .iter()
            .filter(|record| {
                let name = record["name"].as_str().unwrap();
                name.starts_with("missing ")
                    || name.starts_with("frame ")
                    || name.starts_with("argument ")
            })
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 8);
        for record in records {
            let errors = record["result"]["errors"].as_array().unwrap();
            assert_eq!(errors.len(), 1);
            let captured = &errors[0];
            let number = captured["number"].as_i64().unwrap() as i32;
            let message = captured["message"].as_str().unwrap();
            let mut wire = Vec::new();
            error(&mut wire, number, message);
            assert_eq!(
                decoded_error(&wire),
                (
                    number,
                    captured["state"].as_u64().unwrap() as u8,
                    captured["class"].as_u64().unwrap() as u8,
                    message.to_owned(),
                ),
                "{}",
                record["name"]
            );
        }
    }
}

#[test]
fn unrelated_legacy_diagnostics_keep_their_attributes() {
    for (number, message, state, severity) in [
        (
            4106,
            "The function 'row_number' may not have a window frame.",
            1,
            15,
        ),
        (4114, "unrelated diagnostic", 1, 16),
        (
            10752,
            "The function 'OTHER' may not have a window frame.",
            1,
            16,
        ),
        (
            4112,
            "The function 'row_number' must have an OVER clause with ORDER BY.",
            1,
            16,
        ),
    ] {
        let mut wire = Vec::new();
        error(&mut wire, number, message);
        assert_eq!(
            decoded_error(&wire),
            (number, state, severity, message.to_owned())
        );
    }
}
