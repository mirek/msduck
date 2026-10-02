use msduck_core::{types::Type, value::Value};
use msduck_sql::{
    aggregate_columns::{self, Snapshot},
    dialect::ServerDialect,
    parameter::Parameter,
};
use sqlparser::{ast::*, parser::Parser};
use std::collections::HashMap;

fn snapshot() -> Snapshot {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/quoted-session-identifiers.json"
    ))
    .unwrap();
    let columns = &fixture["runs"][0][1]["result"]["sets"][0]["columns"];
    assert_eq!(
        columns,
        &fixture["runs"][1][1]["result"]["sets"][0]["columns"]
    );
    let fields = columns
        .as_array()
        .unwrap()
        .iter()
        .map(|column| {
            let kind = match column["type"].as_str().unwrap() {
                "IntN" => {
                    assert_eq!(column["length"], 4);
                    DataType::Int(None)
                }
                "SmallInt" => DataType::SmallInt(None),
                "NVarChar" => DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                    length: column["length"].as_u64().unwrap() / 2,
                    unit: None,
                })),
                other => panic!("unexpected captured type {other}"),
            };
            (column["name"].as_str().unwrap().into(), Some(kind))
        })
        .collect();
    Snapshot::from([(("dbo".into(), "t".into()), Ok(fields))])
}

fn parameters(value: Value) -> HashMap<String, Parameter> {
    HashMap::from([(
        "@p".into(),
        Parameter {
            value,
            data_type: Type::BigInt,
        },
    )])
}

fn statement(sql: &str) -> Statement {
    Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0)
}

#[test]
fn actual_resolver_keeps_quoted_column_width_separate_from_scalar_width() {
    let catalog = snapshot();
    let before = catalog.clone();
    for value in [Value::BigInt(42), Value::Null] {
        let parameters = parameters(value.clone());
        for sql in [
            "SELECT DATALENGTH([@p]),DATALENGTH(@p) FROM dbo.t",
            "SELECT DATALENGTH(\"@p\"),DATALENGTH(@p) FROM dbo.t WHERE 1=0",
            "SELECT DATALENGTH(([@p])),DATALENGTH(@p) FROM dbo.t",
            "SELECT DATALENGTH(q.[@p]),DATALENGTH(@p) FROM dbo.t q",
            "SELECT DATALENGTH([@p]),DATALENGTH(@p) FROM (SELECT [@p] FROM dbo.t) q",
            "WITH q AS (SELECT [@p] FROM dbo.t) SELECT DATALENGTH([@p]),DATALENGTH(@p) FROM q",
        ] {
            let mut ast = statement(sql);
            aggregate_columns::resolve(&catalog, &mut ast, &parameters)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let text = ast.to_string();
            assert!(text.contains("ELSE 4 END"), "{sql}: {text}");
            assert!(text.contains("ELSE 8 END"), "{sql}: {text}");
            assert!(text.contains("@p"), "{sql}: {text}");
            assert_eq!(catalog, before);
            assert_eq!(parameters["@p"].data_type, Type::BigInt);
            assert_eq!(parameters["@p"].value, value);
        }
    }
}

#[test]
fn actual_resolver_keeps_unknown_ambiguous_and_shadowed_columns_unknown() {
    let catalog = snapshot();
    let parameters = parameters(Value::Null);
    for sql in [
        "SELECT DATALENGTH([@p]) FROM missing",
        "SELECT DATALENGTH([@missing]) FROM dbo.t",
        "SELECT DATALENGTH([@p]) FROM dbo.t a CROSS JOIN dbo.t b",
        "SELECT (SELECT DATALENGTH([@p]) FROM missing) FROM dbo.t",
        "SELECT (SELECT DATALENGTH(q.[@p]) FROM (SELECT 1 AS n) q) FROM dbo.t q",
    ] {
        assert_eq!(
            aggregate_columns::resolve(&catalog, &mut statement(sql), &parameters),
            Err("unsupported DATALENGTH source type".into()),
            "{sql}"
        );
    }
}

#[test]
fn real_counter_columns_and_join_operands_use_row_bindings() {
    let catalog = snapshot();
    let mut ast = statement("SELECT DATALENGTH([@@OPTIONS]),DATALENGTH([@@TRANCOUNT]) FROM dbo.t");
    aggregate_columns::resolve(&catalog, &mut ast, &HashMap::new()).unwrap();
    let text = ast.to_string();
    assert!(text.contains("__msduck_carrier_datalength"), "{text}");
    assert!(text.contains("ELSE 2 END"), "{text}");

    let mut ast = statement("SELECT 1 FROM dbo.t a JOIN (SELECT 7 AS n) b ON [@p]=b.n");
    aggregate_columns::resolve(&catalog, &mut ast, &parameters(Value::Null)).unwrap();
    assert!(ast.to_string().contains("\"a\".\"@p\""), "{ast}");
}
