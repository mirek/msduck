use msduck_core::{types::Type, value::Value};
use msduck_sql::{
    datalength, dialect::ServerDialect, expression_metadata::storage, parameter::Parameter,
};
use sqlparser::{ast::*, parser::Parser};
use std::collections::HashMap;

fn expression(sql: &str) -> Expr {
    Parser::new(&ServerDialect)
        .try_with_sql(sql)
        .unwrap()
        .parse_expr()
        .unwrap()
}

fn captured_columns() -> HashMap<String, DataType> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/quoted-session-identifiers.json"
    ))
    .unwrap();
    let first = &fixture["runs"][0][1]["result"]["sets"][0]["columns"];
    assert_eq!(
        first,
        &fixture["runs"][1][1]["result"]["sets"][0]["columns"]
    );
    first
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
                "NVarChar" => {
                    let bytes = column["length"].as_u64().unwrap();
                    assert_eq!(bytes % 2, 0);
                    DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                        length: bytes / 2,
                        unit: None,
                    }))
                }
                other => panic!("unexpected captured column type {other}"),
            };
            (column["name"].as_str().unwrap().to_lowercase(), kind)
        })
        .collect()
}

fn column(expr: &Expr, columns: &HashMap<String, DataType>) -> Option<DataType> {
    let name = match expr {
        Expr::Identifier(id) => &id.value,
        Expr::CompoundIdentifier(ids) => &ids.last()?.value,
        _ => return None,
    };
    columns.get(&name.to_lowercase()).cloned()
}

#[test]
fn quoted_columns_keep_captured_types_despite_conflicting_scalar_declarations() {
    let columns = captured_columns();
    for value in [Value::BigInt(42), Value::Null] {
        let parameters = HashMap::from([(
            "@p".into(),
            Parameter {
                value: value.clone(),
                data_type: Type::BigInt,
            },
        )]);
        for sql in ["[@p]", "\"@P\"", "([@p])", "q.[@p]", "-([@p])"] {
            let expr = expression(sql);
            let original = expr.clone();
            for _ in 0..2 {
                assert_eq!(
                    storage::kind(&expr, &parameters, &|expr| column(expr, &columns)),
                    Some(DataType::Int(None)),
                    "{sql}"
                );
                assert_eq!(expr, original);
                assert_eq!(parameters["@p"].data_type, Type::BigInt);
                assert_eq!(parameters["@p"].value, value);
            }
        }
        assert_eq!(
            storage::kind(&expression("@p"), &parameters, &|_| panic!("scalar lookup")),
            Some(DataType::BigInt(None))
        );
    }
}

#[test]
fn unknown_quoted_columns_do_not_gain_a_parameter_type() {
    let parameters = HashMap::from([(
        "@p".into(),
        Parameter {
            value: Value::Null,
            data_type: Type::BigInt,
        },
    )]);
    for sql in ["[@p]", "\"@p\"", "([@p])", "q.[@p]", "-[@p]"] {
        assert_eq!(
            storage::kind(&expression(sql), &parameters, &|_| None),
            None,
            "{sql}"
        );
    }
}

#[test]
fn quoted_counter_columns_retain_character_width_through_consumers() {
    let columns = captured_columns();
    for sql in [
        "[@@OPTIONS]",
        "\"@@OPTIONS\"",
        "q.[@@OPTIONS]",
        "MAX([@@OPTIONS])",
    ] {
        assert_eq!(
            storage::kind(&expression(sql), &HashMap::new(), &|expr| column(
                expr, &columns
            )),
            Some(columns["@@options"].clone()),
            "{sql}"
        );
    }
    assert_eq!(
        storage::kind(&expression("[@@TRANCOUNT]"), &HashMap::new(), &|expr| {
            column(expr, &columns)
        }),
        Some(DataType::SmallInt(None))
    );
}

#[test]
fn storage_length_lowering_uses_the_column_width_and_keeps_the_column_operand() {
    let columns = captured_columns();
    let parameters = HashMap::from([(
        "@p".into(),
        Parameter {
            value: Value::BigInt(42),
            data_type: Type::BigInt,
        },
    )]);
    for (sql, expected_width) in [("DATALENGTH([@p])", "4"), ("DATALENGTH(@p)", "8")] {
        let mut expr = expression(sql);
        datalength::lower(&mut expr, &parameters, &|expr| column(expr, &columns)).unwrap();
        let Expr::Cast { expr, .. } = expr else {
            panic!("typed length")
        };
        let Expr::Case {
            conditions,
            else_result,
            ..
        } = *expr
        else {
            panic!("NULL-aware length")
        };
        assert_eq!(conditions.len(), 1);
        let Expr::IsNull(operand) = &conditions[0].condition else {
            panic!("original operand")
        };
        assert_eq!(
            operand.as_ref(),
            &expression(if expected_width == "4" { "[@p]" } else { "@p" })
        );
        assert_eq!(
            else_result.unwrap().as_ref(),
            &msduck_sql::expr::number(expected_width)
        );
    }
}
