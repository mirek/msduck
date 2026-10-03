#[path = "../src/numeric_text.rs"]
mod numeric_text;
use msduck_core::{
    types::{DecimalType, Type},
    value::Decimal,
};
use numeric_text::{Error, Value as Numeric, text};
use serde_json::Value;

fn kind(name: &str) -> Type {
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
fn scalar(kind: Type, source: &str) -> Numeric {
    match kind {
        Type::Decimal(d) => Numeric::Decimal(
            Decimal::new(
                d.precision(),
                d.scale(),
                source.replace('.', "").parse().unwrap(),
            )
            .unwrap(),
        ),
        Type::Money | Type::SmallMoney => {
            Numeric::Money(msduck_core::money::parse_text(source).unwrap())
        }
        _ => Numeric::Integer(source.parse().unwrap()),
    }
}
fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn wire(kind: Type, hex: &str) -> Option<Numeric> {
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
fn compare(result: &Value, role: &str, output: Option<&str>, name: &str) -> usize {
    if role == "native" {
        return 0;
    }
    let row = result["sets"][0]["rows"][0].as_array().unwrap();
    for cell in row {
        let expected = if role.starts_with("cws") {
            Some(
                if role.ends_with("separator") && role != "cws null separator" {
                    output.map_or_else(|| "ab".to_owned(), |s| format!("a{s}b"))
                } else {
                    output.unwrap_or("").to_owned()
                },
            )
        } else {
            output.map(str::to_owned)
        };
        assert_eq!(cell.as_str(), expected.as_deref(), "{name} ({role})");
        if let Some(expected) = expected {
            assert_eq!(
                cell.as_str().unwrap().encode_utf16().collect::<Vec<_>>(),
                expected.encode_utf16().collect::<Vec<_>>(),
                "{name}"
            );
        }
    }
    row.len()
}
#[test]
fn every_retained_function_and_explicit_control_and_prepared_binding() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/concat-numeric-format.json"
    ))
    .unwrap();
    let mut totals = Vec::new();
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let mut comparisons = 0;
            let mut rejected = 0;
            for record in run.as_array().unwrap() {
                let input = &record["input"];
                let Some(input_kind) = input["kind"].as_str() else {
                    continue;
                };
                let name = record["name"].as_str().unwrap();
                if input_kind == "rejected typed scalar" {
                    rejected += 1;
                    assert!(!record["result"]["errors"].as_array().unwrap().is_empty());
                    continue;
                }
                if input_kind == "prepared scalar RPC" {
                    let decl = &record["declarations"][0];
                    let source = if decl["type"] == "Decimal" {
                        Type::Decimal(
                            DecimalType::new(
                                decl["options"]["precision"].as_u64().unwrap() as u8,
                                decl["options"]["scale"].as_u64().unwrap() as u8,
                            )
                            .unwrap(),
                        )
                    } else {
                        kind(decl["type"].as_str().unwrap())
                    };
                    for (i, e) in record["prepared"]["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        let value = wire(
                            source,
                            record["bindingWire"][i][0]["payload"].as_str().unwrap(),
                        );
                        let output = text(source, value, None).unwrap();
                        let row = e["result"]["sets"][0]["rows"][0].as_array().unwrap();
                        assert_eq!(
                            row[0].as_str(),
                            output.as_deref(),
                            "{name} TRANSLATE binding {i}"
                        );
                        assert_eq!(
                            row[1].as_str(),
                            Some(output.as_deref().unwrap_or("")),
                            "{name} CONCAT_WS binding {i}"
                        );
                        comparisons += 2;
                    }
                    continue;
                }
                let source = kind(input["declaration"].as_str().unwrap());
                let role = input["role"].as_str().unwrap();
                if input_kind == "prepared IEEE RPC" {
                    for (i, e) in record["prepared"]["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                    {
                        let output = text(
                            source,
                            wire(
                                source,
                                record["bindingWire"][i][0]["payload"].as_str().unwrap(),
                            ),
                            None,
                        )
                        .unwrap();
                        comparisons += compare(&e["result"], role, output.as_deref(), name);
                    }
                } else {
                    let value = if input_kind == "IEEE RPC" {
                        wire(
                            source,
                            record["parameters"][0]["wire"]["payload"].as_str().unwrap(),
                        )
                    } else {
                        input["scalarText"].as_str().map(|s| scalar(source, s))
                    };
                    let output = text(source, value, None).unwrap();
                    comparisons += compare(&record["result"], role, output.as_deref(), name);
                }
            }
            assert_eq!(rejected, 77);
            totals.push(comparisons);
        }
    }
    assert_eq!(totals.len(), 4);
    assert_eq!(totals[0], 2290);
    assert!(totals.iter().all(|n| *n == totals[0]));
    println!(
        "{} exact string/unit comparisons per run, four runs; 77 source-overflow controls kept separate",
        totals[0]
    );
}
#[test]
fn kinds_ranges_null_styles_and_nonfinite_are_explicit() {
    for (kind, value) in [
        (Type::Bit, 2),
        (Type::TinyInt, -1),
        (Type::TinyInt, 256),
        (Type::SmallInt, 32768),
        (Type::Int, i64::MAX),
    ] {
        assert_eq!(
            text(kind, Some(Numeric::Integer(value)), None),
            Err(Error::InvalidPayload)
        );
    }
    assert_eq!(
        text(Type::SmallMoney, Some(Numeric::Money(i64::MAX)), None),
        Err(Error::InvalidPayload)
    );
    assert_eq!(
        text(Type::Real, Some(Numeric::FloatBits(0)), None),
        Err(Error::InvalidPayload)
    );
    assert_eq!(
        text(
            Type::Float,
            Some(Numeric::FloatBits(f64::NAN.to_bits())),
            None
        ),
        Err(Error::NonFinite)
    );
    assert_eq!(
        text(
            Type::Real,
            Some(Numeric::RealBits(f32::INFINITY.to_bits())),
            None
        ),
        Err(Error::NonFinite)
    );
    assert_eq!(text(Type::Xml, None, None), Err(Error::UnsupportedSource));
    assert_eq!(text(Type::Int, None, Some(1)), Err(Error::UnsupportedStyle));
    let d = Decimal::new(8, 2, 120).unwrap();
    assert_eq!(
        text(
            Type::Decimal(DecimalType::new(9, 2).unwrap()),
            Some(Numeric::Decimal(d)),
            None
        ),
        Err(Error::InvalidPayload)
    );
    assert_eq!(text(Type::Float, None, Some(0)), Ok(None));
}
