#[path = "../src/percentile_order_type.rs"]
mod policy;

use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::Label,
    result::{Origin, Properties},
    types::{BinaryType, DecimalType, Scale, Type},
};
use policy::{BindingError, Kind, declaration};
use serde_json::Value;

fn source(name: &str) -> Type {
    match name {
        "BIT" => Type::Bit,
        "TINYINT" => Type::TinyInt,
        "SMALLINT" => Type::SmallInt,
        "INT" | "Int" => Type::Int,
        "BIGINT" | "BigInt" => Type::BigInt,
        "REAL" | "FLOAT(24)" | "Real" => Type::Real,
        "FLOAT(53)" | "Float" => Type::Float,
        "DECIMAL(10,2)" | "Decimal" => Type::Decimal(DecimalType::new(10, 2).unwrap()),
        "DECIMAL(38,8)" => Type::Decimal(DecimalType::new(38, 8).unwrap()),
        "MONEY" => Type::Money,
        "SMALLMONEY" => Type::SmallMoney,
        "CHAR(8)" => character(Family::Char, Length::Bounded(8)),
        "VARCHAR(8)" | "VarChar" => character(Family::Varchar, Length::Bounded(8)),
        "NCHAR(8)" => character(Family::Nchar, Length::Bounded(8)),
        "NVARCHAR(8)" | "NVarChar" => character(Family::Nvarchar, Length::Bounded(8)),
        "VARCHAR(MAX)" => character(Family::Varchar, Length::Max),
        "NVARCHAR(MAX)" => character(Family::Nvarchar, Length::Max),
        "DATE" => Type::Date,
        "TIME(3)" => Type::Time(Scale::new(3).unwrap()),
        "SMALLDATETIME" => Type::SmallDateTime,
        "DATETIME" => Type::DateTime,
        "DATETIME2(7)" => Type::DateTime2(Scale::new(7).unwrap()),
        "DATETIMEOFFSET(7)" => Type::DateTimeOffset(Scale::new(7).unwrap()),
        "BINARY(4)" => Type::Binary(BinaryType::new(true, Length::Bounded(4)).unwrap()),
        "VARBINARY(4)" | "VarBinary" => {
            Type::Binary(BinaryType::new(false, Length::Bounded(4)).unwrap())
        }
        "UNIQUEIDENTIFIER" | "UniqueIdentifier" => Type::UniqueIdentifier,
        "XML" => Type::Xml,
        "SQL_VARIANT" => Type::Variant,
        _ => panic!("unmapped captured declaration {name}"),
    }
}
fn character(family: Family, length: Length) -> Type {
    Type::Character(CharacterType::new(family, length).unwrap())
}

fn descriptor(data_type: Type, actual: &Value) {
    let (name, width) = match data_type {
        Type::Bit => ("BitN", Some(1)),
        Type::TinyInt => ("IntN", Some(1)),
        Type::SmallInt => ("IntN", Some(2)),
        Type::Int => ("IntN", Some(4)),
        Type::BigInt => ("IntN", Some(8)),
        Type::Real => ("FloatN", Some(4)),
        Type::Float => ("FloatN", Some(8)),
        Type::SmallMoney => ("MoneyN", Some(4)),
        Type::Money => ("MoneyN", Some(8)),
        Type::Decimal(d) => {
            assert_eq!(actual["precision"], d.precision());
            assert_eq!(actual["scale"], d.scale());
            // Tedious's DecimalN length field is not SQL storage capacity.
            ("DecimalN", None)
        }
        Type::Character(c) => {
            let (name, multiplier) = match c.family() {
                Family::Char => ("Char", 1),
                Family::Varchar => ("VarChar", 1),
                Family::Nchar => ("NChar", 2),
                Family::Nvarchar => ("NVarChar", 2),
            };
            let width = match c.length() {
                Length::Bounded(n) => u64::from(n) * multiplier,
                Length::Max => 65535,
            };
            assert_eq!(actual["collation"]["codepage"], "CP1252");
            (name, Some(width))
        }
        Type::Binary(b) => (
            if b.fixed() { "Binary" } else { "VarBinary" },
            Some(match b.length() {
                Length::Bounded(n) => u64::from(n),
                Length::Max => panic!("uncaptured binary MAX"),
            }),
        ),
        Type::Date => ("Date", None),
        Type::Time(s) => {
            assert_eq!(actual["scale"], s.get());
            ("Time", None)
        }
        Type::SmallDateTime => ("DateTimeN", Some(4)),
        Type::DateTime => ("DateTimeN", Some(8)),
        Type::DateTime2(s) => {
            assert_eq!(actual["scale"], s.get());
            ("DateTime2", None)
        }
        Type::DateTimeOffset(s) => {
            assert_eq!(actual["scale"], s.get());
            ("DateTimeOffset", None)
        }
        Type::UniqueIdentifier => ("UniqueIdentifier", Some(16)),
        Type::Variant => ("Variant", Some(8009)),
        _ => panic!("unexpected successful declaration {data_type:?}"),
    };
    assert_eq!(actual["type"], name);
    if let Some(width) = width {
        assert_eq!(actual["length"], width);
    }
    assert_eq!(actual["flags"].as_u64().unwrap() & 1, 1);
}

#[test]
fn both_captures_prove_all_batch_and_prepared_declarations() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/percentile-order-type.json"
    ))
    .unwrap();
    let mut records = 0;
    let mut phases = 0;
    for run in fixture["runs"].as_array().unwrap() {
        for record in run.as_array().unwrap() {
            let name = record["name"].as_str().unwrap();
            if matches!(name, "server version" | "session reusable") {
                continue;
            }
            let prepared = record["mode"] == "prepared";
            let words: Vec<_> = name.split_whitespace().collect();
            let kind_at = usize::from(prepared);
            let kind = match words[kind_at] {
                "CONT" => Kind::Continuous,
                "DISC" => Kind::Discrete,
                _ => panic!("unexpected request {name}"),
            };
            let input = source(words[kind_at + 1]);
            let label = Label::CoercibleDefault("SQL_Latin1_General_CP1_CI_AS".into());
            let actual = declaration(kind, Some(input), Some(&label));
            let mut results = vec![if prepared {
                &record["preparation"]
            } else {
                &record["result"]
            }];
            if prepared {
                results.extend(
                    record["executions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|entry| &entry["result"]),
                );
            }
            for result in results {
                match &actual {
                    Ok(Some(output)) => {
                        assert!(result["errors"].as_array().unwrap().is_empty(), "{name}");
                        assert_eq!(output.properties, Properties::expression(true));
                        assert_eq!(output.properties.origin, Origin::Expression);
                        assert_eq!(
                            output.collation.as_ref(),
                            matches!(input, Type::Character(_)).then_some(&label)
                        );
                        descriptor(output.data_type, &result["sets"][0]["columns"][1]);
                    }
                    Err(BindingError::Sql(error)) => {
                        let errors = result["errors"].as_array().unwrap();
                        assert_eq!(errors[0]["number"], error.number, "{name}");
                        assert_eq!(errors[0]["state"], error.state, "{name}");
                        assert_eq!(errors[0]["class"], error.severity, "{name}");
                        assert_eq!(errors[0]["message"], error.message, "{name}");
                        assert!(result["sets"].as_array().unwrap().is_empty());
                        if prepared {
                            assert_eq!(errors.len(), 2);
                            assert_eq!(errors[1]["number"], 8180);
                            assert_eq!(record["executions"].as_array().unwrap().len(), 0);
                        } else {
                            assert_eq!(errors.len(), 1);
                        }
                    }
                    other => panic!("unexpected policy for captured {name}: {other:?}"),
                }
                phases += 1;
            }
            records += 1;
        }
    }
    assert_eq!(records, 384);
    assert_eq!(phases, 472);
}

#[test]
fn unknown_and_uncaptured_sources_are_explicit_and_collation_is_caller_owned() {
    let explicit = Label::Explicit("caller-controlled-collation".into());
    for kind in [Kind::Continuous, Kind::Discrete] {
        assert_eq!(declaration(kind, None, Some(&explicit)), Ok(None));
        for source in [Type::Text, Type::Ntext, Type::Image] {
            assert_eq!(
                declaration(kind, Some(source), Some(&explicit)),
                Err(BindingError::Unsupported(source))
            );
        }
    }
    let source = character(Family::Nvarchar, Length::Bounded(8));
    let result = declaration(Kind::Discrete, Some(source), Some(&explicit))
        .unwrap()
        .unwrap();
    assert_eq!(result.data_type, source);
    assert_eq!(result.collation, Some(explicit));
    assert_eq!(
        declaration(Kind::Continuous, Some(Type::Int), None)
            .unwrap()
            .unwrap()
            .collation,
        None
    );
}
