use duckdb::{Connection, types::Value};
use msduck::ansi_carrier::{
    Plan as Storage,
    projection::{CapacityDeclaration, Plan},
};
use msduck_core::{
    ansi_bytes::{AnsiView, EncodingIdentity as Encoding},
    ansi_conversion::{
        ProjectionLimits, ProjectionTarget as Target,
        capacity::{Capacity, Family, SourceForm},
    },
};
use serde_json::Value as Json;
use std::path::Path;

const CELL: usize = 65536;
const CHUNK: usize = 8 * CELL;
fn storage(encoding: Encoding) -> Storage {
    Storage::new(encoding, CELL, CHUNK).unwrap()
}
fn setup(db: &Connection) {
    for e in [Encoding::Cp1251, Encoding::Cp1252, Encoding::Utf8] {
        storage(e).register(db).unwrap();
    }
}
fn hex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0);
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn profile(s: &str) -> Encoding {
    match s {
        "cp1251" => Encoding::Cp1251,
        "cp1252" => Encoding::Cp1252,
        "utf8" => Encoding::Utf8,
        _ => panic!("unknown captured profile"),
    }
}
fn target(s: &str) -> Target {
    if s == "unicode" {
        Target::SqlUtf16
    } else {
        Target::Native(profile(s))
    }
}
fn declaration(source: Encoding, target: Target, width: usize) -> CapacityDeclaration {
    CapacityDeclaration {
        source,
        source_form: SourceForm::Bounded,
        target,
        family: Family::Variable,
        capacity: Capacity::Bounded(width),
    }
}
fn plan(d: CapacityDeclaration) -> Plan {
    Plan::bulk_capacity(
        d,
        ProjectionLimits {
            input_bytes: CELL,
            output_bytes: CELL,
        },
        CHUNK,
        CHUNK,
    )
    .unwrap()
}
fn bind(e: Encoding, b: Option<&[u8]>) -> Value {
    storage(e)
        .bind(AnsiView::nullable(e, b, CELL).unwrap())
        .unwrap()
}
fn unicode(v: &Value) -> Option<Vec<u8>> {
    if matches!(v, Value::Null) {
        return None;
    }
    let Value::Struct(fields) = v else {
        panic!("not a UTF16 carrier")
    };
    assert_eq!(fields.keys().count(), 1);
    let Value::Blob(b) = fields.get(&"__msduck_utf16le".to_owned()).unwrap() else {
        panic!("not a UTF16 payload")
    };
    assert_eq!(b.len() % 2, 0);
    Some(b.clone())
}
fn actual(v: &Value, target: Target) -> Option<Vec<u8>> {
    match target {
        Target::Native(e) => storage(e).read_value(v).unwrap().map(|v| v.into_parts().1),
        Target::SqlUtf16 => unicode(v),
    }
}
fn binary(v: &Json) -> Option<Vec<u8>> {
    if v.is_null() {
        None
    } else {
        assert_eq!(v["kind"], "binary");
        Some(hex(v["value"].as_str().unwrap()))
    }
}
fn pinned(relative: &str, digest: &str) -> Vec<u8> {
    let bytes = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap();
    assert_eq!(
        ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref(),
        hex(digest)
    );
    bytes
}
fn object_end(text: &str, start: usize) -> usize {
    assert_eq!(text.as_bytes()[start], b'{');
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    for (offset, &byte) in text.as_bytes()[start..].iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
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
    panic!("unterminated original observation");
}
fn admitted_observations(text: &str) -> Vec<Json> {
    // Unrelated Unicode source observations retain isolated surrogate escapes
    // that serde_json's Rust strings cannot represent. Select the supported
    // declarations before parsing, as the original core replay does; never
    // repair, replace or normalize the SHA-pinned raw fixture.
    let mut observations = Vec::new();
    let mut position = 0;
    while let Some(offset) = text[position..].find("{\"case\":") {
        let start = position + offset;
        let end = object_end(text, start);
        let cs = start + "{\"case\":".len();
        let ce = object_end(text, cs);
        let c: Json = serde_json::from_str(&text[cs..ce]).unwrap();
        if matches!(c["declared"].as_str(), Some("cp1251" | "cp1252"))
            && matches!(c["target"].as_str(), Some("cp1252" | "utf8" | "unicode"))
            && c["declared"] == c["wire"]
            && (c.get("wireWidth").is_none() || c["sourceWidth"] == c["wireWidth"])
        {
            let o: Json = serde_json::from_str(&text[start..end]).unwrap();
            if o["execution"]["result"]["errors"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["number"] == 2628)
            {
                observations.push(o);
            }
        }
        position = end;
    }
    observations
}
#[test]
fn native_callback_replays_every_original_admitted_capacity_observation() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let (mut observations, mut rows, mut failed) = (0, 0, 0);
    for (file, sha) in [
        (
            "bulk-character-conversion",
            "f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27",
        ),
        (
            "bulk-character-capacity",
            "cd82a8853bcac9f3fee98c1fb6c6f17564443918868ece1f1d9f66b3abb0ec92",
        ),
        (
            "bulk-character-trailing-space",
            "d9743a80462871dbed0f2830e984c71e621afb2d704d6a541271ae0f3c305740",
        ),
    ] {
        let fixture = String::from_utf8(pinned(&format!("reference/{file}.json"), sha)).unwrap();
        let mut names = std::collections::BTreeMap::new();
        for o in admitted_observations(&fixture) {
            let c = &o["case"];
            let errors = o["execution"]["result"]["errors"].as_array().unwrap();
            if !matches!(c["declared"].as_str(), Some("cp1251" | "cp1252"))
                || !matches!(c["target"].as_str(), Some("cp1252" | "utf8" | "unicode"))
                || c["declared"] != c["wire"]
                || (c.get("wireWidth").is_some() && c["sourceWidth"] != c["wireWidth"])
                || !errors.iter().all(|e| e["number"] == 2628)
            {
                continue;
            }
            observations += 1;
            *names
                .entry(c["name"].as_str().unwrap().to_owned())
                .or_insert(0) += 1;
            let d = CapacityDeclaration {
                source: profile(c["declared"].as_str().unwrap()),
                source_form: if c["sourceWidth"] == "max" {
                    SourceForm::Max
                } else {
                    SourceForm::Bounded
                },
                target: target(c["target"].as_str().unwrap()),
                family: if matches!(c["targetFamily"].as_str(), Some("char" | "nchar")) {
                    Family::Fixed
                } else {
                    Family::Variable
                },
                capacity: if c["targetWidth"] == "max" {
                    Capacity::Max
                } else {
                    Capacity::Bounded(c["targetWidth"].as_u64().unwrap() as usize)
                },
            };
            let name = format!("__msduck_ansi_project_capacity_{observations}");
            plan(d).register(&db, &name).unwrap();
            let sql = format!("SELECT {name}({}(?))", storage(d.source).pack_function());
            let mut query = db.prepare(&sql).unwrap();
            let captured = o["readback"]["result"]["sets"][1]["rows"]
                .as_array()
                .unwrap();
            let mut rejected = false;
            for row in o["input"].as_array().unwrap() {
                rows += 1;
                let bytes = row["valueHex"].as_str().map(hex);
                let original = bytes.clone();
                let result =
                    query.query_row([bind(d.source, bytes.as_deref())], |r| r.get::<_, Value>(0));
                if errors.is_empty() {
                    let expected = captured.iter().find(|r| r[0] == row["id"]).unwrap();
                    assert_eq!(
                        actual(&result.unwrap(), d.target),
                        binary(&expected[2]),
                        "{} row{}",
                        c["name"],
                        row["id"]
                    );
                } else if let Err(e) = result {
                    assert!(e.to_string().contains("exceeds SQL capacity"), "{e}");
                    rejected = true;
                }
                assert_eq!(bytes, original);
            }
            if !errors.is_empty() {
                failed += 1;
                assert!(rejected);
                assert!(captured.is_empty());
            }
        }
        assert!(names.values().all(|&n| n == 4));
    }
    assert_eq!((observations, rows, failed), (476, 3248, 64));
}
fn unpack(v: &Json) -> Option<Vec<u8>> {
    if v.is_null() {
        return None;
    }
    let mut bytes = Vec::new();
    for pair in v.as_array().unwrap() {
        let byte = u8::try_from(pair[0].as_u64().unwrap()).unwrap();
        let count = usize::try_from(pair[1].as_u64().unwrap()).unwrap();
        assert!(count > 0 && count <= 16385);
        bytes.extend(std::iter::repeat_n(byte, count));
    }
    Some(bytes)
}
#[test]
fn native_callback_replays_all_four_retained_cp1251_capacity_oracles() {
    // These are the original, losslessly packed native/error oracles retained
    // in the completed core suite, not expectations generated by its kernel.
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/msduck-core/tests/ansi_capacity_cp1251.rs"),
    )
    .unwrap();
    let literal = source
        .split_once("const OBSERVED: &str = r#\"")
        .unwrap()
        .1
        .split_once("\"#;")
        .unwrap()
        .0;
    assert_eq!(
        ring::digest::digest(&ring::digest::SHA256, literal.as_bytes()).as_ref(),
        hex("3c03d3bc52f7aba997518a87cad832d11f296e71ce3cc13ca972665a4a49367b")
    );
    let probes: Json = serde_json::from_str(literal).unwrap();
    assert_eq!(probes.as_array().unwrap().len(), 286);
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let (mut observations, mut rows, mut failures) = (0, 0, 0);
    for (index, p) in probes.as_array().unwrap().iter().enumerate() {
        let d = CapacityDeclaration {
            source: profile(p["source"].as_str().unwrap()),
            source_form: if p["form"] == "max" {
                SourceForm::Max
            } else {
                SourceForm::Bounded
            },
            target: Target::Native(Encoding::Cp1251),
            family: if p["fixed"].as_bool().unwrap() {
                Family::Fixed
            } else {
                Family::Variable
            },
            capacity: if p["width"] == "max" {
                Capacity::Max
            } else {
                Capacity::Bounded(p["width"].as_u64().unwrap() as usize)
            },
        };
        let name = format!("__msduck_ansi_project_cp1251_{index}");
        plan(d).register(&db, &name).unwrap();
        let sql = format!("SELECT {name}({}(?))", storage(d.source).pack_function());
        let mut query = db.prepare(&sql).unwrap();
        assert_eq!(p["oracles"].as_array().unwrap().len(), 4);
        for oracle in p["oracles"].as_array().unwrap() {
            observations += 1;
            let failed = !oracle["errors"].as_array().unwrap().is_empty();
            let mut rejected = false;
            for (row, b) in p["inputs"].as_array().unwrap().iter().enumerate() {
                rows += 1;
                let bytes = unpack(b);
                let original = bytes.clone();
                let result =
                    query.query_row([bind(d.source, bytes.as_deref())], |r| r.get::<_, Value>(0));
                if failed {
                    if let Err(e) = result {
                        assert!(e.to_string().contains("exceeds SQL capacity"), "{e}");
                        rejected = true;
                    }
                } else {
                    assert_eq!(
                        actual(&result.unwrap(), d.target),
                        unpack(&oracle["native"][row]),
                        "{} run{observations} row{row}",
                        p["name"]
                    );
                }
                assert_eq!(bytes, original);
            }
            if failed {
                failures += 1;
                assert!(rejected);
                assert!(
                    oracle["errors"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|e| *e == 2628)
                );
                assert!(oracle["native"].as_array().unwrap().is_empty());
            }
        }
    }
    assert_eq!((observations, rows, failures), (1144, 2336, 920));
}

#[test]
fn declarations_and_resource_limits_remain_independent_of_null_and_fitting() {
    let base = declaration(Encoding::Cp1251, Target::SqlUtf16, 1);
    let limits = ProjectionLimits {
        input_bytes: 0,
        output_bytes: 0,
    };
    for d in [
        CapacityDeclaration {
            source: Encoding::Utf8,
            ..base
        },
        CapacityDeclaration {
            source: Encoding::Opaque(1251),
            ..base
        },
        CapacityDeclaration {
            target: Target::Native(Encoding::Opaque(1252)),
            ..base
        },
        CapacityDeclaration {
            capacity: Capacity::Bounded(0),
            ..base
        },
        CapacityDeclaration {
            capacity: Capacity::Bounded(4001),
            ..base
        },
        CapacityDeclaration {
            family: Family::Fixed,
            capacity: Capacity::Max,
            ..base
        },
    ] {
        assert!(Plan::bulk_capacity(d, limits, 0, 0).is_err());
    }
    assert!(
        Plan::bulk_capacity(
            base,
            ProjectionLimits {
                input_bytes: 1,
                output_bytes: 16 * 1024 * 1024 + 1
            },
            1,
            64 * 1024 * 1024
        )
        .is_err()
    );
    assert!(Plan::bulk_capacity(base, limits, 0, 64 * 1024 * 1024 + 1).is_err());
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    Plan::bulk_capacity(base, limits, 0, 0)
        .unwrap()
        .register(&db, "__msduck_ansi_project_zero")
        .unwrap();
    for b in [None, Some(&[][..])] {
        let v: Value = db
            .query_row(
                "SELECT __msduck_ansi_project_zero(__msduck_ansi_pack_1251(?))",
                [bind(Encoding::Cp1251, b)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unicode(&v).as_deref(), b);
    }
    let error = db
        .query_row(
            "SELECT __msduck_ansi_project_zero(__msduck_ansi_pack_1251(' '::BLOB))",
            [],
            |r| r.get::<_, Value>(0),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("native ANSI payload 1 exceeds limit 0"),
        "{error}"
    );
    let fixed = CapacityDeclaration {
        target: Target::Native(Encoding::Utf8),
        family: Family::Fixed,
        capacity: Capacity::Bounded(2),
        ..base
    };
    for (index, output) in [(0, 2), (1, 1)] {
        let name = format!("__msduck_ansi_project_exact_{index}");
        Plan::bulk_capacity(
            fixed,
            ProjectionLimits {
                input_bytes: 1,
                output_bytes: output,
            },
            1,
            2,
        )
        .unwrap()
        .register(&db, &name)
        .unwrap();
        let sql = format!("SELECT {name}(__msduck_ansi_pack_1251(?))");
        let result = db.query_row(&sql, [bind(Encoding::Cp1251, Some(&[0xcf]))], |r| {
            r.get::<_, Value>(0)
        });
        if index == 0 {
            assert_eq!(actual(&result.unwrap(), fixed.target), Some(hex("d09f")));
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("Output payload 2 exceeds limit 1")
            );
        }
    }
}

#[test]
fn capacity_failures_preserve_statement_atomicity_and_connection_recovery() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let d = declaration(Encoding::Cp1252, Target::Native(Encoding::Cp1252), 1);
    plan(d)
        .register(&db, "__msduck_ansi_project_atomic")
        .unwrap();
    db.execute_batch("CREATE TABLE projected(v STRUCT(__msduck_ansi_encoding UINTEGER,__msduck_ansi_bytes BLOB))").unwrap();
    let error = db.execute_batch("INSERT INTO projected SELECT __msduck_ansi_project_atomic(__msduck_ansi_pack_1252(CASE WHEN i=9999 THEN 'AB'::BLOB ELSE 'A'::BLOB END)) FROM range(10000) t(i)").unwrap_err().to_string();
    assert!(
        error.contains("ANSI source payload 2 exceeds SQL capacity 1"),
        "{error}"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM projected", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("BEGIN; INSERT INTO projected SELECT __msduck_ansi_project_atomic(__msduck_ansi_pack_1252('A'::BLOB)); ROLLBACK").unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM projected", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let v: Value = db
        .query_row(
            "SELECT __msduck_ansi_project_atomic(__msduck_ansi_pack_1252('A '::BLOB))",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(actual(&v, d.target), Some(b"A".to_vec()));
    for (index, chunk) in [(0, 4), (1, 3)] {
        let name = format!("__msduck_ansi_project_chunk_{index}");
        let d = CapacityDeclaration {
            family: Family::Fixed,
            capacity: Capacity::Bounded(2),
            ..d
        };
        Plan::bulk_capacity(
            d,
            ProjectionLimits {
                input_bytes: 1,
                output_bytes: 2,
            },
            2,
            chunk,
        )
        .unwrap()
        .register(&db, &name)
        .unwrap();
        let result = db.execute_batch(&format!("INSERT INTO projected SELECT {name}(__msduck_ansi_pack_1252(b)) FROM (VALUES ('A'::BLOB),('B'::BLOB)) t(b)"));
        if index == 0 {
            result.unwrap();
            db.execute_batch("DELETE FROM projected").unwrap();
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("native ANSI payload 4 exceeds limit 3")
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM projected", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
}

#[test]
fn selected_source_form_and_target_do_not_come_from_current_values() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let base = declaration(Encoding::Cp1251, Target::Native(Encoding::Cp1252), 1);
    for (index, source_form) in [SourceForm::Bounded, SourceForm::Max]
        .into_iter()
        .enumerate()
    {
        let d = CapacityDeclaration {
            source_form,
            ..base
        };
        let name = format!("__msduck_ansi_project_form_{index}");
        plan(d).register(&db, &name).unwrap();
        let sql = format!("SELECT {name}(__msduck_ansi_pack_1251(?))");
        let result = db.query_row(&sql, [bind(d.source, Some(b"A B"))], |r| {
            r.get::<_, Value>(0)
        });
        if source_form == SourceForm::Max {
            assert_eq!(actual(&result.unwrap(), d.target), Some(b"A".to_vec()));
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("exceeds SQL capacity")
            );
        }
        let wrong = format!(
            "SELECT {name}(struct_pack(__msduck_ansi_encoding := 1252::UINTEGER,__msduck_ansi_bytes := 'A'::BLOB))"
        );
        assert!(db.query_row(&wrong, [], |r| r.get::<_, Value>(0)).is_err());
        let child = format!(
            "SELECT {name}(struct_pack(__msduck_ansi_encoding := 1251::UINTEGER,__msduck_ansi_bytes := NULL::BLOB))"
        );
        assert!(db.query_row(&child, [], |r| r.get::<_, Value>(0)).is_err());
    }
    let d = CapacityDeclaration {
        target: Target::Native(Encoding::Cp1251),
        source_form: SourceForm::Max,
        ..base
    };
    plan(d).register(&db, "__msduck_ansi_project_same").unwrap();
    assert!(
        db.query_row(
            "SELECT __msduck_ansi_project_same(__msduck_ansi_pack_1251('A B'::BLOB))",
            [],
            |r| r.get::<_, Value>(0)
        )
        .unwrap_err()
        .to_string()
        .contains("exceeds SQL capacity")
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
            "ansi-capacity-{}.duckdb",
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
fn native_and_unicode_capacity_outputs_survive_restart_and_arrow() {
    let file = DatabaseFile::new();
    let inputs = [
        None,
        Some(vec![]),
        Some(vec![0xcf]),
        Some(b"A".to_vec()),
        Some(b" ".to_vec()),
    ];
    {
        let db = Connection::open(&file.0).unwrap();
        setup(&db);
        db.execute_batch("CREATE TABLE original(i INTEGER,v STRUCT(__msduck_ansi_encoding UINTEGER,__msduck_ansi_bytes BLOB))").unwrap();
        for (i, bytes) in inputs.iter().enumerate() {
            db.execute(
                "INSERT INTO original VALUES (?,__msduck_ansi_pack_1251(?))",
                duckdb::params![i as i32, bind(Encoding::Cp1251, bytes.as_deref())],
            )
            .unwrap();
        }
    }
    {
        let db = Connection::open(&file.0).unwrap();
        for (index, target) in [Target::Native(Encoding::Cp1251), Target::SqlUtf16]
            .into_iter()
            .enumerate()
        {
            let d = CapacityDeclaration {
                family: Family::Fixed,
                ..declaration(Encoding::Cp1251, target, 2)
            };
            let name = format!("__msduck_ansi_project_persist_{index}");
            plan(d).register(&db, &name).unwrap();
            db.execute_batch(&format!(
                "CREATE TABLE converted_{index} AS SELECT i,{name}(v) v FROM original"
            ))
            .unwrap();
        }
    }
    let db = Connection::open(&file.0).unwrap();
    let native = [
        None,
        Some(hex("2020")),
        Some(hex("cf20")),
        Some(hex("4120")),
        Some(hex("2020")),
    ];
    let units = [
        None,
        Some(hex("20002000")),
        Some(hex("1f042000")),
        Some(hex("41002000")),
        Some(hex("20002000")),
    ];
    for (table, encoding, expected) in [
        ("original", Encoding::Cp1251, &inputs),
        ("converted_0", Encoding::Cp1251, &native),
    ] {
        let mut q = db
            .prepare(&format!("SELECT v FROM {table} ORDER BY i"))
            .unwrap();
        let mut seen = 0;
        for batch in q.query_arrow([]).unwrap() {
            for row in 0..batch.num_rows() {
                assert_eq!(
                    storage(encoding)
                        .read_array(batch.column(0).as_ref(), row)
                        .unwrap()
                        .map(|v| v.into_parts().1),
                    expected[seen]
                );
                seen += 1;
            }
        }
        assert_eq!(seen, expected.len());
    }
    let mut q = db.prepare("SELECT v FROM converted_1 ORDER BY i").unwrap();
    let mut seen = 0;
    for batch in q.query_arrow([]).unwrap() {
        use duckdb::arrow::array::{BinaryArray, LargeBinaryArray, StructArray};
        let a = batch.column(0);
        let structure = a.as_any().downcast_ref::<StructArray>().unwrap();
        assert_eq!(structure.num_columns(), 1);
        assert_eq!(structure.fields()[0].name(), "__msduck_utf16le");
        for row in 0..batch.num_rows() {
            let bytes = if a.is_null(row) {
                None
            } else {
                let b = structure.column(0);
                assert!(!b.is_null(row));
                Some(if let Some(b) = b.as_any().downcast_ref::<BinaryArray>() {
                    b.value(row).to_vec()
                } else {
                    b.as_any()
                        .downcast_ref::<LargeBinaryArray>()
                        .unwrap()
                        .value(row)
                        .to_vec()
                })
            };
            assert_eq!(bytes, units[seen]);
            seen += 1;
        }
    }
    assert_eq!(seen, units.len());
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
        assert!(input.len() <= source.capacity() && input.len() <= result.capacity());
        for row in 0..input.len() {
            assert!(!source.try_row_is_null(row as u64)?);
            // SAFETY: BIGINT signature, checked capacity and initialized non-NULL row.
            let id = unsafe { source.as_slice_with_len::<i64>(input.len())[row] };
            count.fetch_add(1, Ordering::SeqCst);
            match id % 5 {
                0 => result.set_null(row),
                1 => result.insert(row, &[][..]),
                2 => result.insert(row, &[0xcf][..]),
                3 => result.insert(row, &[0x98][..]),
                _ => result.insert(row, b" ".as_slice()),
            }
        }
        Ok(())
    }
}
#[test]
fn capacity_evaluates_original_once_and_preserves_nulls_across_chunks() {
    let db = Connection::open_in_memory().unwrap();
    setup(&db);
    let d = CapacityDeclaration {
        family: Family::Fixed,
        ..declaration(Encoding::Cp1251, Target::Native(Encoding::Cp1251), 2)
    };
    plan(d).register(&db, "__msduck_ansi_project_once").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    db.register_scalar_function_with_state::<VolatileBytes>("volatile_capacity_bytes", &count)
        .unwrap();
    db.execute_batch("CREATE TABLE once AS SELECT i,__msduck_ansi_project_once(__msduck_ansi_pack_1251(volatile_capacity_bytes(i))) v FROM range(10000) t(i)").unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 10000);
    let expected = [
        None,
        Some(hex("2020")),
        Some(hex("cf20")),
        Some(hex("9820")),
        Some(hex("2020")),
    ];
    let mut seen = 0;
    let mut q = db.prepare("SELECT v FROM once ORDER BY i").unwrap();
    for batch in q.query_arrow([]).unwrap() {
        for row in 0..batch.num_rows() {
            assert_eq!(
                storage(Encoding::Cp1251)
                    .read_array(batch.column(0).as_ref(), row)
                    .unwrap()
                    .map(|v| v.into_parts().1),
                expected[seen % 5]
            );
            seen += 1;
        }
    }
    assert_eq!(seen, 10000);
    assert_eq!(count.load(Ordering::SeqCst), 10000);
}
