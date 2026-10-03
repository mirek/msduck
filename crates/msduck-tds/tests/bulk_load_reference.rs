#[path = "../src/bulk_load.rs"]
mod bulk_load;

use bulk_load::{Column, Decoder, Done, EomMode, Error, TypeInfo, Value, ValueFormat};
use serde_json::Value as Json;

fn fixture() -> Json {
    serde_json::from_str(include_str!("../../../reference/bulk-load-wire.json")).unwrap()
}

fn bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2));
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

fn expected_columns() -> Vec<Column> {
    [
        (
            "id",
            4,
            0x26,
            ValueFormat::ByteLen {
                max: 4,
                exact: true,
            },
            None,
        ),
        (
            "label",
            5,
            0xe7,
            ValueFormat::ShortLen {
                max: 32,
                unicode: true,
            },
            Some([9, 4, 0xd0, 0, 52]),
        ),
        (
            "payload",
            5,
            0xa5,
            ValueFormat::ShortLen {
                max: 16,
                unicode: false,
            },
            None,
        ),
    ]
    .into_iter()
    .map(|(name, flags, id, format, collation)| Column {
        name_utf16: name.encode_utf16().collect(),
        user_type: 0,
        flags,
        type_info: TypeInfo {
            id,
            format,
            precision: None,
            scale: None,
            collation,
        },
    })
    .collect()
}

fn expected_rows(observation: &Json) -> Vec<Vec<Value>> {
    observation["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let id = i32::try_from(row["id"].as_i64().unwrap()).unwrap();
            vec![
                Value {
                    bytes: Some(id.to_le_bytes().to_vec()),
                },
                Value {
                    bytes: row["label"]
                        .as_str()
                        .map(|s| s.encode_utf16().flat_map(u16::to_le_bytes).collect()),
                },
                Value {
                    bytes: row["payloadHex"].as_str().map(bytes),
                },
            ]
        })
        .collect()
}

fn request_payload(observation: &Json) -> Vec<u8> {
    let request = &observation["messages"][2];
    assert_eq!(request["direction"], "out");
    assert_eq!(request["type"], 7);
    let mut payload = Vec::new();
    let packets = request["packets"].as_array().unwrap();
    for (index, packet) in packets.iter().enumerate() {
        let raw = bytes(packet["rawHex"].as_str().unwrap());
        assert_eq!(raw[0], 7);
        assert_eq!(
            usize::from(u16::from_be_bytes(raw[2..4].try_into().unwrap())),
            raw.len()
        );
        assert_eq!(packet["length"].as_u64().unwrap(), raw.len() as u64);
        assert_eq!(raw[1] & 1 != 0, index + 1 == packets.len());
        assert_eq!(packet["status"].as_u64().unwrap(), u64::from(raw[1]));
        assert_eq!(packet["packetId"].as_u64().unwrap(), u64::from(raw[6]));
        assert_eq!(&raw[8..], bytes(packet["payloadHex"].as_str().unwrap()));
        payload.extend_from_slice(&raw[8..]);
    }
    assert_eq!(payload, bytes(request["payloadHex"].as_str().unwrap()));
    payload
}

fn assert_finished(
    decoder: &mut Decoder,
    done: Option<Done>,
    rows: &[Vec<Value>],
    observation: &Json,
) {
    assert_eq!(decoder.columns().unwrap(), expected_columns());
    assert_eq!(rows, expected_rows(observation));
    // The client DONE count is zero even when the server reports inserted rows.
    assert_eq!(
        done,
        Some(Done {
            status: 0,
            command: 0,
            row_count: 0
        })
    );
    assert_eq!(
        observation["callback"]["rowCount"].as_u64().unwrap(),
        rows.len() as u64
    );
    assert!(observation["callback"]["error"].is_null());
    assert_eq!(observation["callback"]["errors"], serde_json::json!([]));
    assert_eq!(decoder.pending_len(), 0);
    assert_eq!(decoder.push(&[], false).err(), Some(Error::Finished));
}

#[test]
fn captured_packets_preserve_metadata_null_empty_unicode_binary_rows_and_done() {
    let fixture = fixture();
    assert_eq!(fixture["runs"].as_array().unwrap().len(), 4);
    let mut cases = 0;
    for run in fixture["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap().iter().skip(1) {
            request_payload(observation);
            let mut decoder = Decoder::new(EomMode::RequireDone);
            let mut rows = Vec::new();
            let mut done = None;
            for packet in observation["messages"][2]["packets"].as_array().unwrap() {
                let raw = bytes(packet["rawHex"].as_str().unwrap());
                let chunk = decoder.push(&raw[8..], raw[1] & 1 != 0).unwrap();
                rows.extend(chunk.rows);
                done = chunk.done;
                assert_eq!(chunk.finished, raw[1] & 1 != 0);
            }
            assert_finished(&mut decoder, done, &rows, observation);
            cases += 1;
        }
    }
    assert_eq!(cases, 12);
}

#[test]
fn every_fragment_split_and_bytewise_feed_replays_all_retained_successes() {
    for run in fixture()["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap().iter().skip(1) {
            let payload = request_payload(observation);
            for split in 0..=payload.len() {
                let mut decoder = Decoder::new(EomMode::RequireDone);
                let first = decoder.push(&payload[..split], false).unwrap();
                assert!(!first.finished);
                let last = decoder.push(&payload[split..], true).unwrap();
                assert!(last.finished);
                let mut rows = first.rows;
                rows.extend(last.rows);
                assert_finished(&mut decoder, last.done, &rows, observation);
            }
            let mut decoder = Decoder::new(EomMode::RequireDone);
            let mut rows = Vec::new();
            let mut done = None;
            for (at, byte) in payload.iter().enumerate() {
                let chunk = decoder.push(&[*byte], at + 1 == payload.len()).unwrap();
                rows.extend(chunk.rows);
                done = chunk.done;
            }
            assert_finished(&mut decoder, done, &rows, observation);
        }
    }
}

#[test]
fn sql_server_4804_done_only_stream_is_rejected_and_poisoned_for_every_split() {
    for run in fixture()["runs"].as_array().unwrap() {
        let observation = &run["observations"][0];
        let payload = request_payload(observation);
        assert_eq!(payload, [vec![0xfd], vec![0; 12]].concat());
        assert_eq!(observation["callback"]["errors"][0]["number"], 4804);
        assert_eq!(observation["callback"]["errors"][0]["state"], 2);
        assert_eq!(observation["callback"]["errors"][0]["class"], 16);
        assert_eq!(
            observation["readback"]["sets"][0]["rows"],
            serde_json::json!([])
        );
        for mode in [EomMode::RequireDone, EomMode::AllowRowBoundary] {
            for split in 0..=payload.len() {
                let mut decoder = Decoder::new(mode);
                let error = match decoder.push(&payload[..split], false) {
                    Ok(chunk) => {
                        assert!(chunk.rows.is_empty());
                        decoder.push(&payload[split..], true).err()
                    }
                    Err(error) => Some(error),
                };
                assert_eq!(error, Some(Error::Malformed));
                assert!(decoder.columns().is_none());
                assert_eq!(decoder.pending_len(), 0);
                assert_eq!(decoder.push(&[], true).err(), Some(Error::Poisoned));
            }
        }
    }
}
