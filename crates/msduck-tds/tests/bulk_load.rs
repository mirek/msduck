#[path = "../src/bulk_load.rs"]
mod bulk_load;

use bulk_load::{Decoder, EomMode, Error, Value};

fn column(name: &str, type_info: &[u8], flags: u16) -> Vec<u8> {
    let mut result = Vec::new();
    result.extend(0u32.to_le_bytes());
    result.extend(flags.to_le_bytes());
    result.extend(type_info);
    result.push(name.encode_utf16().count() as u8);
    result.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
    result
}

fn metadata(columns: &[Vec<u8>]) -> Vec<u8> {
    let mut result = vec![0x81];
    result.extend((columns.len() as u16).to_le_bytes());
    for column in columns {
        result.extend(column);
    }
    result
}

fn done() -> Vec<u8> {
    let mut result = vec![0xfd];
    result.extend(0u16.to_le_bytes());
    result.extend(0u16.to_le_bytes());
    result.extend(1u64.to_le_bytes());
    result
}

fn plp(chunks: &[&[u8]], total: u64) -> Vec<u8> {
    let mut result = total.to_le_bytes().to_vec();
    for chunk in chunks {
        result.extend((chunk.len() as u32).to_le_bytes());
        result.extend(*chunk);
    }
    result.extend(0u32.to_le_bytes());
    result
}

fn fixture() -> (Vec<u8>, Vec<Value>) {
    let collation = [9, 4, 0xd0, 0, 0x34];
    let mut nvarchar_max = vec![0xe7, 0xff, 0xff];
    nvarchar_max.extend(collation);
    let mut wire = metadata(&[
        column("id", &[0x26, 4], 1),
        column("unicode", &nvarchar_max, 1),
        column("binary", &[0xa5, 4, 0], 1),
        column("money", &[0x6e, 8], 1),
        column("numeric", &[0x6a, 5, 9, 2], 1),
        column("when", &[0x2a, 7], 1),
    ]);
    let unicode = [0x3d, 0xd8, 0x00, 0xde, 0x00, 0xd8]; // pair + isolated high surrogate
    wire.push(0xd1);
    wire.extend([4, 42, 0, 0, 0]);
    wire.extend(plp(&[&unicode[..2], &unicode[2..]], unicode.len() as u64));
    wire.extend([3, 0, 0, 0, 0xff]);
    wire.push(0); // NULL money
    wire.extend([5, 1, 0x39, 0x30, 0, 0]);
    wire.extend([8, 0, 0, 0, 0, 0, 0, 0, 0]);
    wire.extend(done());
    let values = vec![
        Value {
            bytes: Some(vec![42, 0, 0, 0]),
        },
        Value {
            bytes: Some(unicode.to_vec()),
        },
        Value {
            bytes: Some(vec![0, 0, 0xff]),
        },
        Value { bytes: None },
        Value {
            bytes: Some(vec![1, 0x39, 0x30, 0, 0]),
        },
        Value {
            bytes: Some(vec![0; 8]),
        },
    ];
    (wire, values)
}

#[test]
fn every_split_and_bytewise_feed_preserves_rows_metadata_and_done() {
    let (wire, expected) = fixture();
    for split in 0..=wire.len() {
        let mut decoder = Decoder::new(EomMode::RequireDone);
        let mut rows = decoder.push(&wire[..split], false).unwrap().rows;
        let last = decoder.push(&wire[split..], true).unwrap();
        rows.extend(last.rows);
        assert_eq!(rows, vec![expected.clone()], "split {split}");
        assert_eq!(last.done.unwrap().row_count, 1);
        assert!(last.finished);
        assert_eq!(decoder.pending_len(), 0);
        assert_eq!(
            decoder.columns().unwrap()[1].name_utf16,
            "unicode".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(decoder.push(&[], false).err(), Some(Error::Finished));
    }
    let mut decoder = Decoder::new(EomMode::RequireDone);
    let mut rows = Vec::new();
    for (at, byte) in wire.iter().enumerate() {
        rows.extend(decoder.push(&[*byte], at + 1 == wire.len()).unwrap().rows);
    }
    assert_eq!(rows, vec![expected]);
}

#[test]
fn complete_rows_are_emitted_before_final_eom_and_not_retained() {
    let meta = metadata(&[column("x", &[0x26, 4], 1)]);
    let row = [0xd1, 4, 1, 2, 3, 4];
    let mut decoder = Decoder::new(EomMode::RequireDone);
    let mut first = meta;
    first.extend(row);
    first.extend(row);
    let chunk = decoder.push(&first, false).unwrap();
    assert_eq!(chunk.rows.len(), 2);
    assert_eq!(decoder.pending_len(), 0);
    let chunk = decoder.push(&[0xd1, 4, 9], false).unwrap();
    assert!(chunk.rows.is_empty());
    assert_eq!(decoder.pending_len(), 3);
    let mut tail = vec![8, 7, 6];
    tail.extend(done());
    let chunk = decoder.push(&tail, true).unwrap();
    assert_eq!(chunk.rows[0][0].bytes, Some(vec![9, 8, 7, 6]));
    assert!(chunk.finished);
}

#[test]
fn done_is_required_unless_row_boundary_mode_is_explicit() {
    let mut wire = metadata(&[column("x", &[0x26, 4], 1)]);
    wire.extend([0xd1, 0]);
    let mut strict = Decoder::new(EomMode::RequireDone);
    assert_eq!(strict.push(&wire, true).err(), Some(Error::MissingDone));
    assert_eq!(strict.push(&done(), true).err(), Some(Error::Poisoned));
    let mut freetds = Decoder::new(EomMode::AllowRowBoundary);
    let result = freetds.push(&wire, true).unwrap();
    assert_eq!(result.rows, vec![vec![Value { bytes: None }]]);
    assert!(result.done.is_none());
    assert!(result.finished);
    let mut incomplete = Decoder::new(EomMode::AllowRowBoundary);
    assert_eq!(
        incomplete.push(&wire[..wire.len() - 1], true).err(),
        Some(Error::TruncatedEom)
    );
}

#[test]
fn malformed_streams_poison_the_decoder() {
    let valid = metadata(&[column("x", &[0x26, 4], 1)]);
    let cases = [
        (vec![0xd1], Error::Malformed),
        (vec![0x81, 0xff, 0xff], Error::ColumnLimit),
        (metadata(&[column("x", &[0xf1], 1)]), Error::UnsupportedType),
        (
            metadata(&[column("x", &[0x26, 4], 0x0800)]),
            Error::EncryptedColumn,
        ),
        ([valid.clone(), vec![0xd2]].concat(), Error::NbcRow),
        ([valid.clone(), vec![0xd1, 5]].concat(), Error::Malformed),
        (
            [valid.clone(), vec![0xd1, 4, 1, 2, 3, 4, 0xfe]].concat(),
            Error::Malformed,
        ),
        (
            [
                valid.clone(),
                vec![0xfd, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            ]
            .concat(),
            Error::NonFinalDone,
        ),
        (
            [valid.clone(), done(), vec![1]].concat(),
            Error::TrailingBytes,
        ),
    ];
    for (wire, expected) in cases {
        let mut decoder = Decoder::new(EomMode::RequireDone);
        assert_eq!(decoder.push(&wire, true).err(), Some(expected), "{wire:?}");
        assert_eq!(decoder.push(&[], false).err(), Some(Error::Poisoned));
    }
}

#[test]
fn truncated_eom_and_plp_lengths_fail_at_boundaries() {
    let (wire, _) = fixture();
    for len in 1..wire.len() {
        let mut decoder = Decoder::new(EomMode::RequireDone);
        // A prefix ending immediately after metadata or a ROW lacks DONE;
        // other prefixes end inside a token.
        assert!(decoder.push(&wire[..len], true).is_err(), "prefix {len}");
        assert_eq!(
            decoder.push(&wire[len..], true).err(),
            Some(Error::Poisoned)
        );
    }
    let mut type_info = vec![0xe7, 0xff, 0xff];
    type_info.extend([9, 4, 0xd0, 0, 0x34]);
    let mut base = metadata(&[column("n", &type_info, 1)]);
    base.push(0xd1);
    let mut mismatch = base.clone();
    mismatch.extend(plp(&[&[65, 0]], 4));
    let mut decoder = Decoder::new(EomMode::RequireDone);
    assert_eq!(decoder.push(&mismatch, true).err(), Some(Error::Malformed));
    let mut over_limit = base;
    over_limit.extend(((16 * 1024 * 1024 + 1) as u64).to_le_bytes());
    let mut decoder = Decoder::new(EomMode::RequireDone);
    assert_eq!(
        decoder.push(&over_limit, true).err(),
        Some(Error::TokenLimit)
    );
}

#[test]
fn scalar_families_preserve_raw_bytes_and_distinguish_empty_from_null() {
    let collation = [9, 4, 0xd0, 0, 0x34];
    let mut varchar = vec![0xa7, 6, 0];
    varchar.extend(collation);
    let columns = [
        column("fixed", &[0x38], 0),
        column("guid", &[0x24, 16], 1),
        column("date", &[0x28], 1),
        column("text", &varchar, 1),
        column("variant", &[0x62, 9, 0, 0, 0], 1),
    ];
    let mut wire = metadata(&columns);
    wire.push(0xd1);
    wire.extend([1, 2, 3, 4]);
    wire.push(16);
    wire.extend([0x42; 16]);
    wire.extend([3, 7, 8, 9]);
    wire.extend([0, 0]); // empty varchar
    wire.extend(u32::MAX.to_le_bytes()); // NULL variant
    wire.extend(done());
    let mut decoder = Decoder::new(EomMode::RequireDone);
    let row = decoder.push(&wire, true).unwrap().rows.remove(0);
    assert_eq!(row[0].bytes, Some(vec![1, 2, 3, 4]));
    assert_eq!(row[1].bytes, Some(vec![0x42; 16]));
    assert_eq!(row[2].bytes, Some(vec![7, 8, 9]));
    assert_eq!(row[3].bytes, Some(vec![]));
    assert_eq!(row[4].bytes, None);
}

#[test]
fn plp_unknown_length_null_and_legacy_lob_are_decoded_losslessly() {
    let mut unicode_max = vec![0xe7, 0xff, 0xff];
    unicode_max.extend([9, 4, 0xd0, 0, 0x34]);
    let mut legacy_text = vec![0x23];
    legacy_text.extend(u32::MAX.to_le_bytes());
    legacy_text.extend([9, 4, 0xd0, 0, 0x34]);
    legacy_text.extend([1, 1, 0, b't', 0]); // one table-name part
    let mut second = Vec::new();
    second.extend(0u32.to_le_bytes());
    second.extend(1u16.to_le_bytes());
    second.extend(legacy_text); // table-name parts follow TYPE_INFO
    second.push(6);
    second.extend("legacy".encode_utf16().flat_map(u16::to_le_bytes));
    let mut wire = metadata(&[column("max", &unicode_max, 1), second]);
    wire.push(0xd1);
    wire.extend(plp(&[&[0x00, 0xd8], &[0x42, 0x00]], u64::MAX - 1));
    wire.push(1); // text pointer length
    wire.push(0x42);
    wire.extend([0; 8]); // timestamp
    wire.extend(3u32.to_le_bytes());
    wire.extend(b"abc");
    wire.push(0xd1);
    wire.extend(u64::MAX.to_le_bytes()); // PLP NULL
    wire.push(0); // text pointer NULL
    wire.extend(done());
    let mut decoder = Decoder::new(EomMode::RequireDone);
    let rows = decoder.push(&wire, true).unwrap().rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0].bytes, Some(vec![0x00, 0xd8, 0x42, 0x00]));
    assert_eq!(rows[0][1].bytes, Some(b"abc".to_vec()));
    assert_eq!(rows[1][0].bytes, None);
    assert_eq!(rows[1][1].bytes, None);
}

#[test]
fn invalid_plp_and_numeric_payloads_fail_before_eom() {
    let mut unicode_max = vec![0xe7, 0xff, 0xff];
    unicode_max.extend([9, 4, 0xd0, 0, 0x34]);
    let base = metadata(&[column("max", &unicode_max, 1)]);
    for body in [
        plp(&[&[65]], u64::MAX - 1), // odd UTF-16 byte count
        plp(&[&[65, 0]], 4),         // declared length disagrees with chunks
    ] {
        let mut wire = base.clone();
        wire.push(0xd1);
        wire.extend(body);
        let mut decoder = Decoder::new(EomMode::RequireDone);
        assert_eq!(decoder.push(&wire, false).err(), Some(Error::Malformed));
    }
    let mut wire = metadata(&[column("d", &[0x6a, 5, 9, 2], 1)]);
    wire.extend([0xd1, 5, 2, 0, 0, 0, 0]); // invalid decimal sign
    let mut decoder = Decoder::new(EomMode::RequireDone);
    assert_eq!(decoder.push(&wire, false).err(), Some(Error::Malformed));
}

#[test]
fn legacy_ntext_max_metadata_keeps_isolated_utf16_units() {
    let mut type_info = vec![0x63];
    type_info.extend(u32::MAX.to_le_bytes());
    type_info.extend([9, 4, 0xd0, 0, 0x34]);
    let mut column = Vec::new();
    column.extend(0u32.to_le_bytes());
    column.extend(1u16.to_le_bytes());
    column.extend(type_info);
    column.extend([1, 1, 0, b't', 0]); // table-name part
    column.extend([1, b'n', 0]);
    let mut wire = metadata(&[column]);
    wire.extend([0xd1, 1, 0x42]); // text pointer
    wire.extend([0; 8]); // timestamp
    wire.extend(2u32.to_le_bytes());
    wire.extend([0x00, 0xd8]); // isolated high surrogate
    wire.extend(done());
    let mut decoder = Decoder::new(EomMode::RequireDone);
    let chunk = decoder.push(&wire, true).unwrap();
    assert_eq!(chunk.rows[0][0].bytes, Some(vec![0x00, 0xd8]));
}
