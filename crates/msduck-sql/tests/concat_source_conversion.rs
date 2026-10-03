use binding::{Declaration, Stored};
use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::Label,
    types::{BinaryType, DecimalType, Scale, Type},
    value::Decimal,
};
use msduck_sql::{
    concat_conversion as binding, concat_ws as rules, numeric_text, temporal_guid_text as temporal,
};
use serde_json::{Value, json};
const DEFAULT: &str = "SQL_Latin1_General_CP1_CI_AS";
fn catalog() -> Vec<rules::Collation> {
    vec![rules::Collation {
        name: DEFAULT.into(),
        supplementary: false,
        case_sensitive: false,
        encoding: rules::Encoding::Cp1252,
    }]
}
fn declaration(source: Type) -> Declaration {
    Declaration {
        source: Some(source),
        collation: matches!(source, Type::Character(_) | Type::Text | Type::Ntext)
            .then(|| Label::CoercibleDefault(DEFAULT.into())),
        style: None,
    }
}
fn character(unicode: bool, width: u16) -> Declaration {
    declaration(Type::Character(
        CharacterType::new(
            if unicode {
                Family::Nvarchar
            } else {
                Family::Varchar
            },
            Length::Bounded(width),
        )
        .unwrap(),
    ))
}
fn assert_descriptor(plan: &binding::Plan, col: &Value, name: &str) {
    let p = plan.result();
    let unicode = matches!(p.declaration.family(), Family::Nvarchar);
    assert_eq!(
        col["type"],
        if unicode { "NVarChar" } else { "VarChar" },
        "{name}"
    );
    assert_eq!(
        col["length"],
        json!(match p.declaration.length() {
            Length::Max => 65535,
            Length::Bounded(n) => u64::from(n) * if unicode { 2 } else { 1 },
        }),
        "{name}"
    );
    assert_eq!(col["flags"], json!(p.flags), "{name}");
    assert_eq!(p.collation.name(), Some(DEFAULT));
    assert_eq!(col["userType"], 0, "{name}");
    assert!(
        col["precision"].is_null() && col["scale"].is_null(),
        "{name}"
    );
    for field in ["schema", "udtInfo", "tableName"] {
        assert_eq!(col[field], json!({"kind":"missing"}), "{name} {field}");
    }
    assert_eq!(col["collation"]["lcid"], 1033, "{name}");
    assert_eq!(col["collation"]["flags"], 13, "{name}");
    assert_eq!(col["collation"]["version"], 0, "{name}");
    assert_eq!(col["collation"]["sortId"], 52, "{name}");
    assert_eq!(col["collation"]["codepage"], "CP1252", "{name}");
}
fn compose(
    source: Type,
    value: Option<Stored<'_>>,
    role: &str,
    language: temporal::Language,
    col: &Value,
    expected: &Value,
    name: &str,
) {
    let unicode = role.contains("unicode");
    let operation = if role.starts_with("tr") {
        rules::Function::Translate
    } else {
        rules::Function::ConcatWs
    };
    let mut decls = vec![character(unicode, 1); 3];
    let (empty, a, b) = (vec![], vec![97], vec![98]);
    let mut values = vec![Some(Stored::Character(empty.as_slice())); 3];
    let position = if operation == rules::Function::Translate {
        0
    } else if role.contains("separator") && role != "cws null separator" {
        values[1] = Some(Stored::Character(&a));
        values[2] = Some(Stored::Character(&b));
        0
    } else if role.contains("last") {
        2
    } else {
        1
    };
    decls[position] = declaration(source);
    values[position] = value;
    if role == "cws null separator" || role.contains("isolated") {
        decls[0] = Declaration::null_literal();
        values[0] = None;
        let other = if position == 2 { 1 } else { 2 };
        decls[other] = Declaration::null_literal();
        values[other] = None;
    }
    let plan = binding::plan(operation, &decls, DEFAULT, &catalog(), language, None).unwrap();
    assert_descriptor(&plan, col, name);
    let output = binding::evaluate_with_keys(&plan, &values, &|u| Some(u.to_vec())).unwrap();
    assert_eq!(
        output,
        expected
            .as_str()
            .map(|s| s.encode_utf16().collect::<Vec<_>>()),
        "{name}"
    );
    // No callback is invoked for empty mappings or CONCAT_WS; both paths agree.
    assert_eq!(
        binding::evaluate(&plan, &values, &|_, _| None).unwrap(),
        output,
        "{name}"
    );
}
mod numeric_helpers {
    use super::*;
    use numeric_text::Value as Numeric;
    pub(super) fn kind(name: &str) -> Type {
        match name {
            "BIT" => Type::Bit,
            "TINYINT" => Type::TinyInt,
            "SMALLINT" => Type::SmallInt,
            "INT" => Type::Int,
            "BIGINT" | "BigInt" => Type::BigInt,
            "MONEY" | "Money" => Type::Money,
            "SMALLMONEY" => Type::SmallMoney,
            "REAL" => Type::Real,
            "FLOAT" => Type::Float,
            s if s.starts_with("DECIMAL(") => {
                let (p, s) = s[8..s.len() - 1].split_once(',').unwrap();
                Type::Decimal(DecimalType::new(p.parse().unwrap(), s.parse().unwrap()).unwrap())
            }
            _ => panic!("unknown declaration {name}"),
        }
    }
    pub(super) fn scalar(kind: Type, source: &str) -> Numeric {
        match kind {
            Type::Decimal(d) => Numeric::Decimal(
                Decimal::new(d.precision(), d.scale(), {
                    let fraction = source.split_once('.').map_or(0, |(_, f)| f.len());
                    assert!(fraction <= usize::from(d.scale()));
                    source.replace('.', "").parse::<i128>().unwrap()
                        * 10i128.pow(u32::from(d.scale()) - fraction as u32)
                })
                .unwrap(),
            ),
            Type::Money | Type::SmallMoney => {
                Numeric::Money(msduck_core::money::parse_text(source).unwrap())
            }
            _ => Numeric::Integer(source.parse().unwrap()),
        }
    }
    pub(super) fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }
    pub(super) fn wire(kind: Type, hex: &str) -> Option<Numeric> {
        let b = bytes(hex);
        if b.is_empty() {
            return None;
        }
        Some(match kind {
            Type::Real => Numeric::RealBits(u32::from_le_bytes(b.try_into().unwrap())),
            Type::Float => Numeric::FloatBits(u64::from_le_bytes(b.try_into().unwrap())),
            Type::BigInt => Numeric::Integer(i64::from_le_bytes(b.try_into().unwrap())),
            Type::Money => {
                let high = i32::from_le_bytes(b[..4].try_into().unwrap());
                let low = u32::from_le_bytes(b[4..].try_into().unwrap());
                Numeric::Money((i64::from(high) << 32) | i64::from(low))
            }
            Type::Decimal(d) => {
                let magnitude = b[1..]
                    .iter()
                    .rev()
                    .fold(0i128, |n, b| n * 256 + i128::from(*b));
                Numeric::Decimal(
                    Decimal::new(
                        d.precision(),
                        d.scale(),
                        if b[0] == 0 { -magnitude } else { magnitude },
                    )
                    .unwrap(),
                )
            }
            _ => panic!("uncaptured wire source"),
        })
    }
}
mod temporal_helpers {
    use super::*;
    use msduck_core::{datetime2::DateTime2, datetimeoffset::DateTimeOffset};
    use temporal::{Language, Stored};
    pub(super) fn source(text: &str) -> Type {
        let scale = || {
            Scale::new(
                text.split_once('(')
                    .unwrap()
                    .1
                    .trim_end_matches(')')
                    .parse()
                    .unwrap(),
            )
            .unwrap()
        };
        match text {
            "DATE" => Type::Date,
            "DATETIME" => Type::DateTime,
            "SMALLDATETIME" => Type::SmallDateTime,
            "UNIQUEIDENTIFIER" => Type::UniqueIdentifier,
            _ if text.starts_with("TIME(") => Type::Time(scale()),
            _ if text.starts_with("DATETIME2(") => Type::DateTime2(scale()),
            _ if text.starts_with("DATETIMEOFFSET(") => Type::DateTimeOffset(scale()),
            _ => panic!("uncaptured declaration {text}"),
        }
    }
    pub(super) fn language(text: &str) -> Language {
        match text {
            "us_english" => Language::UsEnglish,
            "French" => Language::French,
            "German" => Language::German,
            _ => panic!("unknown language"),
        }
    }
    pub(super) fn bytes(text: &str) -> Vec<u8> {
        assert_eq!(text.len() % 2, 0);
        text.as_bytes()
            .chunks_exact(2)
            .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
            .collect()
    }
    pub(super) fn unsigned_le(bytes: &[u8]) -> u64 {
        assert!(bytes.len() <= 8);
        let mut full = [0; 8];
        full[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(full)
    }
    pub(super) fn decode(kind: Type, bytes: &[u8], sql_storage: bool) -> Stored {
        match kind {
            Type::Date => {
                assert_eq!(bytes.len(), 3);
                Stored::Date {
                    days: unsigned_le(bytes) as u32,
                }
            }
            Type::Time(scale) => {
                let payload = if sql_storage {
                    assert_eq!(bytes[0], scale.get());
                    &bytes[1..]
                } else {
                    bytes
                };
                assert_eq!(payload.len(), usize::from(scale.time_bytes()));
                Stored::Time {
                    ticks: unsigned_le(payload) * 10u64.pow(u32::from(7 - scale.get())),
                }
            }
            Type::DateTime2(scale) => {
                let payload = if sql_storage {
                    assert_eq!(bytes[0], scale.get());
                    &bytes[1..]
                } else {
                    bytes
                };
                Stored::DateTime2(DateTime2::decode(payload, scale.get()).unwrap())
            }
            Type::DateTimeOffset(scale) => {
                let payload = if sql_storage {
                    assert_eq!(bytes[0], scale.get());
                    &bytes[1..]
                } else {
                    bytes
                };
                Stored::DateTimeOffset(DateTimeOffset::decode(payload, scale.get()).unwrap())
            }
            Type::DateTime => {
                assert_eq!(bytes.len(), 8);
                let days = bytes[..4].try_into().unwrap();
                let ticks = bytes[4..].try_into().unwrap();
                Stored::DateTime {
                    days: if sql_storage {
                        i32::from_be_bytes(days)
                    } else {
                        i32::from_le_bytes(days)
                    },
                    ticks_300: if sql_storage {
                        u32::from_be_bytes(ticks)
                    } else {
                        u32::from_le_bytes(ticks)
                    },
                }
            }
            Type::SmallDateTime => {
                assert_eq!(bytes.len(), 4);
                let days = bytes[..2].try_into().unwrap();
                let minutes = bytes[2..].try_into().unwrap();
                Stored::SmallDateTime {
                    days: u32::from(if sql_storage {
                        u16::from_be_bytes(days)
                    } else {
                        u16::from_le_bytes(days)
                    }),
                    minutes: if sql_storage {
                        u16::from_be_bytes(minutes)
                    } else {
                        u16::from_le_bytes(minutes)
                    },
                }
            }
            Type::UniqueIdentifier => Stored::Guid(bytes.try_into().unwrap()),
            _ => panic!("unsupported decoder declaration"),
        }
    }
    pub(super) fn native_value(kind: Type, result: &Value) -> Option<Stored> {
        let raw = &result["sets"][0]["rows"][0][1];
        if raw.is_null() {
            None
        } else {
            assert_eq!(raw["kind"], "binary");
            Some(decode(kind, &bytes(raw["value"].as_str().unwrap()), true))
        }
    }
}

#[test]
fn numeric_sources_and_ieee_grid_compose_all_function_roles_and_prepared_bindings() {
    let mut totals = vec![];
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/concat-numeric-format.json"
    ))
    .unwrap();
    for c in fixture["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let (mut checks, mut rejected, mut controls) = (0, 0, 0);
            for record in run.as_array().unwrap() {
                let input = &record["input"];
                let Some(mode) = input["kind"].as_str() else {
                    continue;
                };
                let name = record["name"].as_str().unwrap();
                if mode == "rejected typed scalar" {
                    rejected += 1;
                    assert!(!record["result"]["errors"].as_array().unwrap().is_empty());
                    let native = run
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|r| {
                            r["input"]["kind"] == "rejected typed scalar"
                                && r["input"]["role"] == "native"
                                && r["input"]["declaration"] == input["declaration"]
                                && r["input"]["scalarText"] == input["scalarText"]
                        })
                        .unwrap();
                    assert_eq!(record["result"]["errors"], native["result"]["errors"]);
                    assert!(
                        record["result"]["sets"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|s| s["rows"].as_array().unwrap().is_empty())
                    );
                    continue;
                }
                if mode == "prepared scalar RPC" {
                    let d = &record["declarations"][0];
                    let source = if d["type"] == "Decimal" {
                        Type::Decimal(
                            DecimalType::new(
                                d["options"]["precision"].as_u64().unwrap() as u8,
                                d["options"]["scale"].as_u64().unwrap() as u8,
                            )
                            .unwrap(),
                        )
                    } else {
                        numeric_helpers::kind(d["type"].as_str().unwrap())
                    };
                    for (i, e) in record["prepared"]["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        assert_eq!(
                            e["result"]["sets"][0]["columns"],
                            record["prepared"]["prepare"]["sets"][0]["columns"]
                        );
                        let value = numeric_helpers::wire(
                            source,
                            record["bindingWire"][i][0]["payload"].as_str().unwrap(),
                        )
                        .map(Stored::Numeric);
                        for (j, role) in ["tr ansi", "cws first"].into_iter().enumerate() {
                            compose(
                                source,
                                value,
                                role,
                                temporal::Language::UsEnglish,
                                &e["result"]["sets"][0]["columns"][j],
                                &e["result"]["sets"][0]["rows"][0][j],
                                name,
                            );
                            checks += 1;
                        }
                    }
                    continue;
                }
                let source = numeric_helpers::kind(input["declaration"].as_str().unwrap());
                let role = input["role"].as_str().unwrap();
                if mode == "prepared IEEE RPC" {
                    for (i, e) in record["prepared"]["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        assert_eq!(
                            e["result"]["sets"][0]["columns"],
                            record["prepared"]["prepare"]["sets"][0]["columns"]
                        );
                        let value = numeric_helpers::wire(
                            source,
                            record["bindingWire"][i][0]["payload"].as_str().unwrap(),
                        );
                        if role == "native" {
                            continue;
                        }
                        for (j, cell) in e["result"]["sets"][0]["rows"][0]
                            .as_array()
                            .unwrap()
                            .iter()
                            .enumerate()
                        {
                            if role == "explicit style0" {
                                assert_eq!(
                                    cell.as_str(),
                                    numeric_text::text(source, value, Some(0))
                                        .unwrap()
                                        .as_deref()
                                );
                                controls += 1;
                            } else {
                                compose(
                                    source,
                                    value.map(Stored::Numeric),
                                    role,
                                    temporal::Language::UsEnglish,
                                    &e["result"]["sets"][0]["columns"][j],
                                    cell,
                                    name,
                                );
                                checks += 1;
                            }
                        }
                    }
                } else {
                    let value = if mode == "IEEE RPC" {
                        numeric_helpers::wire(
                            source,
                            record["parameters"][0]["wire"]["payload"].as_str().unwrap(),
                        )
                    } else {
                        input["scalarText"]
                            .as_str()
                            .map(|s| numeric_helpers::scalar(source, s))
                    };
                    if role == "native" {
                        continue;
                    }
                    for (j, cell) in record["result"]["sets"][0]["rows"][0]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        if role == "explicit style0" {
                            assert_eq!(
                                cell.as_str(),
                                numeric_text::text(source, value, Some(0))
                                    .unwrap()
                                    .as_deref()
                            );
                            controls += 1;
                        } else {
                            compose(
                                source,
                                value.map(Stored::Numeric),
                                role,
                                temporal::Language::UsEnglish,
                                &record["result"]["sets"][0]["columns"][j],
                                cell,
                                name,
                            );
                            checks += 1;
                        }
                    }
                }
            }
            assert_eq!(rejected, 77);
            totals.push((checks, controls));
        }
    }
    assert_eq!(totals.len(), 4);
    assert!(totals.iter().all(|n| *n == totals[0]));
    assert_eq!(totals[0].0 + totals[0].1, 2290);
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/float-default-grid.json")).unwrap();
    let mut grid = vec![];
    for c in fixture["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let (mut checks, mut controls) = (0, 0);
            for r in run.as_array().unwrap() {
                let input = &r["input"];
                let Some(decl) = input["declaration"].as_str() else {
                    continue;
                };
                let source = numeric_helpers::kind(decl);
                let check = |result: &Value,
                             value: Option<numeric_text::Value>,
                             checks: &mut usize,
                             controls: &mut usize| {
                    for control in input["controls"].as_array().unwrap() {
                        let role = control["role"].as_str().unwrap();
                        if role == "native" {
                            continue;
                        }
                        let set = control["set"].as_u64().unwrap() as usize;
                        for (j, cell) in result["sets"][set]["rows"][0]
                            .as_array()
                            .unwrap()
                            .iter()
                            .enumerate()
                        {
                            if role == "explicit style0" {
                                assert_eq!(
                                    cell.as_str(),
                                    numeric_text::text(source, value, Some(0))
                                        .unwrap()
                                        .as_deref()
                                );
                                *controls += 1;
                            } else {
                                let role = match (role, j) {
                                    ("translate", 0) => "tr ansi",
                                    ("translate", 1) => "tr unicode",
                                    ("concat_ws", 0) => "cws first",
                                    ("concat_ws", 1) => "cws unicode",
                                    _ => panic!("unknown control"),
                                };
                                compose(
                                    source,
                                    value.map(Stored::Numeric),
                                    role,
                                    temporal::Language::UsEnglish,
                                    &result["sets"][set]["columns"][j],
                                    cell,
                                    r["name"].as_str().unwrap(),
                                );
                                *checks += 1;
                            }
                        }
                    }
                };
                if r["prepared"].is_object() {
                    for (i, e) in r["prepared"]["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        assert_eq!(
                            e["result"]["sets"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|s| &s["columns"])
                                .collect::<Vec<_>>(),
                            r["prepared"]["prepare"]["sets"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|s| &s["columns"])
                                .collect::<Vec<_>>()
                        );
                        check(
                            &e["result"],
                            numeric_helpers::wire(
                                source,
                                r["bindingWire"][i][0]["payload"].as_str().unwrap(),
                            ),
                            &mut checks,
                            &mut controls,
                        );
                    }
                } else {
                    check(
                        &r["result"],
                        numeric_helpers::wire(source, input["payload"].as_str().unwrap()),
                        &mut checks,
                        &mut controls,
                    );
                }
            }
            grid.push((checks, controls));
        }
    }
    assert_eq!(grid, vec![(8728, 4364); 4]);
    println!("numeric composition {totals:?}; IEEE grid {grid:?}; 77 source failures/run retained");
}

#[test]
fn temporal_guid_composition_uses_native_stored_parts_and_actual_prepared_payloads() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/temporal-guid-format.json")).unwrap();
    let (mut checks, mut failures, mut controls) = (0, 0, 0);
    for c in fixture["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let records = run.as_array().unwrap();
            let key = |i: &Value| {
                format!(
                    "{}/{}/{}",
                    i["language"], i["declaration"], i["originalText"]
                )
            };
            let sources: std::collections::HashMap<_, _> = records
                .iter()
                .filter(|r| {
                    r["input"]["kind"] == "typed SQL source"
                        && r["input"]["role"] == "native stored parts"
                })
                .map(|r| (key(&r["input"]), r))
                .collect();
            assert_eq!(sources.len(), 366);
            for r in records {
                let i = &r["input"];
                let Some(decl) = i["declaration"].as_str() else {
                    continue;
                };
                let source = temporal_helpers::source(decl);
                let role = i["role"].as_str().unwrap();
                let lang = temporal_helpers::language(i["language"].as_str().unwrap());
                let check = |result: &Value,
                             value: Option<temporal::Stored>,
                             checks: &mut usize,
                             controls: &mut usize| {
                    if role == "native stored parts" {
                        return;
                    }
                    if role.starts_with("concat") || role.starts_with("translate") {
                        compose(
                            source,
                            value.map(Stored::Temporal),
                            role,
                            lang,
                            &result["sets"][0]["columns"][0],
                            &result["sets"][0]["rows"][0][0],
                            r["name"].as_str().unwrap(),
                        );
                        *checks += 1;
                    } else {
                        let profile = if role.starts_with("default cast") {
                            temporal::Profile::DefaultCast
                        } else if role.starts_with("explicit style0") {
                            temporal::Profile::ExplicitStyle(0)
                        } else {
                            assert!(role.starts_with("explicit style121"));
                            temporal::Profile::ExplicitStyle(121)
                        };
                        let p = temporal::contract(
                            source,
                            profile,
                            lang,
                            if role.ends_with("unicode") {
                                temporal::Domain::Utf16
                            } else {
                                temporal::Domain::Cp1252
                            },
                        )
                        .unwrap();
                        let actual = temporal::format(p, value).unwrap().map(|v| match v {
                            temporal::Text::Ansi(s) => s.encode_utf16().collect::<Vec<_>>(),
                            temporal::Text::Unicode(v) => v,
                        });
                        assert_eq!(
                            actual,
                            result["sets"][0]["rows"][0][0]
                                .as_str()
                                .map(|s| s.encode_utf16().collect::<Vec<_>>())
                        );
                        *controls += 1;
                    }
                };
                if i["kind"] == "typed SQL source" {
                    let original = sources[&key(i)];
                    if !original["result"]["errors"].as_array().unwrap().is_empty() {
                        assert_eq!(r["result"]["errors"], original["result"]["errors"]);
                        assert!(
                            r["result"]["sets"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .all(|s| s["rows"].as_array().unwrap().is_empty())
                        );
                        failures += 1;
                        continue;
                    }
                    check(
                        &r["result"],
                        temporal_helpers::native_value(source, &original["result"]),
                        &mut checks,
                        &mut controls,
                    );
                } else {
                    assert_eq!(i["kind"], "prepared typed source");
                    for e in r["prepared"]["executions"].as_array().unwrap() {
                        assert_eq!(
                            e["result"]["sets"][0]["columns"],
                            r["prepared"]["prepare"]["sets"][0]["columns"]
                        );
                        let wire = &e["wire"][0];
                        let value = if wire["length"] == "00" {
                            assert_eq!(wire["payload"], "");
                            None
                        } else {
                            Some(temporal_helpers::decode(
                                source,
                                &temporal_helpers::bytes(wire["payload"].as_str().unwrap()),
                                false,
                            ))
                        };
                        check(&e["result"], value, &mut checks, &mut controls);
                    }
                }
            }
        }
    }
    assert_eq!(failures, 300);
    assert_eq!(checks + controls, 20920);
    println!(
        "temporal composition {checks}, independent controls {controls}, retained construction failures {failures}"
    );
}

fn source_815(name: &str) -> Type {
    use Type::*;
    match name {
        "tinyint" => TinyInt,
        "smallint" => SmallInt,
        "int" => Int,
        "bigint" => BigInt,
        "bit" => Bit,
        "real" => Real,
        "float" => Float,
        "money" => Money,
        "smallmoney" => SmallMoney,
        "date" => Date,
        "datetime" => DateTime,
        "smalldatetime" => SmallDateTime,
        "guid" => UniqueIdentifier,
        "xml" => Xml,
        "variant" => Variant,
        "image" => Image,
        "text" => Text,
        "ntext" => Ntext,
        s if s.starts_with("decimal") => {
            let (p, s) = s[7..].split_once('_').unwrap();
            Decimal(DecimalType::new(p.parse().unwrap(), s.parse().unwrap()).unwrap())
        }
        s if s.starts_with("datetimeoffset") => {
            DateTimeOffset(Scale::new(s[14..].parse().unwrap()).unwrap())
        }
        s if s.starts_with("datetime2_") => {
            DateTime2(Scale::new(s[10..].parse().unwrap()).unwrap())
        }
        s if s.starts_with("time") => Time(Scale::new(s[4..].parse().unwrap()).unwrap()),
        s if s.starts_with("binary") || s.starts_with("varbinary") => {
            let fixed = s.starts_with("binary");
            let suffix = &s[if fixed { 6 } else { 9 }..];
            Binary(
                BinaryType::new(
                    fixed,
                    if suffix == "max" {
                        Length::Max
                    } else {
                        Length::Bounded(suffix.parse().unwrap())
                    },
                )
                .unwrap(),
            )
        }
        s => {
            let (family, suffix) = if let Some(s) = s.strip_prefix("nvarchar") {
                (Family::Nvarchar, s)
            } else if let Some(s) = s.strip_prefix("varchar") {
                (Family::Varchar, s)
            } else if let Some(s) = s.strip_prefix("nchar") {
                (Family::Nchar, s)
            } else {
                (Family::Char, s.strip_prefix("char").unwrap())
            };
            Character(
                CharacterType::new(
                    family,
                    if suffix == "max" {
                        Length::Max
                    } else {
                        Length::Bounded(suffix.parse().unwrap())
                    },
                )
                .unwrap(),
            )
        }
    }
}

#[derive(Clone)]
enum Payload {
    Character(Vec<u16>),
    Binary(Vec<u8>),
    Numeric(numeric_text::Value),
    Temporal(temporal::Stored),
}
impl Payload {
    fn borrowed(&self) -> Stored<'_> {
        match self {
            Self::Character(v) => Stored::Character(v),
            Self::Binary(v) => Stored::Binary(v),
            Self::Numeric(v) => Stored::Numeric(*v),
            Self::Temporal(v) => Stored::Temporal(*v),
        }
    }
}
fn payload_815(source: Type, r: &Value) -> Option<Payload> {
    let native = &r["result"]["sets"][0]["rows"][0][0];
    if native.is_null() {
        return None;
    }
    Some(match source {
        Type::Character(_) | Type::Text | Type::Ntext => {
            Payload::Character(native.as_str().unwrap().encode_utf16().collect())
        }
        Type::Binary(_) => {
            assert_eq!(native["kind"], "binary");
            Payload::Binary(numeric_helpers::bytes(native["value"].as_str().unwrap()))
        }
        Type::Bit => Payload::Numeric(numeric_text::Value::Integer(i64::from(
            native.as_bool().unwrap(),
        ))),
        Type::TinyInt | Type::SmallInt | Type::Int | Type::BigInt => {
            Payload::Numeric(numeric_text::Value::Integer(
                native
                    .as_i64()
                    .unwrap_or_else(|| native.as_str().unwrap().parse().unwrap()),
            ))
        }
        Type::Real => {
            let n = native.as_f64().unwrap();
            assert_eq!(f64::from(n as f32), n);
            Payload::Numeric(numeric_text::Value::RealBits((n as f32).to_bits()))
        }
        Type::Float => Payload::Numeric(numeric_text::Value::FloatBits(
            native.as_f64().unwrap().to_bits(),
        )),
        Type::Money | Type::SmallMoney => {
            let sql = r["sql"].as_str().unwrap();
            let scalar = sql
                .split_once("CAST(")
                .unwrap()
                .1
                .split_once(" AS ")
                .unwrap()
                .0;
            Payload::Numeric(numeric_helpers::scalar(source, scalar))
        }
        Type::Decimal(d) => {
            let coefficient = if d.scale() == 0 {
                i128::from(native.as_i64().unwrap())
            } else {
                let scalar = r["sql"]
                    .as_str()
                    .unwrap()
                    .split_once("CAST(")
                    .unwrap()
                    .1
                    .split_once(" AS ")
                    .unwrap()
                    .0;
                let places = scalar.split_once('.').map_or(0, |(_, s)| s.len());
                assert!(places <= usize::from(d.scale()));
                let coefficient: i128 = scalar.replace('.', "").parse().unwrap();
                coefficient * 10i128.pow(u32::from(d.scale()) - places as u32)
            };
            Payload::Numeric(numeric_text::Value::Decimal(
                Decimal::new(d.precision(), d.scale(), coefficient).unwrap(),
            ))
        }
        Type::Date | Type::Time(_) | Type::DateTime2(_) | Type::DateTimeOffset(_) => {
            // This independent native/default-CAST source observation retains exact
            // local fields, all declared fractional digits and the stored offset.
            // Lossy Date/nanosecondsDelta carriers are never used for modern values.
            let exact = r["result"]["sets"][0]["rows"][0][1].as_str().unwrap();
            let stored = match source {
                Type::Date => temporal::Stored::Date {
                    days: (msduck_core::datetime2::DateTime2::parse_iso(exact)
                        .unwrap()
                        .ticks()
                        / 864_000_000_000) as u32,
                },
                Type::Time(_) => temporal::Stored::Time {
                    ticks: msduck_core::datetime2::DateTime2::parse_iso(exact)
                        .unwrap()
                        .ticks()
                        .rem_euclid(864_000_000_000) as u64,
                },
                Type::DateTime2(_) => temporal::Stored::DateTime2(
                    msduck_core::datetime2::DateTime2::parse_iso(exact).unwrap(),
                ),
                Type::DateTimeOffset(_) => temporal::Stored::DateTimeOffset(
                    msduck_core::datetimeoffset::DateTimeOffset::parse_iso(exact).unwrap(),
                ),
                _ => unreachable!(),
            };
            Payload::Temporal(stored)
        }
        Type::DateTime | Type::SmallDateTime => {
            // Legacy native millisecond display is invertible on its discrete lattice;
            // check the inversion rather than treating it as modern tick evidence.
            assert_eq!(native["kind"], "date");
            let exact = native["value"].as_str().unwrap().strip_suffix('Z').unwrap();
            let n = msduck_core::datetime2::DateTime2::parse_iso(exact).unwrap();
            let days = n.ticks() / 864_000_000_000 - 693595;
            let day_ticks = n.ticks().rem_euclid(864_000_000_000);
            let stored = if source == Type::DateTime {
                let millis = day_ticks / 10_000;
                let ticks = (millis * 300 + 500) / 1000;
                assert_eq!((ticks * 1000 + 150) / 300, millis);
                temporal::Stored::DateTime {
                    days: days as i32,
                    ticks_300: ticks as u32,
                }
            } else {
                assert_eq!(day_ticks % 600_000_000, 0);
                temporal::Stored::SmallDateTime {
                    days: days as u32,
                    minutes: (day_ticks / 600_000_000) as u16,
                }
            };
            Payload::Temporal(stored)
        }
        Type::UniqueIdentifier => {
            let mut bytes = numeric_helpers::bytes(&native.as_str().unwrap().replace('-', ""));
            bytes[..4].reverse();
            bytes[4..6].reverse();
            bytes[6..8].reverse();
            Payload::Temporal(temporal::Stored::Guid(bytes.try_into().unwrap()))
        }
        Type::Xml | Type::Variant | Type::Image => {
            panic!("unsupported source never converted to a fabricated payload")
        }
    })
}
fn assert_sql(error: binding::Error, expected: &Value) {
    let binding::Error::Function(rules::Error::Sql(e)) = error else {
        panic!("expected captured SQL diagnostic, got {error:?}")
    };
    assert_eq!(json!(e.number), expected["number"]);
    assert_eq!(json!(e.state), expected["state"]);
    assert_eq!(json!(e.severity), expected["class"]);
    assert_eq!(e.message, expected["message"].as_str().unwrap());
}
#[test]
fn conversion_reference_all_families_binary_max_padding_null_and_prepared() {
    let f: Value = serde_json::from_str(include_str!(
        "../../../reference/concat-text-conversion.json"
    ))
    .unwrap();
    let mut totals = vec![];
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let records = run.as_array().unwrap();
            let mut checks = 0;
            let mut errors = 0;
            for r in records {
                let name = r["name"].as_str().unwrap();
                let parts: Vec<_> = name.splitn(3, ' ').collect();
                if parts[0] == "literal"
                    || parts.len() != 3
                    || !matches!(parts[1], "value" | "null")
                    || !(parts[2].starts_with("cws ") || parts[2].starts_with("tr"))
                {
                    continue;
                }
                let source = source_815(parts[0]);
                let operation = if parts[2].starts_with("tr") {
                    rules::Function::Translate
                } else {
                    rules::Function::ConcatWs
                };
                let unicode = parts[2].contains("unicode");
                if let Some(expected) = r["result"]["errors"].as_array().unwrap().first() {
                    let mut ds = vec![character(unicode, 1); 3];
                    ds[if operation == rules::Function::Translate {
                        0
                    } else {
                        1
                    }] = declaration(source);
                    assert_sql(
                        binding::plan(
                            operation,
                            &ds,
                            DEFAULT,
                            &catalog(),
                            temporal::Language::UsEnglish,
                            None,
                        )
                        .unwrap_err(),
                        expected,
                    );
                    errors += 1;
                    continue;
                }
                let original = records
                    .iter()
                    .find(|x| x["name"] == format!("{} {} source", parts[0], parts[1]))
                    .unwrap();
                let payload = payload_815(source, original);
                compose(
                    source,
                    payload.as_ref().map(Payload::borrowed),
                    parts[2],
                    temporal::Language::UsEnglish,
                    &r["result"]["sets"][0]["columns"][0],
                    &r["result"]["sets"][0]["rows"][0][0],
                    name,
                );
                checks += 1;
            }
            for r in records
                .iter()
                .filter(|r| r["name"].as_str().unwrap().starts_with("priority tr "))
            {
                let name = r["name"].as_str().unwrap();
                let unicode = name.ends_with("unicode");
                let long = name.contains("max long");
                let (source, value) = if long {
                    (source_815("varbinarymax"), Some(vec![65; 9001]))
                } else {
                    let parts: Vec<_> = name.split(' ').collect();
                    let source = match parts[2] {
                        "BINARY(10)" => source_815("binary10"),
                        "VARBINARY(10)" => source_815("varbinary10"),
                        "VARBINARY(8000)" => source_815("varbinary8000"),
                        "VARBINARY(MAX)" => source_815("varbinarymax"),
                        _ => panic!("uncaptured source"),
                    };
                    let value = if parts[3] == "NULL" {
                        None
                    } else {
                        let n = records
                            .iter()
                            .find(|r| {
                                r["name"]
                                    == format!(
                                        "{} value source",
                                        match parts[2] {
                                            "BINARY(10)" => "binary10",
                                            "VARBINARY(10)" => "varbinary10",
                                            "VARBINARY(8000)" => "varbinary8000",
                                            _ => "varbinarymax",
                                        }
                                    )
                            })
                            .unwrap();
                        let Some(Payload::Binary(v)) = payload_815(source, n) else {
                            panic!()
                        };
                        Some(v)
                    };
                    (source, value)
                };
                let ds = [
                    declaration(source),
                    character(unicode, 1),
                    character(unicode, 1),
                ];
                let plan = binding::plan(
                    rules::Function::Translate,
                    &ds,
                    DEFAULT,
                    &catalog(),
                    temporal::Language::UsEnglish,
                    None,
                )
                .unwrap();
                let values = [
                    value.as_deref().map(Stored::Binary),
                    Some(Stored::Character(&[65])),
                    Some(Stored::Character(&[90])),
                ];
                let actual =
                    binding::evaluate_with_keys(&plan, &values, &|u| Some(u.to_vec())).unwrap();
                assert_eq!(
                    actual,
                    r["result"]["sets"][0]["rows"][0][0]
                        .as_str()
                        .map(|s| s.encode_utf16().collect::<Vec<_>>()),
                    "{name}"
                );
                assert_descriptor(&plan, &r["result"]["sets"][0]["columns"][0], name);
                if long {
                    assert_eq!(
                        r["result"]["sets"][0]["rows"][0][1]
                            .as_str()
                            .unwrap()
                            .parse::<u64>()
                            .unwrap(),
                        actual.as_ref().unwrap().len() as u64 * if unicode { 2 } else { 1 }
                    );
                }
                checks += 1;
            }
            for (name, source) in [
                ("prepared int conversion", Type::Int),
                ("prepared decimal conversion", source_815("decimal18_4")),
                ("prepared binary max conversion", source_815("varbinarymax")),
            ] {
                let r = records.iter().find(|r| r["name"] == name).unwrap();
                for e in r["prepared"]["executions"].as_array().unwrap() {
                    assert_eq!(
                        e["result"]["sets"][0]["columns"],
                        r["prepared"]["prepare"]["sets"][0]["columns"]
                    );
                    let v = &e["values"]["p"];
                    let payload = if v.is_null() {
                        None
                    } else if let Type::Binary(_) = source {
                        assert_eq!(v["kind"], "binary");
                        Some(Payload::Binary(numeric_helpers::bytes(
                            v["value"].as_str().unwrap(),
                        )))
                    } else {
                        Some(Payload::Numeric(numeric_helpers::scalar(
                            source,
                            &v.to_string(),
                        )))
                    };
                    for (j, operation) in [rules::Function::Translate, rules::Function::ConcatWs]
                        .into_iter()
                        .enumerate()
                    {
                        let mut ds = vec![character(false, 1); 3];
                        let pos = if operation == rules::Function::Translate {
                            0
                        } else {
                            1
                        };
                        ds[pos] = declaration(source);
                        let plan = binding::plan(
                            operation,
                            &ds,
                            DEFAULT,
                            &catalog(),
                            temporal::Language::UsEnglish,
                            None,
                        )
                        .unwrap();
                        let mapping = matches!(source, Type::Binary(_))
                            && operation == rules::Function::Translate;
                        let (a, z) = (vec![65], vec![90]);
                        let empty = [];
                        let mut values = vec![Some(Stored::Character(empty.as_slice())); 3];
                        values[pos] = payload.as_ref().map(Payload::borrowed);
                        if mapping {
                            values[1] = Some(Stored::Character(&a));
                            values[2] = Some(Stored::Character(&z));
                        }
                        let actual =
                            binding::evaluate_with_keys(&plan, &values, &|u| Some(u.to_vec()))
                                .unwrap();
                        assert_eq!(
                            actual,
                            e["result"]["sets"][0]["rows"][0][j]
                                .as_str()
                                .map(|s| s.encode_utf16().collect::<Vec<_>>()),
                            "{name}"
                        );
                        assert_descriptor(&plan, &e["result"]["sets"][0]["columns"][j], name);
                        checks += 1;
                    }
                }
            }
            totals.push((checks, errors));
        }
    }
    assert_eq!(totals.len(), 4);
    assert!(totals.iter().all(|n| *n == totals[0]));
    println!("conversion family composition {totals:?}");
}

#[test]
fn legacy_argument_order_unicode_prescan_and_full_captured_diagnostics() {
    let f: Value =
        serde_json::from_str(include_str!("../../../reference/concat-legacy-family.json")).unwrap();
    let (mut errors, mut checks) = (0, 0);
    let decl = |s: &str| match s {
        "XML" => Type::Xml,
        "SQL_VARIANT" => Type::Variant,
        "IMAGE" => Type::Image,
        "TEXT" => Type::Text,
        "NTEXT" => Type::Ntext,
        _ => panic!(),
    };
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            for r in run.as_array().unwrap() {
                let i = &r["input"];
                if i["kind"] != "isolated" && i["kind"] != "mixed" {
                    continue;
                }
                let separator = i["separator"].as_str().unwrap();
                let mut ds = vec![if separator == "NULL" {
                    Declaration::null_literal()
                } else {
                    character(separator == "N''", 1)
                }];
                let mut values = vec![if separator == "NULL" {
                    None
                } else {
                    Some(Stored::Character(&[]))
                }];
                let a = [97];
                let members = if i["kind"] == "isolated" {
                    vec![i]
                } else {
                    i["members"].as_array().unwrap().iter().collect()
                };
                for m in members {
                    let source = decl(m["declaration"].as_str().unwrap());
                    ds.push(declaration(source));
                    values.push(
                        if matches!(source, Type::Text | Type::Ntext) && m["value"] != "NULL" {
                            Some(Stored::Character(&a))
                        } else {
                            None
                        },
                    );
                }
                if i["kind"] == "isolated" {
                    ds.push(Declaration::null_literal());
                    values.push(None);
                }
                let p = binding::plan(
                    rules::Function::ConcatWs,
                    &ds,
                    DEFAULT,
                    &catalog(),
                    temporal::Language::UsEnglish,
                    None,
                );
                if let Some(expected) = r["result"]["errors"].as_array().unwrap().first() {
                    assert_sql(p.unwrap_err(), expected);
                    errors += 1;
                } else {
                    let p = p.unwrap();
                    assert_descriptor(
                        &p,
                        &r["result"]["sets"][0]["columns"][0],
                        r["name"].as_str().unwrap(),
                    );
                    let actual = binding::evaluate(&p, &values, &|_, _| None).unwrap();
                    assert_eq!(
                        actual,
                        r["result"]["sets"][0]["rows"][0][0]
                            .as_str()
                            .map(|s| s.encode_utf16().collect::<Vec<_>>())
                    );
                    checks += 1;
                }
            }
        }
    }
    assert_eq!((errors, checks), (936, 144));
}

#[test]
fn literal_null_and_ordered_typed_rows_preserve_captured_declarations() {
    let f: Value = serde_json::from_str(include_str!(
        "../../../reference/concat-text-conversion.json"
    ))
    .unwrap();
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let r = run.as_array().unwrap();
            for (name, operation) in [
                ("literal null tr", rules::Function::Translate),
                ("literal null cws", rules::Function::ConcatWs),
            ] {
                let mut ds = vec![character(false, 1); 3];
                let pos = if operation == rules::Function::Translate {
                    0
                } else {
                    1
                };
                ds[pos] = Declaration::null_literal();
                let p = binding::plan(
                    operation,
                    &ds,
                    DEFAULT,
                    &catalog(),
                    temporal::Language::UsEnglish,
                    None,
                )
                .unwrap();
                let mut values = vec![Some(Stored::Character(&[])); 3];
                values[pos] = None;
                let actual = binding::evaluate(&p, &values, &|_, _| None).unwrap();
                let record = r.iter().find(|r| r["name"] == name).unwrap();
                assert_descriptor(&p, &record["result"]["sets"][0]["columns"][0], name);
                assert_eq!(
                    actual,
                    record["result"]["sets"][0]["rows"][0][0]
                        .as_str()
                        .map(|s| s.encode_utf16().collect::<Vec<_>>())
                );
            }
            let record = r.iter().find(|r| r["name"] == "source order").unwrap();
            let ds = [
                character(false, 1),
                declaration(Type::Int),
                character(false, 1),
            ];
            let p = binding::plan(
                rules::Function::ConcatWs,
                &ds,
                DEFAULT,
                &catalog(),
                temporal::Language::UsEnglish,
                None,
            )
            .unwrap();
            assert_descriptor(
                &p,
                &record["result"]["sets"][0]["columns"][1],
                "source order",
            );
            assert_eq!(
                record["result"]["sets"][0]["rows"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            for (row, value) in record["result"]["sets"][0]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .zip([
                    Some(Stored::Numeric(numeric_text::Value::Integer(42))),
                    None,
                ])
            {
                let actual = binding::evaluate(
                    &p,
                    &[
                        Some(Stored::Character(&[124])),
                        value,
                        Some(Stored::Character(&[])),
                    ],
                    &|_, _| None,
                )
                .unwrap();
                assert_eq!(
                    actual,
                    row[1]
                        .as_str()
                        .map(|s| s.encode_utf16().collect::<Vec<_>>())
                );
            }
        }
    }
}
#[test]
fn declarations_null_range_payload_and_allocation_barriers_are_explicit() {
    use binding::Error;
    let create = |source| {
        binding::plan(
            rules::Function::ConcatWs,
            &[
                character(false, 1),
                declaration(source),
                character(false, 1),
            ],
            DEFAULT,
            &catalog(),
            temporal::Language::UsEnglish,
            None,
        )
    };
    let p = create(Type::Int).unwrap();
    let bad = [
        Some(Stored::Character(&[])),
        Some(Stored::Numeric(numeric_text::Value::Integer(i64::MAX))),
        Some(Stored::Character(&[])),
    ];
    assert_eq!(
        binding::evaluate(&p, &bad, &|_, _| None),
        Err(Error::Numeric(numeric_text::Error::InvalidPayload))
    );
    let null = [
        Some(Stored::Character(&[])),
        None,
        Some(Stored::Character(&[])),
    ];
    assert_eq!(
        binding::evaluate(&p, &null, &|_, _| None).unwrap(),
        Some(vec![])
    );
    assert_eq!(p.result().declaration.length(), Length::Bounded(14));
    assert_eq!(
        binding::evaluate(&p, &[None, None], &|_, _| None),
        Err(Error::InvalidPayload)
    );
    assert_eq!(
        binding::evaluate(&p, &[None, Some(Stored::Binary(&[1])), None], &|_, _| None),
        Err(Error::InvalidPayload)
    );
    let mut ds = [
        character(false, 1),
        declaration(Type::Int),
        character(false, 1),
    ];
    ds[1].style = Some(0);
    assert!(matches!(
        binding::plan(
            rules::Function::ConcatWs,
            &ds,
            DEFAULT,
            &catalog(),
            temporal::Language::UsEnglish,
            None
        ),
        Err(Error::Conversion(
            msduck_sql::concat_text_conversion::Unsupported::Style
        ))
    ));
    assert!(matches!(
        create(Type::Decimal(DecimalType::new(19, 5).unwrap())),
        Err(Error::Function(rules::Error::UnknownConversionWidth))
    ));
    assert!(matches!(
        create(Type::Binary(
            BinaryType::new(false, Length::Bounded(11)).unwrap()
        )),
        Err(Error::Function(rules::Error::UnknownConversionWidth))
    ));
    let ds = [
        character(false, 1),
        declaration(Type::Date),
        character(false, 1),
    ];
    assert!(matches!(
        binding::plan(
            rules::Function::ConcatWs,
            &ds,
            DEFAULT,
            &catalog(),
            temporal::Language::Unknown,
            None
        ),
        Err(Error::Temporal(temporal::Error::UnknownLanguage))
    ));
    let p = create(Type::Binary(
        BinaryType::new(true, Length::Bounded(10)).unwrap(),
    ))
    .unwrap();
    assert_eq!(
        binding::evaluate(
            &p,
            &[None, Some(Stored::Binary(&[65, 66])), None],
            &|_, _| None
        ),
        Err(Error::Conversion(
            msduck_sql::concat_text_conversion::Unsupported::InvalidBinaryValue
        ))
    );
    let source = Type::Character(CharacterType::new(Family::Nvarchar, Length::Max).unwrap());
    let p = binding::plan(
        rules::Function::ConcatWs,
        &vec![declaration(source); 3],
        DEFAULT,
        &catalog(),
        temporal::Language::UsEnglish,
        None,
    )
    .unwrap();
    let half = vec![97; rules::MAX_INPUT_UNITS / 2 + 1];
    assert_eq!(
        binding::evaluate(
            &p,
            &[
                None,
                Some(Stored::Character(&half)),
                Some(Stored::Character(&half))
            ],
            &|_, _| None
        ),
        Err(Error::AggregateInputLimit)
    );
    let ds = [
        Declaration::null_literal(),
        declaration(Type::Int),
        Declaration::null_literal(),
    ];
    let p = binding::plan(
        rules::Function::ConcatWs,
        &ds,
        DEFAULT,
        &catalog(),
        temporal::Language::UsEnglish,
        None,
    )
    .unwrap();
    assert_eq!(
        binding::evaluate(&p, &[Some(Stored::Character(&[])), None, None], &|_, _| {
            None
        }),
        Err(Error::InvalidPayload)
    );
}

#[test]
fn original_binary_unicode_units_first_mapping_and_mismatch_remain_exact() {
    let source = Type::Binary(BinaryType::new(false, Length::Max).unwrap());
    let ds = [declaration(source), character(true, 2), character(true, 2)];
    let p = binding::plan(
        rules::Function::Translate,
        &ds,
        DEFAULT,
        &catalog(),
        temporal::Language::UsEnglish,
        None,
    )
    .unwrap();
    assert_eq!(p.result().declaration.length(), Length::Max);
    let bytes = [0x41, 0x42, 0x3d, 0xd8, 0x00, 0xde, 0x41];
    let mapping = [0x4241, 0x4241];
    let replacement = [90, 88];
    let values = [
        Some(Stored::Binary(&bytes)),
        Some(Stored::Character(&mapping)),
        Some(Stored::Character(&replacement)),
    ];
    assert_eq!(
        binding::evaluate_with_keys(&p, &values, &|u| Some(u.to_vec())).unwrap(),
        Some(vec![90, 0xd83d, 0xde00, 65])
    );
    assert_eq!(
        binding::evaluate(&p, &values, &|a, b| Some(a == b)).unwrap(),
        Some(vec![90, 0xd83d, 0xde00, 65])
    );
    let mismatched = [
        Some(Stored::Binary(&[])),
        Some(Stored::Character(&mapping)),
        Some(Stored::Character(&[90])),
    ];
    let Err(binding::Error::Function(rules::Error::Sql(e))) =
        binding::evaluate_with_keys(&p, &mismatched, &|_| None::<u16>)
    else {
        panic!("empty input must not suppress9828")
    };
    assert_eq!((e.number, e.state, e.severity), (9828, 3, 16));
    assert_eq!(
        e.message,
        "The second and third arguments of the TRANSLATE built-in function must contain an equal number of characters."
    );
    assert_eq!(
        binding::evaluate_with_keys(
            &p,
            &[
                None,
                Some(Stored::Character(&mapping)),
                Some(Stored::Character(&[90]))
            ],
            &|_| None::<u16>
        )
        .unwrap(),
        None
    );
}

#[test]
fn plan_snapshot_precedes_prepared_bindings_and_unknown_encodings() {
    let mut declarations = [
        character(false, 1),
        declaration(Type::BigInt),
        character(false, 1),
    ];
    let p = binding::plan(
        rules::Function::ConcatWs,
        &declarations,
        DEFAULT,
        &catalog(),
        temporal::Language::UsEnglish,
        None,
    )
    .unwrap();
    assert_eq!(p.result().declaration.length(), Length::Bounded(26));
    declarations[1].source = Some(Type::Int);
    for value in [Some(i64::MAX), None, Some(i64::MIN), Some(i64::MAX)] {
        let actual = binding::evaluate(
            &p,
            &[
                Some(Stored::Character(&[])),
                value.map(|n| Stored::Numeric(numeric_text::Value::Integer(n))),
                Some(Stored::Character(&[])),
            ],
            &|_, _| None,
        )
        .unwrap();
        assert_eq!(
            actual,
            Some(value.map_or_else(Vec::new, |v| v.to_string().encode_utf16().collect()))
        );
        assert_eq!(p.result().declaration.length(), Length::Bounded(26));
    }
    let mut utf8 = catalog();
    utf8[0].encoding = rules::Encoding::Utf8;
    assert!(matches!(
        binding::plan(
            rules::Function::ConcatWs,
            &declarations,
            DEFAULT,
            &utf8,
            temporal::Language::UsEnglish,
            None
        ),
        Err(binding::Error::Function(rules::Error::UnknownEncoding))
    ));
    declarations[1].collation = Some(Label::CoercibleDefault(DEFAULT.into()));
    assert!(matches!(
        binding::plan(
            rules::Function::ConcatWs,
            &declarations,
            DEFAULT,
            &catalog(),
            temporal::Language::UsEnglish,
            None
        ),
        Err(binding::Error::Function(rules::Error::InvalidDeclaration))
    ));
}
