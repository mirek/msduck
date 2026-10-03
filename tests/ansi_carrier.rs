use duckdb::{
    Connection,
    arrow::{
        array::{Array, ArrayRef, BinaryArray, LargeBinaryArray, StructArray, UInt32Array},
        buffer::NullBuffer,
        datatypes::{DataType, Field, Fields},
    },
    core::{DataChunkHandle, LogicalTypeId as Id},
    types::Value,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::{WritableVector, data_chunk_to_arrow, record_batch_to_duckdb_data_chunk},
};
use msduck::ansi_carrier::Plan;
use msduck_core::ansi_bytes::{AnsiView, EncodingIdentity as Encoding};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn plan(encoding: Encoding) -> Plan {
    Plan::new(encoding, 1024 * 1024, 8 * 1024 * 1024).unwrap()
}
fn raw(value: Option<&[u8]>, encoding: Encoding) -> Option<AnsiView<'_>> {
    AnsiView::nullable(encoding, value, usize::MAX).unwrap()
}
fn roundtrip(db: &Connection, plan: Plan, bytes: Option<&[u8]>) {
    let query = format!(
        "SELECT {}({}(?))",
        plan.unpack_function(),
        plan.pack_function()
    );
    let actual: Option<Vec<u8>> = db
        .query_row(
            &query,
            [plan.bind(raw(bytes, plan.encoding())).unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actual.as_deref(), bytes);
    let query = format!("SELECT {}(?) AS v", plan.pack_function());
    let value: Value = db
        .query_row(
            &query,
            [plan.bind(raw(bytes, plan.encoding())).unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let actual_value = plan.read_value(&value).unwrap();
    assert_eq!(actual_value.as_ref().map(|v| v.view().bytes()), bytes);
    let mut statement = db.prepare(&query).unwrap();
    let batches: Vec<_> = statement
        .query_arrow([plan.bind(raw(bytes, plan.encoding())).unwrap()])
        .unwrap()
        .collect();
    assert_eq!(batches.len(), 1);
    let actual = plan.read_array(batches[0].column(0).as_ref(), 0).unwrap();
    assert_eq!(actual.as_ref().map(|v| v.view().bytes()), bytes);
    assert!(
        actual
            .as_ref()
            .is_none_or(|v| v.view().encoding() == plan.encoding())
    );
}

#[test]
fn native_vectors_and_arrow_preserve_every_cp_byte_null_and_empty() {
    let file = DatabaseFile::new();
    let db = Connection::open(&file.0).unwrap();
    for encoding in [Encoding::Cp1251, Encoding::Cp1252] {
        let plan = plan(encoding);
        plan.register(&db).unwrap();
        let table = if encoding == Encoding::Cp1251 {
            "cp1251_cells"
        } else {
            "cp1252_cells"
        };
        db.execute_batch(&format!("CREATE TABLE {table}(id INTEGER, v STRUCT(__msduck_ansi_encoding UINTEGER, __msduck_ansi_bytes BLOB))")).unwrap();
        roundtrip(&db, plan, None);
        roundtrip(&db, plan, Some(&[]));
        for byte in 0..=255 {
            roundtrip(&db, plan, Some(&[byte]));
            db.execute(
                &format!(
                    "INSERT INTO {table} VALUES (?, {}(?))",
                    plan.pack_function()
                ),
                duckdb::params![
                    byte as i32,
                    plan.bind(raw(Some(&[byte]), encoding)).unwrap()
                ],
            )
            .unwrap();
        }
        let bytes: Vec<u8> = (0..=255).collect();
        roundtrip(&db, plan, Some(&bytes));
    }
    drop(db);
    let db = Connection::open(&file.0).unwrap();
    for (encoding, table) in [
        (Encoding::Cp1251, "cp1251_cells"),
        (Encoding::Cp1252, "cp1252_cells"),
    ] {
        let plan = plan(encoding);
        let mut statement = db
            .prepare(&format!("SELECT v FROM {table} ORDER BY id"))
            .unwrap();
        let batches: Vec<_> = statement.query_arrow([]).unwrap().collect();
        let values = plan.read_batch(batches[0].column(0).as_ref()).unwrap();
        assert_eq!(values.len(), 256);
        for (byte, value) in values.iter().enumerate() {
            assert_eq!(value.as_ref().unwrap().view().bytes(), [byte as u8]);
            assert_eq!(value.as_ref().unwrap().view().encoding(), encoding);
        }
    }
}

struct DatabaseFile(PathBuf);
impl DatabaseFile {
    fn new() -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&directory).unwrap();
        use ring::rand::SecureRandom;
        let mut random = [0; 16];
        ring::rand::SystemRandom::new().fill(&mut random).unwrap();
        Self(directory.join(format!(
            "ansi-carrier-{}.duckdb",
            uuid::Uuid::from_bytes(random)
        )))
    }
}
impl Drop for DatabaseFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("duckdb.wal"));
    }
}

#[test]
fn persisted_invalid_utf8_reopens_without_text_decoding_or_binary_aliasing() {
    let file = DatabaseFile::new();
    let plan = plan(Encoding::Utf8);
    let cells: [Option<&[u8]>; 6] = [
        None,
        Some(&[]),
        Some(&[0xc3, 0x28]),
        Some(&[0xed, 0xa0, 0x80]),
        Some(&[0xe0, 0x80, 0xaf]),
        Some(&[65, 0xed, 0xa0, 0x80, 66]),
    ];
    {
        let db = Connection::open(&file.0).unwrap();
        plan.register(&db).unwrap();
        db.execute_batch("CREATE TABLE native_ansi(id INTEGER, v STRUCT(__msduck_ansi_encoding UINTEGER, __msduck_ansi_bytes BLOB)); CREATE TABLE ordinary_binary(v BLOB)").unwrap();
        for (id, bytes) in cells.iter().enumerate() {
            db.execute(
                &format!(
                    "INSERT INTO native_ansi VALUES (?, {}(?))",
                    plan.pack_function()
                ),
                duckdb::params![id as i32, plan.bind(raw(*bytes, Encoding::Utf8)).unwrap()],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO ordinary_binary VALUES (?)",
            [&[0xed, 0xa0, 0x80][..]],
        )
        .unwrap();
    }
    let db = Connection::open(&file.0).unwrap();
    plan.register(&db).unwrap();
    let mut query = db.prepare("SELECT v FROM native_ansi ORDER BY id").unwrap();
    let batches: Vec<_> = query.query_arrow([]).unwrap().collect();
    let values = plan.read_batch(batches[0].column(0).as_ref()).unwrap();
    assert_eq!(values.len(), cells.len());
    for (value, expected) in values.iter().zip(cells) {
        assert_eq!(value.as_ref().map(|v| v.view().bytes()), expected);
    }
    let mut query = db.prepare("SELECT v FROM ordinary_binary").unwrap();
    let batches: Vec<_> = query.query_arrow([]).unwrap().collect();
    assert!(plan.read_batch(batches[0].column(0).as_ref()).is_err());
}

fn array(
    tags: Vec<Option<u32>>,
    bytes: Vec<Option<&[u8]>>,
    validity: Option<NullBuffer>,
    large: bool,
) -> StructArray {
    let bytes: ArrayRef = if large {
        Arc::new(LargeBinaryArray::from(bytes))
    } else {
        Arc::new(BinaryArray::from(bytes))
    };
    let fields: Fields = vec![
        Field::new("__msduck_ansi_encoding", DataType::UInt32, true),
        Field::new("__msduck_ansi_bytes", bytes.data_type().clone(), true),
    ]
    .into();
    StructArray::new(
        fields,
        vec![Arc::new(UInt32Array::from(tags)), bytes],
        validity,
    )
}

#[test]
fn arrow_slices_parent_validity_large_binary_and_zero_rows_are_typed() {
    let plan = plan(Encoding::Cp1251);
    for large in [false, true] {
        let values = array(
            vec![Some(1251), None, Some(1251), Some(1251)],
            vec![Some(&[0x98]), None, Some(&[]), Some(&[0xff])],
            Some(NullBuffer::from(vec![true, false, true, true])),
            large,
        );
        let sliced = values.slice(1, 3);
        let actual = plan.read_batch(&sliced).unwrap();
        assert!(actual[0].is_none());
        assert!(actual[1].as_ref().unwrap().view().bytes().is_empty());
        assert_eq!(actual[2].as_ref().unwrap().view().bytes(), [0xff]);
        let empty = values.slice(0, 0);
        plan.validate_array(&empty).unwrap();
        assert!(plan.read_batch(&empty).unwrap().is_empty());
        assert!(plan.read_array(&empty, 0).is_err());
        assert!(plan.read_array(&values, 4).is_err());
    }
}

#[test]
fn malformed_shapes_null_children_and_wrong_tags_are_explicit_errors() {
    let plan = plan(Encoding::Cp1251);
    for values in [
        array(vec![None], vec![Some(&[])], None, false),
        array(vec![Some(1251)], vec![None], None, false),
        array(vec![Some(1252)], vec![Some(&[])], None, false),
    ] {
        assert!(plan.read_array(&values, 0).is_err());
        assert!(plan.read_batch(&values).is_err());
    }
    let binary = BinaryArray::from(vec![None::<&[u8]>]);
    assert!(plan.read_array(&binary, 0).is_err());
    let utf16 = StructArray::new(
        vec![Field::new("__msduck_utf16le", DataType::Binary, true)].into(),
        vec![Arc::new(BinaryArray::from(vec![None::<&[u8]>]))],
        None,
    );
    assert!(plan.read_batch(&utf16).is_err());
    for value in [
        Value::Blob(vec![]),
        Value::Struct(vec![("__msduck_utf16le".into(), Value::Blob(vec![]))].into()),
        Value::Struct(
            vec![
                ("__msduck_ansi_encoding".into(), Value::UInt(1251)),
                ("__msduck_ansi_bytes".into(), Value::Null),
            ]
            .into(),
        ),
    ] {
        assert!(plan.read_value(&value).is_err());
    }
    let db = Connection::open_in_memory().unwrap();
    plan.register(&db).unwrap();
    for value in [
        "struct_pack(__msduck_ansi_encoding := 1252::UINTEGER, __msduck_ansi_bytes := ''::BLOB)",
        "struct_pack(__msduck_ansi_encoding := NULL::UINTEGER, __msduck_ansi_bytes := ''::BLOB)",
        "struct_pack(__msduck_ansi_encoding := 1251::UINTEGER, __msduck_ansi_bytes := NULL::BLOB)",
    ] {
        assert!(
            db.query_row::<Vec<u8>, _, _>(
                &format!("SELECT {}({value})", plan.unpack_function()),
                [],
                |r| r.get(0)
            )
            .is_err()
        );
    }
    roundtrip(&db, plan, Some(&[0x98]));
}

#[test]
fn unsupported_plans_and_encoding_mismatches_fail_before_empty_payloads() {
    for identity in [
        Encoding::Opaque(1251),
        Encoding::Opaque(1252),
        Encoding::Opaque(65001),
        Encoding::Opaque(u32::MAX),
    ] {
        assert!(Plan::new(identity, 0, 0).is_err());
    }
    assert!(Plan::new(Encoding::Cp1251, 2, 1).is_err());
    let plan = Plan::new(Encoding::Cp1251, 0, 0).unwrap();
    assert!(matches!(plan.bind(None).unwrap(), Value::Null));
    assert!(
        matches!(plan.bind(raw(Some(&[]),Encoding::Cp1251)).unwrap(),Value::Blob(b) if b.is_empty())
    );
    assert!(plan.bind(raw(Some(&[]), Encoding::Cp1252)).is_err());
}

#[test]
fn cell_and_batch_preflight_limits_are_enforced_on_native_and_arrow_paths() {
    let db = Connection::open_in_memory().unwrap();
    let plan = Plan::new(Encoding::Utf8, 2, 3).unwrap();
    plan.register(&db).unwrap();
    roundtrip(&db, plan, Some(&[0xc3, 0xa9]));
    assert!(plan.bind(raw(Some(&[1, 2, 3]), Encoding::Utf8)).is_err());
    assert!(
        db.prepare(&format!("SELECT {}(?)", plan.pack_function()))
            .unwrap()
            .query_arrow([&[1, 2, 3][..]])
            .is_err()
    );
    let within = array(
        vec![Some(65001), Some(65001)],
        vec![Some(&[1, 2]), Some(&[3])],
        None,
        false,
    );
    assert_eq!(plan.read_batch(&within).unwrap().len(), 2);
    let over = array(
        vec![Some(65001), Some(65001)],
        vec![Some(&[1, 2]), Some(&[3, 4])],
        None,
        false,
    );
    assert!(plan.read_batch(&over).is_err());
    assert!(
        db.prepare(&format!(
            "SELECT {}(v) FROM (VALUES ('ab'::BLOB),('cd'::BLOB)) d(v)",
            plan.pack_function()
        ))
        .unwrap()
        .query_arrow([])
        .is_err()
    );
    assert!(db.prepare(&format!("SELECT {}(v) FROM (VALUES (struct_pack(__msduck_ansi_encoding := 65001::UINTEGER, __msduck_ansi_bytes := 'ab'::BLOB)), (struct_pack(__msduck_ansi_encoding := 65001::UINTEGER, __msduck_ansi_bytes := 'cd'::BLOB))) d(v)",plan.unpack_function())).unwrap().query_arrow([]).is_err());
    let over_cell = array(vec![Some(65001)], vec![Some(&[1, 2, 3])], None, false);
    assert!(plan.read_batch(&over_cell).is_err());
}

#[test]
fn native_errors_preserve_statement_atomicity_and_explicit_transaction_rollback() {
    let db = Connection::open_in_memory().unwrap();
    let plan = Plan::new(Encoding::Cp1252, 2, 16384).unwrap();
    plan.register(&db).unwrap();
    db.execute_batch(
        "CREATE TABLE rows(v STRUCT(__msduck_ansi_encoding UINTEGER, __msduck_ansi_bytes BLOB))",
    )
    .unwrap();
    db.execute(
        &format!(
            "INSERT INTO rows SELECT {}('ok'::BLOB)",
            plan.pack_function()
        ),
        [],
    )
    .unwrap();
    let insert = format!(
        "INSERT INTO rows SELECT {}(CASE WHEN i=7000 THEN 'bad'::BLOB ELSE 'ok'::BLOB END) FROM range(10000) d(i)",
        plan.pack_function()
    );
    assert!(db.execute(&insert, []).is_err());
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM rows", [], |r| r.get(0))
            .unwrap(),
        1
    );
    db.execute_batch("BEGIN TRANSACTION").unwrap();
    db.execute(
        &format!(
            "INSERT INTO rows SELECT {}('tx'::BLOB)",
            plan.pack_function()
        ),
        [],
    )
    .unwrap();
    assert!(db.execute(&insert, []).is_err());
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM rows", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

struct VolatileBytes;
impl VScalar for VolatileBytes {
    type State = Arc<AtomicUsize>;
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Bigint.into()],
            Id::Blob.into(),
        )]
    }
    fn volatile() -> bool {
        true
    }
    fn invoke(
        count: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use duckdb::core::Inserter;
        let source = input.flat_vector(0);
        let result = output.flat_vector();
        for row in 0..input.len() {
            let value = unsafe { source.as_slice_with_len::<i64>(input.len())[row] };
            count.fetch_add(1, Ordering::SeqCst);
            result.insert(row, value.to_le_bytes().as_slice());
        }
        Ok(())
    }
}

#[test]
fn native_packing_evaluates_volatile_operands_once_across_multiple_chunks() {
    let db = Connection::open_in_memory().unwrap();
    let plan = plan(Encoding::Cp1251);
    plan.register(&db).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    db.register_scalar_function_with_state::<VolatileBytes>("volatile_native_bytes", &count)
        .unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE once AS SELECT i, {}(volatile_native_bytes(i)) v FROM range(10000) d(i)",
        plan.pack_function()
    ))
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 10000);
    let mut statement = db
        .prepare(&format!(
            "SELECT i, {}(v) FROM once ORDER BY i",
            plan.unpack_function()
        ))
        .unwrap();
    let values = statement
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .unwrap();
    for (expected, row) in values.enumerate() {
        let (id, bytes) = row.unwrap();
        assert_eq!(id, expected as i64);
        assert_eq!(bytes, id.to_le_bytes());
    }
    assert_eq!(count.load(Ordering::SeqCst), 10000);
}

#[test]
fn arrow_native_vector_bridge_preserves_tags_parent_null_and_payload_bytes() {
    let plan = plan(Encoding::Utf8);
    let values = array(
        vec![Some(65001), None, Some(65001)],
        vec![Some(&[0xed, 0xa0, 0x80]), None, Some(&[])],
        Some(NullBuffer::from(vec![true, false, true])),
        false,
    );
    let schema = Arc::new(duckdb::arrow::datatypes::Schema::new(vec![Field::new(
        "v",
        values.data_type().clone(),
        true,
    )]));
    let batch =
        duckdb::arrow::record_batch::RecordBatch::try_new(schema.clone(), vec![Arc::new(values)])
            .unwrap();
    let mut chunk = DataChunkHandle::new(&[plan.storage_type()]);
    record_batch_to_duckdb_data_chunk(&batch, &mut chunk).unwrap();
    let native = data_chunk_to_arrow(&chunk).unwrap();
    let values = plan.read_batch(native.column(0).as_ref()).unwrap();
    assert_eq!(
        values[0].as_ref().unwrap().view().bytes(),
        [0xed, 0xa0, 0x80]
    );
    assert!(values[1].is_none());
    assert!(values[2].as_ref().unwrap().view().bytes().is_empty());
}

fn object_end(text: &str, start: usize) -> usize {
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    for (offset, &byte) in text.as_bytes()[start..].iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false
            } else if byte == b'\\' {
                escaped = true
            } else if byte == b'"' {
                quoted = false
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return start + offset + 1;
                    }
                }
                _ => {}
            }
        }
    }
    panic!("unterminated original reference observation")
}

fn observations(reference: &str, select: impl Fn(&str) -> bool) -> Vec<serde_json::Value> {
    // Select complete observations without modifying raw fixtures or legitimate
    // isolated UTF16 surrogates in unrelated capacity diagnostics.
    let mut selected = Vec::new();
    let mut position = 0;
    while let Some(offset) = reference[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(reference, start);
        let case_start = start + "{\"case\":".len();
        let case_end = object_end(reference, case_start);
        let case: serde_json::Value =
            serde_json::from_str(&reference[case_start..case_end]).unwrap();
        if select(case["name"].as_str().unwrap()) {
            selected.push(serde_json::from_str(&reference[start..end]).unwrap());
        }
        position = end;
    }
    selected
}
fn hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn original_reference_native_cells_and_fragmented_max_survive_storage_unchanged() {
    let file = DatabaseFile::new();
    let db = Connection::open(&file.0).unwrap();
    let encodings = [Encoding::Cp1251, Encoding::Cp1252, Encoding::Utf8];
    let tables = ["retained_1251", "retained_1252", "retained_utf8"];
    let mut expected: [Vec<Option<Vec<u8>>>; 3] = std::array::from_fn(|_| Vec::new());
    for (encoding, table) in encodings.into_iter().zip(tables) {
        plan(encoding).register(&db).unwrap();
        db.execute_batch(&format!("CREATE TABLE {table}(id INTEGER, v STRUCT(__msduck_ansi_encoding UINTEGER, __msduck_ansi_bytes BLOB))")).unwrap();
    }
    let encoding = include_str!("../reference/bulk-character-encoding.json");
    let conversion = include_str!("../reference/bulk-character-conversion.json");
    let capacity = include_str!("../reference/bulk-character-capacity.json");
    for (reference, expected) in [
        (
            encoding,
            "0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83",
        ),
        (
            conversion,
            "f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27",
        ),
        (
            capacity,
            "cd82a8853bcac9f3fee98c1fb6c6f17564443918868ece1f1d9f66b3abb0ec92",
        ),
    ] {
        let digest = ring::digest::digest(&ring::digest::SHA256, reference.as_bytes());
        assert_eq!(digest.as_ref(), hex(expected));
    }
    let mut selected: Vec<(Encoding, serde_json::Value)> = observations(encoding, |n| {
        n.ends_with("-same-max") || n.starts_with("utf-8-invalid-") && n.ends_with("-same")
    })
    .into_iter()
    .map(|o| {
        let name = o["case"]["name"].as_str().unwrap();
        let encoding = if name.starts_with("CP1251") {
            Encoding::Cp1251
        } else if name.starts_with("CP1252") {
            Encoding::Cp1252
        } else {
            Encoding::Utf8
        };
        (encoding, o)
    })
    .collect();
    assert_eq!(selected.len(), 32);
    let malformed = observations(conversion, |n| {
        n.starts_with("utf8-") && n.ends_with("-utf8")
    });
    assert_eq!(malformed.len(), 64);
    selected.extend(malformed.into_iter().map(|o| (Encoding::Utf8, o)));
    let max = observations(capacity, |n| {
        n == "cp1251-max-to-utf8-varchar-max" || n == "utf8-max-to-cp1252-varchar-max"
    });
    assert_eq!(max.len(), 8);
    selected.extend(max.into_iter().map(|o| {
        let target = if o["case"]["name"] == "utf8-max-to-cp1252-varchar-max" {
            Encoding::Cp1252
        } else {
            Encoding::Utf8
        };
        (target, o)
    }));
    let mut invalid = 0;
    let mut large = 0;
    let mut cells = 0;
    for (encoding, observation) in selected {
        // These are actual native target bytes, not source bytes, display or
        // SQL UTF16 projection. Failed SQL loads have no cells to preserve.
        for row in observation["readback"]["result"]["sets"][1]["rows"]
            .as_array()
            .unwrap()
        {
            let value = &row[2];
            let bytes = if value.is_null() {
                None
            } else {
                assert_eq!(value["kind"], "binary");
                let bytes = hex(value["value"].as_str().unwrap());
                if encoding == Encoding::Utf8 && std::str::from_utf8(&bytes).is_err() {
                    invalid += 1;
                }
                if bytes.len() > 8000 {
                    large += 1;
                }
                Some(bytes)
            };
            let index = encodings.iter().position(|x| *x == encoding).unwrap();
            let table = tables[index];
            roundtrip(&db, plan(encoding), bytes.as_deref());
            db.execute(
                &format!(
                    "INSERT INTO {table} VALUES (?, {}(?))",
                    plan(encoding).pack_function()
                ),
                duckdb::params![
                    expected[index].len() as i32,
                    plan(encoding)
                        .bind(raw(bytes.as_deref(), encoding))
                        .unwrap()
                ],
            )
            .unwrap();
            expected[index].push(bytes);
            cells += 1;
        }
    }
    assert_eq!((invalid, large, cells), (40, 16, 252));
    assert_eq!(expected.each_ref().map(Vec::len), [28, 44, 180]);
    drop(db);
    let db = Connection::open(&file.0).unwrap();
    for ((encoding, table), expected) in encodings.into_iter().zip(tables).zip(expected) {
        let mut statement = db
            .prepare(&format!("SELECT v FROM {table} ORDER BY id"))
            .unwrap();
        let batches: Vec<_> = statement.query_arrow([]).unwrap().collect();
        let actual: Vec<_> = batches
            .iter()
            .flat_map(|b| plan(encoding).read_batch(b.column(0).as_ref()).unwrap())
            .collect();
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(
                actual.as_ref().map(|v| v.view().bytes()),
                expected.as_deref()
            );
        }
    }
}
