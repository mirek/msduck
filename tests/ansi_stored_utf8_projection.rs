use duckdb::{Connection, types::Value};
use msduck::ansi_carrier::{Plan as Storage, projection::Plan};
use msduck_core::{
    ansi_bytes::{AnsiView, EncodingIdentity as Encoding},
    ansi_conversion::{ProjectionLimits, ProjectionTarget},
};
use std::path::Path;

const CELL: usize = 12401;
const CHUNK: usize = 1024 * 1024;
const NAME: &str = "__msduck_ansi_project_stored";
const SQL: &str = "SELECT __msduck_ansi_project_stored(__msduck_ansi_pack_65001(?))";
fn storage() -> Storage {
    Storage::new(Encoding::Utf8, CELL, CHUNK).unwrap()
}
fn plan() -> Plan {
    Plan::stored_utf8_to_sql_utf16(
        ProjectionLimits {
            input_bytes: CELL,
            output_bytes: CELL * 2,
        },
        CHUNK,
        CHUNK * 2,
    )
    .unwrap()
}
fn setup(db: &Connection) {
    storage().register(db).unwrap();
    plan().register(db, NAME).unwrap();
}
fn hex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0);
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn bind(bytes: Option<&[u8]>) -> Value {
    storage()
        .bind(AnsiView::nullable(Encoding::Utf8, bytes, CELL).unwrap())
        .unwrap()
}
fn unicode(value: &Value) -> Option<Vec<u8>> {
    if matches!(value, Value::Null) {
        return None;
    }
    let Value::Struct(fields) = value else {
        panic!("not a UTF16 carrier")
    };
    assert_eq!(fields.keys().count(), 1);
    let Value::Blob(bytes) = fields.get(&"__msduck_utf16le".to_owned()).unwrap() else {
        panic!("not a UTF16 payload")
    };
    assert_eq!(bytes.len() % 2, 0);
    Some(bytes.clone())
}
fn assert_unicode_type(kind: &duckdb::arrow::datatypes::DataType) {
    use duckdb::arrow::datatypes::DataType;
    let DataType::Struct(fields) = kind else {
        panic!("not a UTF16 struct")
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name(), "__msduck_utf16le");
    assert!(matches!(
        fields[0].data_type(),
        DataType::Binary | DataType::LargeBinary
    ));
}
fn unicode_arrow(array: &dyn duckdb::arrow::array::Array, row: usize) -> Option<Vec<u8>> {
    use duckdb::arrow::array::{BinaryArray, LargeBinaryArray, StructArray};
    assert_unicode_type(array.data_type());
    assert!(row < array.len());
    if array.is_null(row) {
        return None;
    }
    let structure = array.as_any().downcast_ref::<StructArray>().unwrap();
    let payload = structure.column(0);
    assert!(!payload.is_null(row));
    let bytes = if let Some(b) = payload.as_any().downcast_ref::<BinaryArray>() {
        b.value(row)
    } else {
        payload
            .as_any()
            .downcast_ref::<LargeBinaryArray>()
            .unwrap()
            .value(row)
    };
    assert_eq!(bytes.len() % 2, 0);
    Some(bytes.to_vec())
}
#[test]
fn callback_replays_all_original_stored_native_unicode_and_boundary_results() {
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("reference/stored-utf8-projection.json"),
    )
    .unwrap();
    assert_eq!(
        ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref(),
        hex("04b4b49603116046ac475d31a244207aa61054a672aacaffa976a106800961e9")
    );
    let fixture: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let mut project = db.prepare(SQL).unwrap();
    let mut native = db
        .prepare("SELECT __msduck_ansi_unpack_65001(__msduck_ansi_pack_65001(?))")
        .unwrap();
    let (mut checked, mut failed) = (0, 0);
    for run in fixture["runs"].as_array().unwrap() {
        for o in run["observations"].as_array().unwrap() {
            let input = o["input"].as_str().map(hex);
            let original = input.clone();
            let actual_native: Option<Vec<u8>> = native
                .query_row([bind(input.as_deref())], |r| r.get(0))
                .unwrap();
            assert_eq!(actual_native, input, "native {}", o["input"]);
            let expected_native = o["result"]["sets"][0]["rows"][0][0]["value"]
                .as_str()
                .map(hex);
            assert_eq!(actual_native, expected_native);
            let actual = project.query_row([bind(input.as_deref())], |r| r.get::<_, Value>(0));
            let errors = o["result"]["errors"].as_array().unwrap();
            if errors.is_empty() {
                let expected = o["result"]["sets"][1]["rows"][0][0]["value"]
                    .as_str()
                    .map(hex);
                assert_eq!(
                    unicode(&actual.unwrap()),
                    expected,
                    "Unicode {}",
                    o["input"]
                );
            } else {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0]["number"], 9833);
                assert!(
                    actual
                        .unwrap_err()
                        .to_string()
                        .contains("invalid stored UTF8 conversion boundary"),
                    "{}",
                    o["input"]
                );
                failed += 1;
            }
            assert_eq!(input, original);
            checked += 1;
        }
    }
    assert_eq!((checked, failed), (6088, 612));
}
#[test]
fn frozen_eof_controls_use_the_real_callback_while_strict_projection_stays_strict() {
    let source = include_str!("../scripts/capture-stored-utf8-projection.mjs");
    let prefix = "export const eofControls = Object.freeze(";
    let start = source.find(prefix).unwrap() + prefix.len();
    let end = start
        + source[start..]
            .find(".map(control=>Object.freeze(control)))")
            .unwrap();
    let controls: Vec<(String, Option<String>)> =
        serde_json::from_str(&source[start..end]).unwrap();
    assert_eq!(controls.len(), 33);
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    assert_eq!(plan().source_encoding(), Encoding::Utf8);
    assert_eq!(plan().target(), ProjectionTarget::SqlUtf16);
    for (input, expected) in controls {
        let actual = db.query_row(SQL, [bind(Some(&hex(&input)))], |r| r.get::<_, Value>(0));
        if let Some(expected) = expected {
            assert_eq!(unicode(&actual.unwrap()), Some(hex(&expected)), "{input}");
        } else {
            assert!(
                actual
                    .unwrap_err()
                    .to_string()
                    .contains("invalid stored UTF8 conversion boundary"),
                "{input}"
            );
        }
    }
    Plan::new(
        Encoding::Utf8,
        ProjectionTarget::SqlUtf16,
        ProjectionLimits {
            input_bytes: CELL,
            output_bytes: CELL * 2,
        },
        CHUNK,
        CHUNK * 2,
    )
    .unwrap()
    .register(&db, "__msduck_ansi_project_strict")
    .unwrap();
    assert!(
        db.query_row(
            "SELECT __msduck_ansi_project_strict(__msduck_ansi_pack_65001(?))",
            [bind(Some(&hex("eda080")))],
            |r| r.get::<_, Value>(0)
        )
        .unwrap_err()
        .to_string()
        .contains("invalid UTF8")
    );
    assert_eq!(
        unicode(
            &db.query_row(SQL, [bind(Some(&hex("eda080")))], |r| r.get::<_, Value>(0))
                .unwrap()
        ),
        Some(hex("fdfffdff"))
    );
}
#[test]
fn declaration_limits_null_empty_and_original_input_precede_eof_fitting() {
    for (input, output, input_chunk, output_chunk) in [
        (1, 2, 0, 2),
        (1, 2, 1, 1),
        (1, 16 * 1024 * 1024 + 1, 1, 64 * 1024 * 1024),
        (1, 2, 1, 64 * 1024 * 1024 + 1),
    ] {
        assert!(
            Plan::stored_utf8_to_sql_utf16(
                ProjectionLimits {
                    input_bytes: input,
                    output_bytes: output
                },
                input_chunk,
                output_chunk
            )
            .is_err()
        );
    }
    let db = Connection::open_in_memory().unwrap();
    storage().register(&db).unwrap();
    Plan::stored_utf8_to_sql_utf16(
        ProjectionLimits {
            input_bytes: 0,
            output_bytes: 0,
        },
        0,
        0,
    )
    .unwrap()
    .register(&db, NAME)
    .unwrap();
    for input in [None, Some(&[][..])] {
        let actual = db
            .query_row(SQL, [bind(input)], |r| r.get::<_, Value>(0))
            .unwrap();
        assert_eq!(unicode(&actual).as_deref(), input);
    }
    // C2 would fit to empty, but the original byte still exceeds this plan.
    let error = db
        .query_row(SQL, [bind(Some(&[0xc2]))], |r| r.get::<_, Value>(0))
        .unwrap_err()
        .to_string();
    assert!(!error.contains("conversion boundary"));
    assert!(
        error.contains("native ANSI payload 1 exceeds limit 0"),
        "{error}"
    );
    let mut empty = db.prepare("SELECT __msduck_ansi_project_stored(__msduck_ansi_pack_65001(NULL::BLOB)) v WHERE false").unwrap();
    let batches: Vec<_> = empty.query_arrow([]).unwrap().collect();
    assert_unicode_type(&empty.column_type(0));
    for batch in batches {
        assert_unicode_type(batch.column(0).data_type());
        assert_eq!(batch.num_rows(), 0);
    }
}
#[test]
fn malformed_physical_identity_and_children_are_rejected_even_in_stored_mode() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    for value in [
        "struct_pack(__msduck_ansi_encoding := 1252::UINTEGER, __msduck_ansi_bytes := 'A'::BLOB)",
        "struct_pack(__msduck_ansi_encoding := NULL::UINTEGER, __msduck_ansi_bytes := 'A'::BLOB)",
        "struct_pack(__msduck_ansi_encoding := 65001::UINTEGER, __msduck_ansi_bytes := NULL::BLOB)",
        "'A'::BLOB",
    ] {
        assert!(
            db.query_row(&format!("SELECT {NAME}({value})"), [], |r| r
                .get::<_, Value>(0))
                .is_err()
        );
    }
}
#[test]
fn late_boundary_and_output_limits_keep_insert_atomic_and_connection_reusable() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    db.execute_batch("CREATE TABLE projected(v STRUCT(__msduck_utf16le BLOB))")
        .unwrap();
    let failure = db.execute_batch("INSERT INTO projected SELECT __msduck_ansi_project_stored(__msduck_ansi_pack_65001(CASE WHEN i=9999 THEN from_hex('80') ELSE 'A'::BLOB END)) FROM range(10000) t(i)");
    assert!(
        failure
            .unwrap_err()
            .to_string()
            .contains("invalid stored UTF8 conversion boundary")
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM projected", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    for (index, output, chunk) in [(0, 3, 6), (1, 4, 4)] {
        let name = format!("__msduck_ansi_project_limit_{index}");
        Plan::stored_utf8_to_sql_utf16(
            ProjectionLimits {
                input_bytes: 4,
                output_bytes: output,
            },
            8,
            chunk,
        )
        .unwrap()
        .register(&db, &name)
        .unwrap();
        let error = db.execute_batch(&format!("INSERT INTO projected SELECT {name}(__msduck_ansi_pack_65001(b)) FROM (VALUES (from_hex('f0908080')), (from_hex('f0908080'))) t(b)")).unwrap_err().to_string();
        assert!(
            error.contains(if index == 0 {
                "Output payload 4 exceeds limit 3"
            } else {
                "native ANSI payload 8 exceeds limit 4"
            }),
            "{error}"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM projected", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    db.execute_batch("BEGIN; INSERT INTO projected SELECT __msduck_ansi_project_stored(__msduck_ansi_pack_65001(from_hex('eda080'))); ROLLBACK").unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM projected", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        unicode(
            &db.query_row(SQL, [bind(Some(&hex("f0908080")))], |r| r
                .get::<_, Value>(0))
                .unwrap()
        ),
        Some(hex("00d800dc"))
    );
}

struct DatabaseFile(std::path::PathBuf);
impl DatabaseFile {
    fn new() -> Self {
        use ring::rand::SecureRandom;
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&directory).unwrap();
        let mut nonce = [0; 16];
        ring::rand::SystemRandom::new().fill(&mut nonce).unwrap();
        Self(directory.join(format!(
            "ansi-stored-utf8-{}.duckdb",
            uuid::Uuid::from_bytes(nonce)
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
fn native_and_projected_bytes_survive_restart_and_arrow_without_text_repair() {
    let file = DatabaseFile::new();
    let cells = vec![
        (None, None),
        (Some(vec![]), Some(vec![])),
        (Some(hex("eda080")), Some(hex("fdfffdff"))),
        (Some(hex("41e1a0")), Some(vec![])),
        (Some(hex("f0908080")), Some(hex("00d800dc"))),
    ];
    {
        let db = Connection::open(&file.0).unwrap();
        setup(&db);
        db.execute_batch("CREATE TABLE native(id INTEGER, v STRUCT(__msduck_ansi_encoding UINTEGER, __msduck_ansi_bytes BLOB)); CREATE TABLE converted(id INTEGER,v STRUCT(__msduck_utf16le BLOB))").unwrap();
        for (i, (input, _)) in cells.iter().enumerate() {
            db.execute(
                "INSERT INTO native VALUES (?,__msduck_ansi_pack_65001(?))",
                duckdb::params![i as i32, bind(input.as_deref())],
            )
            .unwrap();
        }
    }
    {
        let db = Connection::open(&file.0).unwrap();
        setup(&db);
        for (i, (input, expected)) in cells.iter().enumerate() {
            let native: Option<Vec<u8>> = db
                .query_row(
                    "SELECT __msduck_ansi_unpack_65001(v) FROM native WHERE id=?",
                    [i as i32],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(&native, input);
            let projected: Value = db
                .query_row(
                    "SELECT __msduck_ansi_project_stored(v) FROM native WHERE id=?",
                    [i as i32],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(&unicode(&projected), expected);
            db.execute("INSERT INTO converted SELECT id,__msduck_ansi_project_stored(v) FROM native WHERE id=?",[i as i32]).unwrap();
        }
    }
    {
        let db = Connection::open(&file.0).unwrap();
        let mut q = db.prepare("SELECT v FROM converted ORDER BY id").unwrap();
        let mut seen = 0;
        for batch in q.query_arrow([]).unwrap() {
            assert_unicode_type(batch.column(0).data_type());
            for row in 0..batch.num_rows() {
                assert_eq!(unicode_arrow(batch.column(0).as_ref(), row), cells[seen].1);
                seen += 1;
            }
        }
        assert_eq!(seen, cells.len());
    }
}

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
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
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        assert!(input.len() <= source.capacity());
        for row in 0..input.len() {
            assert!(!source.try_row_is_null(row as u64)?);
            // SAFETY: signature BIGINT, checked capacity and non-NULL slot.
            let id = unsafe { source.as_slice_with_len::<i64>(input.len())[row] };
            count.fetch_add(1, Ordering::SeqCst);
            match id % 5 {
                0 => result.set_null(row),
                1 => result.insert(row, &[][..]),
                2 => result.insert(row, &[0xed, 0xa0, 0x80][..]),
                3 => result.insert(row, &[0xf0, 0x90, 0x80, 0x80][..]),
                _ => result.insert(row, b"A".as_slice()),
            }
        }
        Ok(())
    }
}
#[test]
fn stored_projection_evaluates_original_once_and_preserves_nulls_across_chunks() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let count = Arc::new(AtomicUsize::new(0));
    db.register_scalar_function_with_state::<VolatileBytes>("volatile_stored_bytes", &count)
        .unwrap();
    db.execute_batch("CREATE TABLE once AS SELECT i,__msduck_ansi_project_stored(__msduck_ansi_pack_65001(volatile_stored_bytes(i))) v FROM range(10000) t(i)").unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 10000);
    let expected = [
        None,
        Some(vec![]),
        Some(hex("fdfffdff")),
        Some(hex("00d800dc")),
        Some(hex("4100")),
    ];
    let mut seen = 0;
    let mut q = db.prepare("SELECT v FROM once ORDER BY i").unwrap();
    for batch in q.query_arrow([]).unwrap() {
        for row in 0..batch.num_rows() {
            assert_eq!(
                unicode_arrow(batch.column(0).as_ref(), row),
                expected[seen % 5]
            );
            seen += 1;
        }
    }
    assert_eq!(seen, 10000);
    assert_eq!(count.load(Ordering::SeqCst), 10000);
}
