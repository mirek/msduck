use msduck_tds::MAX_MESSAGE;
#[path = "../src/ansi_value.rs"]
mod ansi_value;
use ansi_value::{Declaration, Value, encode};

fn declaration(fixed: bool, capacity: Option<u16>) -> Declaration {
    Declaration::new(if fixed { 0xaf } else { 0xa7 }, capacity).unwrap()
}
fn frame(declaration: Declaration, value: Value<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    encode(&mut out, declaration, value, MAX_MESSAGE).unwrap();
    out
}

#[test]
fn bounded_null_empty_fixed_and_all_bytes_are_exact() {
    let variable = declaration(false, Some(8000));
    assert_eq!(frame(variable, Value::Null), [255, 255]);
    assert_eq!(frame(variable, Value::Bytes(&[])), [0, 0]);
    let bytes: Vec<_> = (0..=255).collect();
    let mut expected = vec![0, 1];
    expected.extend(&bytes);
    assert_eq!(frame(variable, Value::Bytes(&bytes)), expected);
    assert_eq!(
        frame(declaration(true, Some(3)), Value::Bytes(b"A  ")),
        [3, 0, b'A', b' ', b' ']
    );
    assert_eq!(frame(declaration(true, Some(3)), Value::Null), [255, 255]);
}

#[test]
fn max_null_empty_known_length_and_original_chunk_boundaries() {
    let max = declaration(false, None);
    assert_eq!(frame(max, Value::Null), [255; 8]);
    assert_eq!(frame(max, Value::Bytes(&[])), [0; 12]);
    assert_eq!(frame(max, Value::Chunks(&[])), [0; 12]);
    let chunks: &[&[u8]] = &[&[0xf0, 0x9f, 0xa6], &[0x86, 0xc3, 0x28]];
    assert_eq!(
        frame(max, Value::Chunks(chunks)),
        [
            6, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0xf0, 0x9f, 0xa6, 3, 0, 0, 0, 0x86, 0xc3, 0x28, 0,
            0, 0, 0
        ]
    );
    let bytes: Vec<_> = (0..=255).collect();
    let mut expected = 256u64.to_le_bytes().to_vec();
    expected.extend(256u32.to_le_bytes());
    expected.extend(&bytes);
    expected.extend([0; 4]);
    assert_eq!(frame(max, Value::Bytes(&bytes)), expected);
}

#[test]
fn malformed_native_bytes_are_not_decoded_or_repaired() {
    for bytes in [
        &[0x80][..],
        &[0xc0, 0xaf],
        &[0xf0, 0x9f, 0x92],
        &[0xc3, 0x28],
        &[0xed, 0xa0, 0x80],
    ] {
        let bounded = frame(declaration(false, Some(8)), Value::Bytes(bytes));
        assert_eq!(&bounded[2..], bytes);
        let max = frame(declaration(false, None), Value::Bytes(bytes));
        assert_eq!(&max[12..max.len() - 4], bytes);
    }
}

#[test]
fn unsupported_families_and_invalid_declarations_are_rejected() {
    for type_id in [0x23, 0xe7, 0xef, 0xa5, 0xad, 0x38, 0] {
        assert!(Declaration::new(type_id, Some(1)).is_err());
    }
    for type_id in [0xaf, 0xa7] {
        for capacity in [0, 8001, u16::MAX] {
            assert!(Declaration::new(type_id, Some(capacity)).is_err());
        }
    }
    assert!(Declaration::new(0xaf, None).is_err());
}

#[test]
fn every_preflight_failure_preserves_existing_output() {
    let variable = declaration(false, Some(2));
    let fixed = declaration(true, Some(2));
    let max = declaration(false, None);
    for (decl, value, limit) in [
        (variable, Value::Bytes(b"abc"), MAX_MESSAGE),
        (fixed, Value::Bytes(b"a"), MAX_MESSAGE),
        (fixed, Value::Bytes(b""), MAX_MESSAGE),
        (variable, Value::Chunks(&[b"a"]), MAX_MESSAGE),
        (max, Value::Chunks(&[b"a", b""]), MAX_MESSAGE),
        (max, Value::Bytes(b"a"), 19),
        (max, Value::Null, 10),
        (variable, Value::Null, 4),
    ] {
        let mut out = vec![1, 2, 3];
        let before = out.clone();
        assert!(encode(&mut out, decl, value, limit).is_err());
        assert_eq!(out, before);
    }
    let mut out = vec![1, 2, 3];
    encode(&mut out, max, Value::Bytes(b"a"), 20).unwrap();
    assert_eq!(out.len(), 20);
    let huge = vec![7; MAX_MESSAGE];
    let mut out = vec![1, 2, 3];
    assert!(encode(&mut out, max, Value::Bytes(&huge), usize::MAX).is_err());
    assert_eq!(out, [1, 2, 3]);
    // Payload fits alone but complete chunk framing does not.
    let many: Vec<&[u8]> = vec![b"a"; MAX_MESSAGE / 4];
    assert!(encode(&mut out, max, Value::Chunks(&many), usize::MAX).is_err());
    assert_eq!(out, [1, 2, 3]);
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2));
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
struct Read<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Read<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let bytes = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        bytes
    }
    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn short(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn long(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn wide(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }
}
#[derive(Clone, Copy)]
struct Column {
    kind: u8,
    capacity: u16,
}
fn columns(r: &mut Read<'_>) -> Vec<Column> {
    let count = r.short();
    (0..count)
        .map(|_| {
            r.take(6); // userType + flags; they are not supplied to this value writer.
            let kind = r.byte();
            let capacity = match kind {
                0x38 => 4,
                0x26 => u16::from(r.byte()),
                0xa7 | 0xe7 => {
                    let n = r.short();
                    r.take(5);
                    n
                }
                0xa5 => r.short(),
                _ => panic!("unexpected captured result type {kind:x}"),
            };
            let units = r.byte();
            r.take(usize::from(units) * 2);
            Column { kind, capacity }
        })
        .collect()
}
fn value<'a>(r: &mut Read<'a>, c: Column) -> Option<Vec<&'a [u8]>> {
    if c.kind == 0x38 {
        return Some(vec![r.take(4)]);
    }
    if c.kind == 0x26 {
        let n = r.byte();
        return (n != 0).then(|| vec![r.take(usize::from(n))]);
    }
    if c.capacity != u16::MAX {
        let n = r.short();
        return (n != u16::MAX).then(|| vec![r.take(usize::from(n))]);
    }
    let total = r.wide();
    if total == u64::MAX {
        return None;
    }
    // The retained SQL Server result values use a known total, not PLP_UNKNOWN.
    assert_ne!(total, u64::MAX - 1);
    let mut chunks = Vec::new();
    loop {
        let n = r.long();
        if n == 0 {
            break;
        }
        chunks.push(r.take(n as usize));
    }
    assert_eq!(chunks.iter().map(|b| b.len() as u64).sum::<u64>(), total);
    Some(chunks)
}

#[test]
fn all_four_sql_server_readbacks_reproduce_actual_ansi_value_frames() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/bulk-character-encoding.json"
    ))
    .unwrap();
    let runs = fixture["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4);
    let mut checked = 0;
    let mut bitmap_nulls = 0;
    let mut split_character = false;
    let mut malformed = [false; 2];
    for run in runs {
        assert_eq!(run["observations"].as_array().unwrap().len(), 33);
        for observation in run["observations"].as_array().unwrap() {
            let mut wire = Vec::new();
            for packet in observation["readback"]["packets"].as_array().unwrap() {
                if packet["direction"] != "in" {
                    continue;
                }
                let packet = hex(packet["rawHex"].as_str().unwrap());
                assert_eq!(packet[0], 4);
                assert_eq!(
                    usize::from(u16::from_be_bytes([packet[2], packet[3]])),
                    packet.len()
                );
                wire.extend_from_slice(&packet[8..]);
            }
            let mut r = Read {
                bytes: &wire,
                pos: 0,
            };
            let mut cols = Vec::new();
            while r.pos < wire.len() {
                match r.byte() {
                    0x81 => cols = columns(&mut r),
                    0xa9 => {
                        let length = r.short();
                        r.take(usize::from(length)); // ORDER, unrelated to value framing.
                    }
                    0xfd..=0xff => {
                        r.take(12);
                    }
                    token @ (0xd1 | 0xd2) => {
                        let nulls = if token == 0xd2 {
                            r.take(cols.len().div_ceil(8)).to_vec()
                        } else {
                            Vec::new()
                        };
                        let mut ansi = None;
                        let mut native = None;
                        for (i, c) in cols.iter().enumerate() {
                            let bitmap_null =
                                !nulls.is_empty() && nulls[i / 8] & (1 << (i % 8)) != 0;
                            let start = r.pos;
                            let chunks = if bitmap_null { None } else { value(&mut r, *c) };
                            if c.kind == 0xa7 {
                                let decl = Declaration::new(
                                    c.kind,
                                    (c.capacity != u16::MAX).then_some(c.capacity),
                                )
                                .unwrap();
                                let written = match &chunks {
                                    None => frame(decl, Value::Null),
                                    Some(chunks) if c.capacity == u16::MAX => {
                                        frame(decl, Value::Chunks(chunks))
                                    }
                                    Some(chunks) => frame(decl, Value::Bytes(chunks[0])),
                                };
                                if bitmap_null {
                                    assert_eq!(r.pos, start); // NBCROW emits no NULL value frame.
                                    bitmap_nulls += 1;
                                } else {
                                    assert_eq!(
                                        written,
                                        wire[start..r.pos],
                                        "{}",
                                        observation["case"]["name"]
                                    );
                                }
                                if let Some(chunks) = &chunks {
                                    split_character |= chunks.windows(2).any(|p| {
                                        p[0].ends_with(&[0xf0, 0x9f, 0xa6])
                                            && p[1].starts_with(&[0x86])
                                    });
                                }
                                ansi = Some(chunks.map(|c| c.concat()));
                                checked += 1;
                            } else if c.kind == 0xa5 {
                                native = Some(chunks.map(|c| c.concat()));
                            }
                        }
                        if let Some(ansi) = ansi {
                            assert_eq!(Some(ansi.clone()), native, "native binary projection");
                            malformed[0] |= ansi.as_deref() == Some(&[0xc3, 0x28]);
                            malformed[1] |= ansi.as_deref() == Some(&[0xed, 0xa0, 0x80]);
                        }
                    }
                    token => panic!("unexpected captured readback token {token:x}"),
                }
            }
        }
    }
    assert_eq!(checked, 312); // 78 ANSI result values in each of four original runs.
    assert!(bitmap_nulls > 0);
    assert!(split_character);
    assert_eq!(malformed, [true, true]);
}
