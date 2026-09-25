#[path = "../src/for_xml.rs"]
mod for_xml;

use for_xml::{Error, Mode, Outcome, Plan, Row, TEXT_COLUMN_NAME, encode};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/for-xml-wire.json")).unwrap()
}

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn success_cases(run: &Value) {
    let ordinary = run
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "text")
        .unwrap();
    let text_row = ordinary["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|token| token["token"] == "ROW")
        .unwrap();
    let pointer = hex(text_row["framing"]["pointerHex"].as_str().unwrap());
    let timestamp: [u8; 8] = hex(text_row["framing"]["timestampHex"].as_str().unwrap())
        .try_into()
        .unwrap();

    for case in run
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["name"] != "attribute order error")
    {
        let metadata = case["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .find(|token| token["token"] == "COLMETADATA")
            .unwrap();
        let name = metadata["name"].as_str().unwrap();
        let flags = metadata["flags"].as_u64().unwrap() as u16;
        let count = case["tokens"].as_array().unwrap().last().unwrap()["count"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let values: Vec<Vec<u16>> = case["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|token| token["token"] == "ROW")
            .map(|token| token["value"].as_str().unwrap().encode_utf16().collect())
            .collect();
        let mut output = Vec::new();
        if metadata["typeId"] == 0x63 {
            let collation: [u8; 5] = hex(metadata["typeInfo"]["collation"].as_str().unwrap())
                .try_into()
                .unwrap();
            let table = metadata["typeInfo"]["table"][0].as_str().unwrap();
            let rows: Vec<_> = values.iter().map(|value| Row::NText(value)).collect();
            encode(
                &mut output,
                Outcome::Success(Plan {
                    mode: Mode::NText {
                        collation,
                        table,
                        pointer: &pointer,
                        timestamp,
                    },
                    name,
                    flags,
                    rows: &rows,
                    source_row_count: count,
                }),
            )
            .unwrap();
        } else {
            assert_eq!(metadata["typeId"], 0xf1);
            let chunk_values: Vec<Vec<Vec<u16>>> = case["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|token| token["token"] == "ROW")
                .zip(values.iter())
                .map(|(token, units)| {
                    let mut offset = 0;
                    let chunks = token["framing"]["chunks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|length| {
                            let length = length.as_u64().unwrap() as usize / 2;
                            let chunk = units[offset..offset + length].to_vec();
                            offset += length;
                            chunk
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(offset, units.len());
                    chunks
                })
                .collect();
            let chunk_refs: Vec<Vec<&[u16]>> = chunk_values
                .iter()
                .map(|chunks| chunks.iter().map(Vec::as_slice).collect())
                .collect();
            let rows: Vec<_> = chunk_refs.iter().map(|chunks| Row::Xml(chunks)).collect();
            encode(
                &mut output,
                Outcome::Success(Plan {
                    mode: Mode::Xml,
                    name,
                    flags,
                    rows: &rows,
                    source_row_count: count,
                }),
            )
            .unwrap();
        }
        assert_eq!(
            output,
            hex(case["responses"][0]["payloadHex"].as_str().unwrap()),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn captured_text_xml_empty_nested_and_large_tokens_match_all_four_databases() {
    let fixture = fixture();
    for runs in [&fixture["runs"], &fixture["independentRuns"]] {
        for run in runs.as_array().unwrap() {
            success_cases(run);
        }
    }
}

#[test]
fn validation_error_emits_no_success_tokens() {
    let mut output = vec![0xaa];
    encode(&mut output, Outcome::ValidationError).unwrap();
    assert_eq!(output, [0xaa]);
    let fixture = fixture();
    for runs in [&fixture["runs"], &fixture["independentRuns"]] {
        for run in runs.as_array().unwrap() {
            let invalid = run
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["name"] == "attribute order error")
                .unwrap();
            let tokens = invalid["tokens"].as_array().unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .map(|token| token["token"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["ERROR", "DONE"]
            );
            assert_eq!(tokens[0]["number"], 6852);
        }
    }
}

#[test]
fn invalid_shapes_fail_before_mutating_the_output() {
    let mut output = vec![0x55];
    let pointer = [0x11; 16];
    let wrong = [Row::Xml(&[])];
    let plan = |rows| Plan {
        mode: Mode::NText {
            collation: [9, 4, 208, 0, 52],
            table: "x",
            pointer: &pointer,
            timestamp: [0; 8],
        },
        name: TEXT_COLUMN_NAME,
        flags: 1,
        rows,
        source_row_count: 1,
    };
    assert_eq!(
        encode(&mut output, Outcome::Success(plan(&wrong))),
        Err(Error::WrongRowKind)
    );
    assert_eq!(output, [0x55]);

    let empty_chunk: &[u16] = &[];
    let chunks = [empty_chunk];
    let xml_rows = [Row::Xml(&chunks)];
    assert_eq!(
        encode(
            &mut output,
            Outcome::Success(Plan {
                mode: Mode::Xml,
                name: "",
                flags: 3,
                rows: &xml_rows,
                source_row_count: 1,
            })
        ),
        Err(Error::EmptyPlpChunk)
    );
    assert_eq!(output, [0x55]);

    let huge_pointer = [0; 256];
    let empty_rows = [];
    assert_eq!(
        encode(
            &mut output,
            Outcome::Success(Plan {
                mode: Mode::NText {
                    collation: [0; 5],
                    table: "x",
                    pointer: &huge_pointer,
                    timestamp: [0; 8]
                },
                name: TEXT_COLUMN_NAME,
                flags: 1,
                rows: &empty_rows,
                source_row_count: 0,
            })
        ),
        Err(Error::InvalidPointer)
    );
    assert_eq!(output, [0x55]);

    let too_large = vec![0u16; for_xml::MAX_ENCODED_BYTES / 2];
    let rows = [Row::NText(&too_large)];
    assert_eq!(
        encode(&mut output, Outcome::Success(plan(&rows))),
        Err(Error::ResultTooLarge)
    );
    assert_eq!(output, [0x55]);
}

#[test]
fn xml_plp_keeps_caller_chunk_boundaries_and_logical_done_count() {
    let first = [b'A' as u16];
    let second = [b'B' as u16];
    let chunks: [&[u16]; 2] = [&first, &second];
    let rows = [Row::Xml(&chunks)];
    let mut output = Vec::new();
    encode(
        &mut output,
        Outcome::Success(Plan {
            mode: Mode::Xml,
            name: "",
            flags: 3,
            rows: &rows,
            source_row_count: 7,
        }),
    )
    .unwrap();
    assert_eq!(
        &output[12..],
        &hex("d1feffffffffffffff02000000410002000000420000000000fd1000c1000700000000000000")
    );
}
