use duckdb::{Connection, types::Value};
use msduck::ansi_carrier::{Plan as Storage, projection::Plan};
use msduck_core::{
    ansi_bytes::{AnsiView, EncodingIdentity as Encoding},
    ansi_conversion::{ProjectionLimits, ProjectionTarget as Target},
};

const CELL: usize = 1024 * 1024;
const CHUNK: usize = 8 * CELL;
fn storage(source: Encoding) -> Storage {
    Storage::new(source, CELL, CHUNK).unwrap()
}
fn plan(source: Encoding, target: Target) -> Plan {
    Plan::new(
        source,
        target,
        ProjectionLimits {
            input_bytes: CELL,
            output_bytes: CELL,
        },
        CHUNK,
        CHUNK,
    )
    .unwrap()
}
fn actual(value: &Value, target: Target) -> Option<Vec<u8>> {
    if matches!(value, Value::Null) {
        return None;
    }
    Some(match target {
        Target::Native(e) => {
            storage(e)
                .read_value(value)
                .unwrap()
                .unwrap()
                .into_parts()
                .1
        }
        Target::SqlUtf16 => {
            let Value::Struct(fields) = value else {
                panic!("expected UTF16 carrier")
            };
            assert_eq!(fields.keys().count(), 1);
            let Some(Value::Blob(bytes)) = fields.get(&"__msduck_utf16le".to_owned()) else {
                panic!("expected UTF16 byte payload")
            };
            assert_eq!(bytes.len() % 2, 0);
            bytes.clone()
        }
    })
}
fn check(
    db: &Connection,
    source: Encoding,
    target: Target,
    bytes: Option<&[u8]>,
    expected: Option<&[u8]>,
) {
    let value: Value = db
        .query_row(
            &format!(
                "SELECT __msduck_ansi_project_check({}(?))",
                storage(source).pack_function()
            ),
            [storage(source)
                .bind(AnsiView::nullable(source, bytes, CELL).unwrap())
                .unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actual(&value, target).as_deref(), expected);
}

#[test]
fn native_and_utf16_carriers_preserve_null_empty_units_and_materialization() {
    for (source, target, input, expected) in [
        (
            Encoding::Cp1251,
            Target::Native(Encoding::Cp1252),
            vec![0x98, 0xa9],
            vec![0x3f, 0xa9],
        ),
        (
            Encoding::Cp1252,
            Target::SqlUtf16,
            vec![0x81, 0x80],
            vec![0x81, 0, 0xac, 0x20],
        ),
        (
            Encoding::Utf8,
            Target::SqlUtf16,
            vec![0xf0, 0x90, 0x80, 0x80],
            vec![0, 0xd8, 0, 0xdc],
        ),
        (
            Encoding::Utf8,
            Target::Native(Encoding::Utf8),
            vec![0, 0xc3, 0xa9],
            vec![0, 0xc3, 0xa9],
        ),
    ] {
        let db = Connection::open_in_memory().unwrap();
        storage(source).register(&db).unwrap();
        plan(source, target)
            .register(&db, "__msduck_ansi_project_check")
            .unwrap();
        check(&db, source, target, None, None);
        check(&db, source, target, Some(&[]), Some(&[]));
        check(&db, source, target, Some(&input), Some(&expected));
        db.execute_batch(&format!("CREATE OR REPLACE TABLE projected AS SELECT __msduck_ansi_project_check({}(from_hex('{}'))) v",storage(source).pack_function(),input.iter().map(|b|format!("{b:02x}")).collect::<String>())).unwrap();
        let value: Value = db
            .query_row("SELECT v FROM projected", [], |r| r.get(0))
            .unwrap();
        assert_eq!(actual(&value, target), Some(expected));
        let mut statement = db.prepare("SELECT v FROM projected WHERE false").unwrap();
        let batches: Vec<_> = statement.query_arrow([]).unwrap().collect();
        assert!(batches.iter().all(|b| b.num_rows() == 0));
    }
}

#[test]
fn unsupported_plans_names_tags_null_children_and_malformed_bytes_are_explicit() {
    let zero = ProjectionLimits {
        input_bytes: 0,
        output_bytes: 0,
    };
    for e in [Encoding::Cp1251, Encoding::Cp1252, Encoding::Opaque(65001)] {
        assert!(Plan::new(Encoding::Utf8, Target::Native(e), zero, 0, 0).is_err());
    }
    assert!(Plan::new(Encoding::Opaque(1252), Target::SqlUtf16, zero, 0, 0).is_err());
    assert!(
        Plan::new(
            Encoding::Cp1252,
            Target::SqlUtf16,
            ProjectionLimits {
                input_bytes: 2,
                output_bytes: 3
            },
            1,
            4
        )
        .is_err()
    );
    let db = Connection::open_in_memory().unwrap();
    storage(Encoding::Utf8).register(&db).unwrap();
    let p = plan(Encoding::Utf8, Target::SqlUtf16);
    for n in [
        "project",
        "__msduck_ansi_project_bad;SELECT 1",
        "__msduck_ansi_project_é",
    ] {
        assert!(p.register(&db, n).is_err());
    }
    p.register(&db, "__msduck_ansi_project_check").unwrap();
    for bytes in ["80", "c228", "eda080", "f09080"] {
        let sql = format!(
            "SELECT __msduck_ansi_project_check({}(from_hex('{bytes}')))",
            storage(Encoding::Utf8).pack_function()
        );
        let error = db
            .query_row(&sql, [], |r| r.get::<_, Value>(0))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid UTF8"), "{error}");
    }
    for expression in [
        "struct_pack(__msduck_ansi_encoding := 1252::UINTEGER, __msduck_ansi_bytes := 'A'::BLOB)",
        "struct_pack(__msduck_ansi_encoding := NULL::UINTEGER, __msduck_ansi_bytes := 'A'::BLOB)",
        "struct_pack(__msduck_ansi_encoding := 65001::UINTEGER, __msduck_ansi_bytes := NULL::BLOB)",
        "'A'::BLOB",
    ] {
        assert!(
            db.query_row(
                &format!("SELECT __msduck_ansi_project_check({expression})"),
                [],
                |r| r.get::<_, Value>(0)
            )
            .is_err()
        );
    }
    assert_eq!(
        db.query_row("SELECT 42", [], |r| r.get::<_, i32>(0))
            .unwrap(),
        42
    );
}

#[test]
fn chunk_and_cell_limits_fail_atomically_and_leave_connection_usable() {
    let db = Connection::open_in_memory().unwrap();
    storage(Encoding::Cp1252).register(&db).unwrap();
    for (index, (input, output, input_chunk, output_chunk)) in
        [(0, 2, 2, 4), (2, 1, 4, 4), (2, 4, 2, 8), (2, 4, 4, 4)]
            .into_iter()
            .enumerate()
    {
        let name = format!("__msduck_ansi_project_limit_{index}");
        let p = Plan::new(
            Encoding::Cp1252,
            Target::SqlUtf16,
            ProjectionLimits {
                input_bytes: input,
                output_bytes: output,
            },
            input_chunk,
            output_chunk,
        )
        .unwrap();
        p.register(&db, &name).unwrap();
        db.execute_batch("CREATE OR REPLACE TABLE atomic(v STRUCT(__msduck_utf16le BLOB))")
            .unwrap();
        let result=db.execute_batch(&format!("INSERT INTO atomic SELECT {name}(__msduck_ansi_pack_1252(b)) FROM (VALUES ('ab'::BLOB),('cd'::BLOB)) t(b)"));
        assert!(result.is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM atomic", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let exact = Plan::new(
        Encoding::Cp1252,
        Target::SqlUtf16,
        ProjectionLimits {
            input_bytes: 2,
            output_bytes: 4,
        },
        4,
        8,
    )
    .unwrap();
    exact.register(&db, "__msduck_ansi_project_limit").unwrap();
    db.execute_batch("BEGIN; INSERT INTO atomic SELECT __msduck_ansi_project_limit(__msduck_ansi_pack_1252('ab'::BLOB)); ROLLBACK").unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM atomic", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

fn hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn binary(v: &serde_json::Value) -> Option<Vec<u8>> {
    if v.is_null() {
        None
    } else {
        assert_eq!(v["kind"], "binary");
        Some(hex(v["value"].as_str().unwrap()))
    }
}
fn object_end(s: &str, start: usize) -> usize {
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    for (i, b) in s.as_bytes()[start..].iter().copied().enumerate() {
        if quoted {
            if escaped {
                escaped = false
            } else if b == b'\\' {
                escaped = true
            } else if b == b'"' {
                quoted = false
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return start + i + 1;
                    }
                }
                _ => {}
            }
        }
    }
    panic!("unterminated retained observation")
}
fn domain(case: &serde_json::Value) -> Option<(Encoding, Target)> {
    if case["declared"] != case["wire"]
        || case["sourceFamily"] != "varchar"
        || !matches!(case["targetFamily"].as_str(), Some("varchar" | "nvarchar"))
    {
        return None;
    }
    let source = match case["declared"].as_str()? {
        "cp1251" => Encoding::Cp1251,
        "cp1252" => Encoding::Cp1252,
        "utf8" => Encoding::Utf8,
        _ => return None,
    };
    // Complete-projection fixtures exclude known width truncation and mismatched
    // declaration admission by original probe identity, never candidate equality.
    if source == Encoding::Cp1251
        && !matches!(
            case["name"].as_str()?,
            "cp1251-every-byte-cp1251"
                | "cp1251-every-byte-cp1252"
                | "cp1251-every-byte-unicode"
                | "cp1251-to-utf8-copyright-width2"
                | "cp1251-to-utf8-max-fragmented"
        )
    {
        return None;
    }
    let target = match case["target"].as_str()? {
        "cp1251" => Target::Native(Encoding::Cp1251),
        "cp1252" => Target::Native(Encoding::Cp1252),
        "utf8" => Target::Native(Encoding::Utf8),
        "unicode" => Target::SqlUtf16,
        _ => return None,
    };
    if source == Encoding::Utf8
        && !matches!(target, Target::Native(Encoding::Utf8) | Target::SqlUtf16)
    {
        return None;
    }
    Some((source, target))
}

#[test]
fn all_original_applicable_rows_project_through_native_vectors_and_materialized_arrow() {
    use std::collections::BTreeMap;
    type Cells = Vec<(Option<Vec<u8>>, Option<Vec<u8>>)>;
    let mut groups: BTreeMap<(u8, u8), Cells> = BTreeMap::new();
    let (mut observations, mut failed, mut malformed, mut comparisons) = (0, 0, 0, 0);
    for (reference, sha) in [
        (
            include_str!("../reference/bulk-character-conversion.json"),
            "f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27",
        ),
        (
            include_str!("../reference/bulk-character-cp1252.json"),
            "d9d8baa3ce4530f1af077feda8c80c9394b110557748b1fb84df0db9201189c2",
        ),
        (
            include_str!("../reference/bulk-character-utf8-boundary.json"),
            "fa1e3ae36794cfc4b197d2ff122c5ab5effb99f3b449d0ad2eafea354c18be6a",
        ),
        (
            include_str!("../reference/bulk-character-utf8-bounded-target.json"),
            "6c951a06d19c60c2a71e4656b570976baf726982cffcdef207d8e96bb5b92455",
        ),
    ] {
        assert_eq!(
            ring::digest::digest(&ring::digest::SHA256, reference.as_bytes()).as_ref(),
            hex(sha)
        );
        let mut names = BTreeMap::new();
        let mut position = 0;
        while let Some(offset) = reference[position..].find("{\"case\":") {
            let start = position + offset;
            let end = object_end(reference, start);
            let cs = start + "{\"case\":".len();
            let ce = object_end(reference, cs);
            let case: serde_json::Value = serde_json::from_str(&reference[cs..ce]).unwrap();
            position = end;
            let Some((source, target)) = domain(&case) else {
                continue;
            };
            *names
                .entry(case["name"].as_str().unwrap().to_owned())
                .or_insert(0) += 1;
            observations += 1;
            let o: serde_json::Value = serde_json::from_str(&reference[start..end]).unwrap();
            let out = &o["readback"]["result"]["sets"][1];
            if !o["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .is_empty()
            {
                failed += 1;
                assert!(out["rows"].as_array().unwrap().is_empty());
                continue;
            }
            assert_eq!(out["columns"][2]["type"], "VarBinary");
            assert_eq!(out["columns"][4]["type"], "VarBinary");
            assert_eq!(
                out["columns"][1]["type"],
                if target == Target::SqlUtf16 {
                    "NVarChar"
                } else {
                    "VarChar"
                }
            );
            for input in o["input"].as_array().unwrap() {
                let bytes = input["valueHex"].as_str().map(hex);
                if source == Encoding::Utf8
                    && bytes
                        .as_ref()
                        .is_some_and(|b| std::str::from_utf8(b).is_err())
                {
                    malformed += 1;
                    continue;
                }
                let row = out["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row[0] == input["id"])
                    .unwrap();
                let targets = if matches!(target, Target::Native(e) if e == source || e == Encoding::Utf8)
                {
                    vec![(target, 2), (Target::SqlUtf16, 4)]
                } else {
                    vec![(
                        target,
                        if matches!(target, Target::Native(_)) {
                            2
                        } else {
                            4
                        },
                    )]
                };
                for (target, field) in targets {
                    let key = (
                        match source {
                            Encoding::Cp1251 => 1,
                            Encoding::Cp1252 => 2,
                            Encoding::Utf8 => 3,
                            _ => unreachable!(),
                        },
                        match target {
                            Target::Native(Encoding::Cp1251) => 1,
                            Target::Native(Encoding::Cp1252) => 2,
                            Target::Native(Encoding::Utf8) => 3,
                            Target::SqlUtf16 => 4,
                            _ => unreachable!(),
                        },
                    );
                    groups
                        .entry(key)
                        .or_default()
                        .push((bytes.clone(), binary(&row[field])));
                    comparisons += 1
                }
            }
        }
        for count in names.values() {
            assert_eq!(*count, 4)
        }
    }
    assert_eq!((observations, failed, malformed), (784, 236, 184));
    assert_eq!(comparisons, 11996);
    for ((source, target), cells) in groups {
        let source = match source {
            1 => Encoding::Cp1251,
            2 => Encoding::Cp1252,
            3 => Encoding::Utf8,
            _ => unreachable!(),
        };
        let target = match target {
            1 => Target::Native(Encoding::Cp1251),
            2 => Target::Native(Encoding::Cp1252),
            3 => Target::Native(Encoding::Utf8),
            4 => Target::SqlUtf16,
            _ => unreachable!(),
        };
        let db = Connection::open_in_memory().unwrap();
        storage(source).register(&db).unwrap();
        plan(source, target)
            .register(&db, "__msduck_ansi_project_replay")
            .unwrap();
        db.execute_batch("CREATE TABLE original_input(id BIGINT,b BLOB)")
            .unwrap();
        {
            let mut app = db.appender("original_input").unwrap();
            for (i, (input, _)) in cells.iter().enumerate() {
                app.append_row(duckdb::params![
                    i as i64,
                    input.clone().map(Value::Blob).unwrap_or(Value::Null)
                ])
                .unwrap();
            }
            app.flush().unwrap();
        }
        db.execute_batch(&format!("CREATE TABLE projected AS SELECT id,__msduck_ansi_project_replay({}(b)) v FROM original_input",storage(source).pack_function())).unwrap();
        let mut query = db.prepare("SELECT v FROM projected ORDER BY id").unwrap();
        let batches: Vec<_> = query.query_arrow([]).unwrap().collect();
        let mut index = 0;
        for b in batches {
            let array = b.column(0);
            for row in 0..b.num_rows() {
                let value = match target {
                    Target::Native(e) => storage(e)
                        .read_array(array.as_ref(), row)
                        .unwrap()
                        .map(|v| v.into_parts().1),
                    Target::SqlUtf16 => unicode_arrow(array.as_ref(), row),
                };
                assert_eq!(
                    value, cells[index].1,
                    "source {source:?}, target {target:?}, index {index}"
                );
                index += 1
            }
        }
        assert_eq!(index, cells.len());
    }
}

use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
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
fn projection_evaluates_volatile_original_once_across_chunks() {
    let db = Connection::open_in_memory().unwrap();
    let storage = storage(Encoding::Cp1251);
    storage.register(&db).unwrap();
    let p = plan(Encoding::Cp1251, Target::Native(Encoding::Cp1251));
    p.register(&db, "__msduck_ansi_project_once").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    db.register_scalar_function_with_state::<VolatileBytes>("volatile_native_bytes", &count)
        .unwrap();
    db.execute_batch("CREATE TABLE once AS SELECT i,__msduck_ansi_project_once(__msduck_ansi_pack_1251(volatile_native_bytes(i))) v FROM range(10000) d(i)").unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 10000);
    let mut q = db
        .prepare("SELECT i,__msduck_ansi_unpack_1251(v) FROM once ORDER BY i")
        .unwrap();
    for (expected, row) in q
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .unwrap()
        .enumerate()
    {
        let (id, bytes) = row.unwrap();
        assert_eq!(id, expected as i64);
        assert_eq!(bytes, id.to_le_bytes())
    }
    assert_eq!(count.load(Ordering::SeqCst), 10000);
}

fn unicode_arrow(array: &dyn duckdb::arrow::array::Array, row: usize) -> Option<Vec<u8>> {
    use duckdb::arrow::{
        array::{BinaryArray, LargeBinaryArray, StructArray},
        datatypes::DataType,
    };
    let DataType::Struct(fields) = array.data_type() else {
        panic!("not UTF16 struct")
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name(), "__msduck_utf16le");
    assert!(matches!(
        fields[0].data_type(),
        DataType::Binary | DataType::LargeBinary
    ));
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
fn earlier_variable_encoding_controls_keep_actual_primary_native_or_unicode_bytes() {
    let reference = include_str!("../reference/bulk-character-encoding.json");
    assert_eq!(
        ring::digest::digest(&ring::digest::SHA256, reference.as_bytes()).as_ref(),
        hex("0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83")
    );
    let (mut position, mut rows, mut malformed, mut failed, mut observations) = (0, 0, 0, 0, 0);
    while let Some(offset) = reference[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(reference, start);
        let cs = start + "{\"case\":".len();
        let ce = object_end(reference, cs);
        let case: serde_json::Value = serde_json::from_str(&reference[cs..ce]).unwrap();
        position = end;
        let source = match case["source"].as_str() {
            Some("SQL_Latin1_General_CP1_CI_AS") => Encoding::Cp1252,
            Some("Cyrillic_General_100_BIN2") => Encoding::Cp1251,
            Some("Latin1_General_100_BIN2_UTF8") => Encoding::Utf8,
            _ => continue,
        };
        let target = match case["target"].as_str() {
            Some("same") => Target::Native(source),
            Some("cp1252") if source != Encoding::Utf8 => Target::Native(Encoding::Cp1252),
            Some("unicode") => Target::SqlUtf16,
            _ => continue,
        };
        observations += 1;
        let o: serde_json::Value = serde_json::from_str(&reference[start..end]).unwrap();
        let out = &o["readback"]["result"]["sets"][1];
        if !o["execution"]["result"]["errors"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            failed += 1;
            assert!(out["rows"].as_array().unwrap().is_empty());
            continue;
        }
        assert_eq!(out["columns"][2]["type"], "VarBinary");
        assert_eq!(
            out["columns"][1]["type"],
            if target == Target::SqlUtf16 {
                "NVarChar"
            } else {
                "VarChar"
            }
        );
        let db = Connection::open_in_memory().unwrap();
        storage(source).register(&db).unwrap();
        plan(source, target)
            .register(&db, "__msduck_ansi_project_check")
            .unwrap();
        for input in o["input"].as_array().unwrap() {
            let bytes = input["valueHex"].as_str().map(hex);
            if source == Encoding::Utf8
                && bytes
                    .as_ref()
                    .is_some_and(|b| std::str::from_utf8(b).is_err())
            {
                malformed += 1;
                continue;
            }
            let row = out["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r[0] == input["id"])
                .unwrap();
            let expected = binary(&row[2]);
            check(&db, source, target, bytes.as_deref(), expected.as_deref());
            rows += 1
        }
    }
    assert_eq!((observations, rows, malformed, failed), (104, 400, 16, 24));
    println!(
        "original884 observations={observations} rows={rows} malformed={malformed} failed={failed}"
    );
}
