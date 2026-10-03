#[path = "../src/concat_text_conversion.rs"]
mod conversion;
use conversion::*;
use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{BinaryType, DecimalType, Scale, Type},
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../reference/concat-text-conversion.json"
    ))
    .unwrap()
}
fn source(name: &str) -> Type {
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
fn domain(column: &Value) -> Domain {
    if column["type"] == "NVarChar" {
        Domain::Utf16Le
    } else {
        assert_eq!(column["type"], "VarChar");
        Domain::Cp1252
    }
}
fn wire(length: Length, domain: Domain) -> u64 {
    match length {
        Length::Max => 65535,
        Length::Bounded(n) => u64::from(n) * if domain == Domain::Utf16Le { 2 } else { 1 },
    }
}

#[test]
fn declaration_contracts_match_every_applicable_family_in_all_four_runs() {
    let f = fixture();
    let mut runs = 0;
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            runs += 1;
            let mut checked = 0;
            for record in run.as_array().unwrap() {
                let name = record["name"].as_str().unwrap();
                let parts: Vec<_> = name.splitn(3, ' ').collect();
                if name.starts_with("literal ")
                    || parts.len() != 3
                    || !matches!(parts[1], "value" | "null")
                    || !(parts[2].starts_with("cws ") || parts[2].starts_with("tr"))
                {
                    continue;
                }
                let src = source(parts[0]);
                let function = if parts[2].starts_with("tr") {
                    Function::Translate
                } else {
                    Function::ConcatWs
                };
                let result = &record["result"];
                if !result["errors"].as_array().unwrap().is_empty() {
                    let error = contract(function, src, Domain::Cp1252, None).unwrap_err();
                    let Unsupported::FunctionSource { number } = error else {
                        panic!("{name}: {error:?}")
                    };
                    assert_eq!(
                        u64::from(number),
                        result["errors"][0]["number"].as_u64().unwrap(),
                        "{name}"
                    );
                    checked += 1;
                    continue;
                }
                let column = &result["sets"][0]["columns"][0];
                let d = domain(column);
                let plan = contract(function, src, d, None).unwrap();
                assert_eq!(plan.source, src);
                let mut width = wire(plan.allocation, d);
                if function == Function::ConcatWs
                    && !parts[2].starts_with("cws isolated")
                    && width != 65535
                {
                    width = (width + if d == Domain::Utf16Le { 4 } else { 2 }).min(8000);
                }
                assert_eq!(column["length"].as_u64().unwrap(), width, "{name}");
                assert_eq!(
                    column["flags"],
                    if function == Function::ConcatWs {
                        32
                    } else {
                        33
                    },
                    "{name}"
                );
                if parts[1] == "null" {
                    assert_eq!(
                        result["sets"][0]["rows"][0][0],
                        if function == Function::ConcatWs {
                            Value::String(if parts[2] == "cws separator" {
                                "ab".into()
                            } else {
                                String::new()
                            })
                        } else {
                            Value::Null
                        }
                    );
                }
                checked += 1;
            }
            assert_eq!(checked, 784);
        }
    }
    assert_eq!(runs, 4);
}

fn binary_bytes(value: &Value) -> Option<Vec<u8>> {
    if value.is_null() {
        return None;
    }
    assert_eq!(value["kind"], "binary");
    let hex = value["value"].as_str().unwrap();
    Some(
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect(),
    )
}
fn json_text(text: Option<conversion::Text>) -> Value {
    match text {
        None => Value::Null,
        Some(conversion::Text::Ansi(s)) => Value::String(s),
        Some(conversion::Text::Unicode(u)) => Value::String(String::from_utf16(&u).unwrap()),
    }
}

#[test]
fn stored_binary_domains_match_captured_cws_and_translate_values() {
    let f = fixture();
    let families = [
        "binary2",
        "binary10",
        "varbinary2",
        "varbinary10",
        "varbinary8000",
        "varbinarymax",
    ];
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let records = run.as_array().unwrap();
            for family in families {
                for binding in ["value", "null"] {
                    let native = records
                        .iter()
                        .find(|r| r["name"] == format!("{family} {binding} source"))
                        .unwrap();
                    let bytes = binary_bytes(&native["result"]["sets"][0]["rows"][0][0]);
                    for operation in [
                        "tr",
                        "tr unicode",
                        "cws isolated first",
                        "cws isolated last",
                        "cws unicode",
                    ] {
                        let record = records
                            .iter()
                            .find(|r| r["name"] == format!("{family} {binding} {operation}"))
                            .unwrap();
                        let col = &record["result"]["sets"][0]["columns"][0];
                        let function = if operation.starts_with("tr") {
                            Function::Translate
                        } else {
                            Function::ConcatWs
                        };
                        let plan = contract(function, source(family), domain(col), None).unwrap();
                        let text = binary_text(plan, bytes.as_deref()).unwrap();
                        let actual = if function == Function::ConcatWs && text.is_none() {
                            Value::String(String::new())
                        } else {
                            json_text(text)
                        };
                        assert_eq!(
                            actual, record["result"]["sets"][0]["rows"][0][0],
                            "{}",
                            record["name"]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn max_odd_byte_padding_and_priority_translation_are_not_ansi_promotion() {
    let f = fixture();
    let bytes = vec![65; 9001];
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            let records = run.as_array().unwrap();
            for d in [Domain::Cp1252, Domain::Utf16Le] {
                let p = contract(Function::Translate, source("varbinarymax"), d, None).unwrap();
                assert_eq!(p.allocation, Length::Max);
                let mut text = binary_text(p, Some(&bytes)).unwrap().unwrap();
                match &mut text {
                    conversion::Text::Ansi(s) => *s = s.replace('A', "Z"),
                    conversion::Text::Unicode(u) => u.iter_mut().for_each(|x| {
                        if *x == 65 {
                            *x = 90
                        }
                    }),
                }
                let name = if d == Domain::Cp1252 {
                    "priority tr binary max long"
                } else {
                    "priority tr binary max long unicode"
                };
                let record = records.iter().find(|r| r["name"] == name).unwrap();
                assert_eq!(
                    json_text(Some(text)),
                    record["result"]["sets"][0]["rows"][0][0]
                );
                assert_eq!(record["result"]["sets"][0]["columns"][0]["length"], 65535);
            }
        }
    }
}

#[test]
fn prepared_metadata_remains_declaration_only_across_all_bindings() {
    let f = fixture();
    for c in f["containers"].as_array().unwrap() {
        for run in c["runs"].as_array().unwrap() {
            for (name, src) in [
                ("prepared int conversion", Type::Int),
                ("prepared binary max conversion", source("varbinarymax")),
                ("prepared decimal conversion", source("decimal18_4")),
            ] {
                let r = run
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["name"] == name)
                    .unwrap();
                let p = &r["prepared"];
                for result in std::iter::once(&p["prepare"]).chain(
                    p["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|e| &e["result"]),
                ) {
                    for (i, func) in [Function::Translate, Function::ConcatWs]
                        .into_iter()
                        .enumerate()
                    {
                        let col = &result["sets"][0]["columns"][i];
                        let d = domain(col);
                        let plan = contract(func, src, d, None).unwrap();
                        let mut n = wire(plan.allocation, d);
                        if func == Function::ConcatWs && n != 65535 {
                            n += 2;
                        }
                        assert_eq!(col["length"].as_u64().unwrap(), n);
                    }
                }
            }
        }
    }
}

#[test]
fn unknown_formats_declarations_and_invalid_storage_values_are_explicit() {
    assert_eq!(
        contract(Function::Translate, Type::Int, Domain::Unknown, None),
        Err(Unsupported::Domain)
    );
    assert_eq!(
        contract(Function::Translate, Type::Int, Domain::Cp1252, Some(0)),
        Err(Unsupported::Style)
    );
    assert_eq!(
        contract(
            Function::ConcatWs,
            source("decimal18_2"),
            Domain::Cp1252,
            None
        ),
        Err(Unsupported::Declaration)
    );
    assert_eq!(
        contract(Function::ConcatWs, source("time1"), Domain::Cp1252, None),
        Err(Unsupported::Declaration)
    );
    assert_eq!(
        contract(
            Function::ConcatWs,
            source("varbinary3"),
            Domain::Cp1252,
            None
        ),
        Err(Unsupported::Declaration)
    );
    for f in [Function::ConcatWs, Function::Translate] {
        assert_eq!(
            contract(f, source("nvarcharmax"), Domain::Cp1252, None),
            Err(Unsupported::Domain)
        );
    }
    let numeric = contract(Function::ConcatWs, Type::Int, Domain::Cp1252, None).unwrap();
    assert_eq!(binary_text(numeric, None), Err(Unsupported::Format));
    let fixed = contract(
        Function::Translate,
        source("binary10"),
        Domain::Utf16Le,
        None,
    )
    .unwrap();
    assert_eq!(
        binary_text(fixed, Some(b"AB")),
        Err(Unsupported::InvalidBinaryValue)
    );
    assert_eq!(binary_text(fixed, None), Ok(None));
    let variable = contract(
        Function::Translate,
        source("varbinary2"),
        Domain::Utf16Le,
        None,
    )
    .unwrap();
    assert_eq!(
        binary_text(variable, Some(b"ABC")),
        Err(Unsupported::InvalidBinaryValue)
    );
    assert_eq!(
        binary_text(variable, Some(&[0x41])),
        Ok(Some(conversion::Text::Unicode(vec![65])))
    );
    assert_eq!(
        binary_text(variable, Some(&[0x00, 0xd8])),
        Ok(Some(conversion::Text::Unicode(vec![0xd800])))
    );
    let max = contract(
        Function::Translate,
        source("varbinarymax"),
        Domain::Cp1252,
        None,
    )
    .unwrap();
    assert_eq!(
        binary_text(max, Some(&vec![0; INPUT_LIMIT + 1])),
        Err(Unsupported::InputLimit)
    );
    let mut forged = max;
    forged.allocation = Length::Bounded(8000);
    assert_eq!(binary_text(forged, None), Err(Unsupported::Declaration));
}
