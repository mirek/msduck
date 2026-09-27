#[path = "../src/tvp.rs"]
mod tvp;

use serde_json::Value as Json;
use tvp::{Cell, ColumnType, Error, Limits, Value, decode_exact, decode_prefix};

fn hex_bytes(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn tvp_wire(request: &Json) -> Vec<u8> {
    let payload = hex_bytes(request["payloadHex"].as_str().unwrap());
    let mut offset = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let proc_chars = u16::from_le_bytes(payload[offset..offset + 2].try_into().unwrap()) as usize;
    offset += 2 + proc_chars * 2 + 2; // procedure and two-byte RPC option flags
    let param_chars = usize::from(payload[offset]);
    offset += 1 + param_chars * 2 + 1; // name and status flags
    assert_eq!(payload[offset], 0xf3);
    payload[offset..].to_vec()
}

fn reference_observations() -> Json {
    serde_json::from_str(include_str!("../../../reference/tvp-wire.json")).unwrap()
}

#[test]
fn retained_sql_server_requests_preserve_type_metadata_and_cell_bytes() {
    let fixture = reference_observations();
    for run in fixture["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap() {
            let request = &observation["request"];
            let wire = tvp_wire(request);
            let decoded = decode_exact(&wire, Limits::default()).unwrap();
            let expected = &request["tvp"];
            assert_eq!(
                String::from_utf16(&decoded.type_name.schema).unwrap(),
                expected["schema"]
            );
            assert_eq!(
                String::from_utf16(&decoded.type_name.name).unwrap(),
                expected["typeName"]
            );
            if expected["columnCount"] == 65535 {
                assert_eq!(decoded.value, Value::Null);
                continue;
            }
            let Value::Table { columns, rows } = decoded.value else {
                panic!("expected table TVP");
            };
            let expected_columns = expected["columns"].as_array().unwrap();
            assert_eq!(columns.len(), expected_columns.len());
            for (column, source) in columns.iter().zip(expected_columns) {
                assert_eq!(u64::from(column.user_type), source["userType"]);
                assert_eq!(u64::from(column.flags), source["flags"]);
                match &column.column_type {
                    ColumnType::IntN { width } => {
                        assert_eq!(source["typeInfo"]["kind"], 0x26);
                        assert_eq!(u64::from(*width), source["typeInfo"]["width"]);
                    }
                    ColumnType::NVarChar {
                        max_bytes,
                        collation,
                    } => {
                        assert_eq!(source["typeInfo"]["kind"], 0xe7);
                        assert_eq!(u64::from(*max_bytes), source["typeInfo"]["maxBytes"]);
                        assert_eq!(
                            hex_bytes(source["typeInfo"]["collationHex"].as_str().unwrap()),
                            collation
                        );
                    }
                    ColumnType::VarBinary { max_bytes } => {
                        assert_eq!(source["typeInfo"]["kind"], 0xa5);
                        assert_eq!(u64::from(*max_bytes), source["typeInfo"]["maxBytes"]);
                    }
                }
            }
            let expected_rows = expected["rows"].as_array().unwrap();
            assert_eq!(rows.len(), expected_rows.len());
            for (row, source) in rows.iter().zip(expected_rows) {
                let source = source.as_array().unwrap();
                assert_eq!(row.len(), source.len());
                for (cell, source) in row.iter().zip(source) {
                    if source["null"] == true {
                        assert_eq!(*cell, Cell::Null);
                    } else {
                        let expected = hex_bytes(source["valueHex"].as_str().unwrap());
                        assert_eq!(*cell, Cell::Bytes(&expected));
                    }
                }
            }
        }
    }
}

fn one_int_row() -> Vec<u8> {
    // TVPTYPE; empty names; one INTN(4) column; metadata end; row 42; rows end.
    let mut bytes = vec![0xf3, 0, 0, 0, 1, 0];
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend([0x26, 4, 0, 0, 1, 4, 42, 0, 0, 0, 0]);
    bytes
}

#[test]
fn every_truncation_and_trailing_byte_is_rejected() {
    let wire = one_int_row();
    assert!(decode_exact(&wire, Limits::default()).is_ok());
    for end in 0..wire.len() {
        assert!(
            decode_exact(&wire[..end], Limits::default()).is_err(),
            "end {end}"
        );
    }
    let mut trailing = wire.clone();
    trailing.push(0xee);
    assert_eq!(
        decode_exact(&trailing, Limits::default()),
        Err(Error::TrailingBytes)
    );
    let (_, consumed) = decode_prefix(&trailing, Limits::default()).unwrap();
    assert_eq!(consumed, wire.len());

    let mut later_parameters = wire.clone();
    later_parameters.extend(vec![0x7f; 1024]);
    let (_, consumed) = decode_prefix(
        &later_parameters,
        Limits {
            max_input_bytes: wire.len(),
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(consumed, wire.len());
}

#[test]
fn rejects_bad_tokens_lengths_counts_and_unsupported_metadata() {
    let wire = one_int_row();
    let mut changed = wire.clone();
    changed[0] = 0;
    assert_eq!(
        decode_exact(&changed, Limits::default()),
        Err(Error::InvalidType)
    );

    let mut changed = wire.clone();
    changed[4] = 0;
    assert_eq!(
        decode_exact(&changed, Limits::default()),
        Err(Error::InvalidColumnCount)
    );

    let mut changed = wire.clone();
    changed[12] = 0xff;
    assert_eq!(
        decode_exact(&changed, Limits::default()),
        Err(Error::UnsupportedColumnType(0xff))
    );

    let mut changed = wire.clone();
    changed[15] = 0x10;
    assert_eq!(
        decode_exact(&changed, Limits::default()),
        Err(Error::UnsupportedMetadata(0x10))
    );

    let mut changed = wire.clone();
    changed[16] = 0x02;
    assert_eq!(
        decode_exact(&changed, Limits::default()),
        Err(Error::UnexpectedToken(0x02))
    );

    let mut changed = wire.clone();
    changed[17] = 3;
    assert_eq!(
        decode_exact(&changed, Limits::default()),
        Err(Error::InvalidCellLength)
    );

    assert_eq!(
        decode_exact(
            &wire,
            Limits {
                max_input_bytes: wire.len() - 1,
                ..Limits::default()
            }
        ),
        Err(Error::InputLimit)
    );
    assert_eq!(
        decode_exact(
            &wire,
            Limits {
                max_columns: 0,
                ..Limits::default()
            }
        ),
        Err(Error::ColumnLimit)
    );
    assert_eq!(
        decode_exact(
            &wire,
            Limits {
                max_rows: 0,
                ..Limits::default()
            }
        ),
        Err(Error::RowLimit)
    );
    assert_eq!(
        decode_exact(
            &wire,
            Limits {
                max_cells: 0,
                ..Limits::default()
            }
        ),
        Err(Error::CellLimit)
    );
    assert_eq!(
        decode_exact(
            &wire,
            Limits {
                max_cell_bytes: 3,
                ..Limits::default()
            }
        ),
        Err(Error::CellLimit)
    );
}

#[test]
fn default_columns_omit_wire_data_and_null_differs_from_empty() {
    let mut wire = one_int_row();
    wire[10] = 0x00;
    wire[11] = 0x02; // fDefault
    wire.drain(17..22); // no length or bytes for this column
    let value = decode_exact(&wire, Limits::default()).unwrap();
    let Value::Table { rows, .. } = value.value else {
        panic!("expected table");
    };
    assert_eq!(rows, vec![vec![Cell::Default]]);

    let null = [0xf3, 0, 0, 0, 0xff, 0xff, 0, 0];
    assert_eq!(
        decode_exact(&null, Limits::default()).unwrap().value,
        Value::Null
    );
    assert_eq!(
        decode_exact(&null[..7], Limits::default()),
        Err(Error::Truncated)
    );
}

#[test]
fn malformed_wire_never_panics() {
    let wire = one_int_row();
    for index in 0..wire.len() {
        for replacement in [0, 1, 0xff] {
            let mut changed = wire.clone();
            changed[index] = replacement;
            let _ = decode_exact(&changed, Limits::default());
        }
    }
}
