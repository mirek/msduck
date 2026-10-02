use msduck_core::{
    character::{CharacterType, Family, Length},
    collation::Label,
    diagnostic::SqlError,
    types::{DecimalType, Type},
};
use msduck_sql::{
    dialect::ServerDialect,
    greatest_least::{Function, *},
    sql_type,
};
use serde_json::{Value as Json, json};
use sqlparser::{ast::*, parser::Parser};
use std::{collections::HashMap, ops::ControlFlow};
const CI: &str = "SQL_Latin1_General_CP1_CI_AS";
fn fixture() -> Json {
    serde_json::from_str(include_str!("../../../reference/greatest-least.json")).unwrap()
}
fn runs(f: &Json) -> Vec<&Vec<Json>> {
    f["containers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| {
            c["runs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_array().unwrap())
        })
        .collect()
}
fn character(f: Family, n: u16) -> Type {
    Type::Character(CharacterType::new(f, Length::Bounded(n)).unwrap())
}
fn decimal(p: u8, s: u8) -> Type {
    Type::Decimal(DecimalType::new(p, s).unwrap())
}
fn declaration(text: &str) -> Type {
    let e = Parser::new(&ServerDialect)
        .try_with_sql(&format!("CAST(NULL AS {text})"))
        .unwrap()
        .parse_expr()
        .unwrap();
    let Expr::Cast { data_type, .. } = e else {
        panic!("cast")
    };
    sql_type::declaration(&data_type).unwrap()
}
fn args(f: &sqlparser::ast::Function) -> Vec<&Expr> {
    let FunctionArguments::List(list) = &f.args else {
        panic!("args")
    };
    list.args
        .iter()
        .map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
            _ => panic!("scalar argument"),
        })
        .collect()
}
fn call(f: &sqlparser::ast::Function, env: &HashMap<String, Argument>) -> Result<Plan, Error> {
    let function = match f.name.to_string().to_uppercase().as_str() {
        "GREATEST" => Function::Greatest,
        "LEAST" => Function::Least,
        _ => panic!("function"),
    };
    let a: Vec<_> = args(f).into_iter().map(|e| operand(e, env)).collect();
    plan(function, &a)
}
fn operand(e: &Expr, env: &HashMap<String, Argument>) -> Argument {
    let mut result = match e {
        Expr::Nested(e) | Expr::UnaryOp { expr: e, .. } => operand(e, env),
        Expr::Value(v) => match &v.value {
            Value::Null => Argument::untyped_null(),
            Value::Number(n, _) => {
                if n.contains(['e', 'E']) {
                    Argument::typed(Type::Float, Some(false))
                } else if let Some((whole, fraction)) = n.split_once('.') {
                    let mut a = Argument::typed(
                        decimal((whole.len() + fraction.len()) as u8, fraction.len() as u8),
                        Some(false),
                    );
                    a.decimal_family = DecimalFamily::Numeric;
                    a
                } else {
                    let mut a = Argument::typed(Type::Int, Some(false));
                    a.origin = Origin::IntegerLiteral(n.len().min(10) as u8);
                    a
                }
            }
            Value::SingleQuotedString(s) | Value::NationalStringLiteral(s) => {
                let family = if matches!(v.value, Value::NationalStringLiteral(_)) {
                    Family::Nvarchar
                } else {
                    Family::Varchar
                };
                let mut a = Argument::typed(
                    character(family, s.encode_utf16().count().max(1) as u16),
                    Some(false),
                );
                a.collation = Some(Label::CoercibleDefault(CI.into()));
                a
            }
            Value::HexStringLiteral(s) => Argument::typed(
                declaration(&format!("varbinary({})", s.len() / 2)),
                Some(false),
            ),
            _ => panic!("literal {e}"),
        },
        Expr::Cast {
            expr, data_type, ..
        } => {
            let inner = operand(expr, env);
            let mut a = Argument::typed(sql_type::declaration(data_type).unwrap(), Some(true));
            a.decimal_family = if matches!(data_type, DataType::Numeric(_)) {
                DecimalFamily::Numeric
            } else {
                DecimalFamily::Decimal
            };
            if matches!(a.data_type, Some(Type::Character(_))) {
                a.collation = inner
                    .collation
                    .or_else(|| Some(Label::CoercibleDefault(CI.into())));
            }
            a
        }
        Expr::Identifier(i) => env
            .get(&i.value.to_lowercase())
            .unwrap_or_else(|| panic!("identifier {i}"))
            .clone(),
        Expr::Collate { expr, collation } => {
            let mut a = operand(expr, env);
            a.collation = Some(Label::Explicit(collation.to_string()));
            a
        }
        Expr::BinaryOp { .. } => Argument::typed(Type::Int, Some(true)), // captured division argument; evaluation is root-side
        Expr::Function(f) => match f.name.to_string().to_uppercase().as_str() {
            "GREATEST" | "LEAST" => {
                let p = call(f, env).unwrap();
                Argument {
                    data_type: Some(p.declaration.data_type),
                    decimal_family: p.declaration.decimal_family,
                    nullable: p.declaration.nullable,
                    collation: p.declaration.collation,
                    origin: Origin::Expression,
                }
            }
            "MAX" | "MIN" => {
                let mut a = operand(args(f)[0], env);
                a.origin = Origin::Expression;
                a.nullable = Some(true);
                a
            }
            "REPLICATE" => operand(args(f)[0], env),
            _ => panic!("operand {e}"),
        },
        _ => panic!("operand {e}"),
    };
    // Every catalog/default collation is explicitly supplied by this fixture adapter.
    if matches!(result.data_type, Some(Type::Character(_))) && result.collation.is_none() {
        result.collation = Some(Label::CoercibleDefault(CI.into()));
    }
    result
}
fn environment(record: &Json) -> HashMap<String, Argument> {
    let mut env = HashMap::new();
    for (name, kind, nullable, collation) in [
        ("nn1", Type::Int, false, None),
        ("nn2", Type::Int, false, None),
        ("n1", Type::Int, true, None),
        ("n2", Type::Int, true, None),
        (
            "cs",
            character(Family::Varchar, 10),
            true,
            Some("Latin1_General_CS_AS"),
        ),
        (
            "bin",
            character(Family::Varchar, 10),
            true,
            Some("Latin1_General_BIN"),
        ),
        ("txt", Type::Text, true, None),
        ("ntxt", Type::Ntext, true, None),
        ("img", Type::Image, true, None),
    ] {
        let mut a = Argument::typed(kind, Some(nullable));
        a.origin = Origin::Column;
        a.collation = collation.map(|n| Label::Implicit(n.into()));
        env.insert(name.into(), a);
    }
    if let Some(params) = record["parameters"].as_array() {
        for p in params {
            let text = match p["type"].as_str().unwrap() {
                "Decimal" => format!(
                    "decimal({},{})",
                    p["options"]["precision"], p["options"]["scale"]
                ),
                "VarChar" | "NVarChar" | "VarBinary" => format!(
                    "{}({})",
                    p["type"].as_str().unwrap(),
                    p["options"]["length"]
                ),
                "DateTime2" => format!("datetime2({})", p["options"]["scale"]),
                n => n.into(),
            };
            let mut a = Argument::typed(declaration(&text), Some(true));
            if matches!(a.data_type, Some(Type::Character(_))) {
                a.collation = Some(Label::CoercibleDefault(CI.into()));
            }
            env.insert(format!("@{}", p["name"].as_str().unwrap()), a);
        }
    }
    env
}
fn parsed(record: &Json) -> Vec<Statement> {
    Parser::parse_sql(&ServerDialect, record["sql"].as_str().unwrap()).unwrap()
}
fn scalar_calls(statements: &[Statement]) -> Vec<sqlparser::ast::Function> {
    let mut calls = Vec::new();
    let _ = visit_expressions(&statements.to_vec(), |e| {
        if let Expr::Function(f) = e
            && matches!(
                f.name.to_string().to_uppercase().as_str(),
                "GREATEST" | "LEAST"
            )
        {
            calls.push(f.clone());
        }
        ControlFlow::<()>::Continue(())
    });
    calls
}
fn error_matches(error: &SqlError, raw: &Json) {
    assert_eq!(
        json!([error.number, error.state, error.severity, &error.message]),
        json!([raw["number"], raw["state"], raw["class"], raw["message"]])
    );
}
#[test]
fn every_capture_declaration_and_compile_error_uses_explicit_inputs() {
    let f = fixture();
    let captures = runs(&f);
    assert_eq!(captures.len(), 4);
    assert_eq!(captures[0].len(), 221);
    for records in captures {
        assert_eq!(records, runs(&f)[0]);
        let mut count = 0;
        for (i, record) in records.iter().enumerate() {
            if record["name"].as_str().unwrap().starts_with("describe") || i < 3 {
                continue;
            }
            let statements = parsed(record);
            let original = statements.clone();
            let env = environment(record);
            let calls = scalar_calls(&statements);
            assert!(!calls.is_empty(), "{}", record["name"]);
            for (call_index, c) in calls.iter().enumerate() {
                let a: Vec<_> = args(c).iter().map(|e| operand(e, &env)).collect();
                let before = a.clone();
                let p = call(c, &env);
                assert_eq!(a, before);
                if let Err(Error::Sql(e)) = &p {
                    if call_index == 0 {
                        error_matches(e, &record["result"]["errors"][0]);
                    }
                } else {
                    assert!(p.is_ok(), "{}: {p:?}", record["name"]);
                }
                count += 1;
            }
            assert_eq!(statements, original);
            // Describe directly projected calls; wrappers/WHERE/ORDER/context execution stay root-side.
            if let Some(next) = records.get(i + 1)
                && next["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("describe"))
            {
                let rows = next["result"]["sets"][0]["rows"].as_array().unwrap();
                if rows[0][8].is_null() {
                    assert_eq!(calls.len(), rows.len());
                    for (c, row) in calls.iter().zip(rows) {
                        let p = call(c, &env).unwrap();
                        assert_eq!(
                            p.declaration.data_type,
                            declaration(row[2].as_str().unwrap()),
                            "{}",
                            record["name"]
                        );
                        let numeric = row[2].as_str().unwrap().starts_with("numeric(");
                        assert_eq!(
                            p.declaration.decimal_family == DecimalFamily::Numeric,
                            numeric,
                            "{}",
                            record["name"]
                        );
                        assert_eq!(
                            p.declaration.nullable,
                            row[6].as_bool(),
                            "{}",
                            record["name"]
                        );
                        assert_eq!(
                            p.declaration.collation.as_ref().and_then(Label::name),
                            row[7].as_str(),
                            "{}",
                            record["name"]
                        );
                        let columns = if i == 219 {
                            &record["result"]["executions"][0]["result"]["sets"][0]["columns"]
                        } else {
                            &record["result"]["sets"][0]["columns"]
                        };
                        let column = &columns[row[0].as_u64().unwrap() as usize - 1];
                        assert_eq!(
                            p.declaration.flags(),
                            column["flags"].as_u64().map(|n| n as u16),
                            "{}",
                            record["name"]
                        );
                    }
                }
            }
        }
        assert!(count > 200);
    }
}

// This test adapter supplies conversions and collation keys independently of
// selection. It supports the retained small numeric/ASCII examples only.
fn raw_value(e: &Expr, env: &HashMap<String, Argument>, values: &HashMap<String, Json>) -> Json {
    match e {
        Expr::Nested(e) | Expr::Collate { expr: e, .. } => raw_value(e, env, values),
        Expr::UnaryOp { expr, .. } => json!(-raw_value(expr, env, values).as_f64().unwrap()),
        Expr::Value(v) => match &v.value {
            Value::Null => Json::Null,
            Value::Number(n, _) => json!(n.parse::<f64>().unwrap()),
            Value::SingleQuotedString(s) | Value::NationalStringLiteral(s) => json!(s),
            _ => panic!("value {e}"),
        },
        Expr::Cast {
            expr, data_type, ..
        } => {
            let mut v = raw_value(expr, env, values);
            if let Type::Character(c) = sql_type::declaration(data_type).unwrap()
                && matches!(c.family(), Family::Char | Family::Nchar)
                && !v.is_null()
            {
                let Length::Bounded(n) = c.length() else {
                    panic!("fixed")
                };
                v = json!(format!(
                    "{:<width$}",
                    v.as_str().unwrap(),
                    width = n as usize
                ));
            }
            v
        }
        Expr::Identifier(i) => values[&i.value.to_lowercase()].clone(),
        Expr::Function(f) => {
            let p = call(f, env).unwrap();
            let raw: Vec<_> = args(f).iter().map(|e| raw_value(e, env, values)).collect();
            let converted: Vec<_> = raw.iter().map(|v| Ok(convert(v, &p.declaration))).collect();
            p.select(&converted).unwrap().map_or(Json::Null, |n| {
                logical_value(&converted[n].as_ref().unwrap().clone())
            })
        }
        _ => panic!("value {e}"),
    }
}
fn convert(v: &Json, d: &Declaration) -> Comparable {
    if v.is_null() {
        return Comparable::Null;
    }
    match d.data_type {
        Type::Character(c) => {
            let mut s = v.as_str().unwrap().to_owned();
            if matches!(c.family(), Family::Char | Family::Nchar) {
                let Length::Bounded(n) = c.length() else {
                    panic!("fixed")
                };
                s = format!("{s:<width$}", width = n as usize);
            }
            let collation = d
                .collation
                .as_ref()
                .and_then(Label::name)
                .unwrap()
                .to_owned();
            let key = if collation.ends_with("_BIN") {
                s.trim_end_matches(' ').chars().map(u32::from).collect()
            } else if collation.ends_with("_CS_AS") {
                s.trim_end_matches(' ')
                    .chars()
                    .flat_map(|c| [u32::from(c.to_ascii_lowercase()), u32::from(c)])
                    .collect()
            } else {
                s.trim_end_matches(' ')
                    .to_ascii_lowercase()
                    .chars()
                    .map(u32::from)
                    .collect()
            };
            Comparable::Character {
                units: s.encode_utf16().collect(),
                sort_key: key,
                collation,
            }
        }
        Type::Real | Type::Float => {
            let n = v
                .as_f64()
                .unwrap_or_else(|| v.as_str().unwrap().parse().unwrap());
            Comparable::Float(if d.data_type == Type::Real {
                f64::from(n as f32)
            } else {
                n
            })
        }
        t => {
            let scale = match t {
                Type::Decimal(d) => d.scale(),
                Type::Money | Type::SmallMoney => 4,
                _ => 0,
            };
            let n = v
                .as_f64()
                .unwrap_or_else(|| v.as_str().unwrap().parse().unwrap());
            Comparable::Exact {
                coefficient: (n * 10f64.powi(scale.into())).round() as i128,
                scale,
            }
        }
    }
}
fn json_number(n: f64) -> Json {
    if n.fract() == 0.0 && n >= i64::MIN as f64 && n < i64::MAX as f64 {
        json!(n as i64)
    } else {
        json!(n)
    }
}
fn captured_value(v: &Comparable, kind: Type) -> Json {
    if kind == Type::Bit
        && let Comparable::Exact { coefficient, .. } = v
    {
        return json!(*coefficient != 0);
    }
    if kind == Type::BigInt
        && let Comparable::Exact { coefficient, .. } = v
    {
        json!(coefficient.to_string())
    } else {
        logical_value(v)
    }
}
fn logical_value(v: &Comparable) -> Json {
    match v {
        Comparable::Null => Json::Null,
        Comparable::Exact { coefficient, scale } => {
            json_number(*coefficient as f64 / 10f64.powi((*scale).into()))
        }
        Comparable::Float(n) => json_number(*n),
        Comparable::Character { units, .. } => json!(String::from_utf16(units).unwrap()),
        _ => panic!("logical value"),
    }
}
#[test]
fn captured_numeric_character_and_parameter_rows_select_converted_values() {
    let f = fixture();
    for records in runs(&f) {
        for i in (3..=101)
            .step_by(2)
            .chain([153, 155, 198, 200, 202, 204, 206, 208, 210, 212, 218])
        {
            if matches!(i, 19 | 43 | 45 | 97) {
                continue;
            } // compile error, exact 38-digit keys, conversion error covered separately
            let record = &records[i];
            let env = environment(record);
            let values: HashMap<_, _> = record["parameters"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .map(|p| {
                            (
                                format!("@{}", p["name"].as_str().unwrap()),
                                p["value"].clone(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let statements = parsed(record);
            let Statement::Query(q) = &statements[0] else {
                panic!("query")
            };
            let SetExpr::Select(s) = q.body.as_ref() else {
                panic!("select")
            };
            for (j, item) in s.projection.iter().enumerate() {
                let SelectItem::ExprWithAlias {
                    expr: Expr::Function(fun),
                    ..
                } = item
                else {
                    panic!("projection")
                };
                let p = call(fun, &env).unwrap();
                let raw: Vec<_> = args(fun)
                    .iter()
                    .map(|e| raw_value(e, &env, &values))
                    .collect();
                let converted: Vec<_> =
                    raw.iter().map(|v| Ok(convert(v, &p.declaration))).collect();
                let original = converted.clone();
                let got = p.select(&converted).unwrap().map_or(Json::Null, |n| {
                    captured_value(converted[n].as_ref().unwrap(), p.declaration.data_type)
                });
                assert_eq!(
                    got, record["result"]["sets"][0]["rows"][0][j],
                    "{} column {j}",
                    record["name"]
                );
                assert_eq!(converted, original);
            }
        }
    }
}
#[test]
fn prepared_declarations_survive_null_conversion_failure_and_recovery() {
    let f = fixture();
    for records in runs(&f) {
        let record = &records[219];
        let env = environment(record);
        let calls = scalar_calls(&parsed(record));
        let plans: Vec<_> = calls.iter().map(|c| call(c, &env).unwrap()).collect();
        for exec in record["result"]["executions"].as_array().unwrap() {
            let values: HashMap<_, _> = exec["values"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (format!("@{k}"), v.clone()))
                .collect();
            for (j, (c, p)) in calls.iter().zip(&plans).enumerate() {
                assert_eq!(p.declaration.data_type, decimal(12, 2));
                assert_eq!(p.declaration.nullable, Some(true));
                if values["@c"] == json!("x") {
                    let err = conversion_failure(
                        character(Family::Varchar, 8),
                        p.declaration.data_type,
                        "x",
                    )
                    .unwrap();
                    error_matches(&err, &exec["result"]["errors"][0]);
                    assert_eq!(
                        p.select(&[
                            Ok(Comparable::Exact {
                                coefficient: 100,
                                scale: 2
                            }),
                            Ok(Comparable::Exact {
                                coefficient: 250,
                                scale: 2
                            }),
                            Err(err.clone())
                        ]),
                        Err(Error::Sql(err))
                    );
                } else {
                    let converted: Vec<_> = args(c)
                        .iter()
                        .map(|e| Ok(convert(&raw_value(e, &env, &values), &p.declaration)))
                        .collect();
                    let got = p.select(&converted).unwrap().map_or(Json::Null, |n| {
                        captured_value(converted[n].as_ref().unwrap(), p.declaration.data_type)
                    });
                    assert_eq!(got, exec["result"]["sets"][0]["rows"][0][j]);
                }
                assert_eq!(call(c, &env).unwrap(), *p);
            }
        }
        let err = conversion_failure(character(Family::Varchar, 3), Type::Int, "abc").unwrap();
        error_matches(&err, &records[97]["result"]["errors"][0]);
    }
}
#[test]
fn exact_caps_temporal_ties_binary_guid_variant_and_late_errors() {
    let f = fixture();
    for records in runs(&f) {
        let huge = 99999999999999999999999999999999999999i128;
        for (i, keys, indices) in [
            (
                43,
                vec![
                    Comparable::Exact {
                        coefficient: huge,
                        scale: 0,
                    },
                    Comparable::Exact {
                        coefficient: 1,
                        scale: 0,
                    },
                ],
                [0, 1],
            ),
            (
                45,
                vec![
                    Comparable::Exact {
                        coefficient: 5 * 10i128.pow(36),
                        scale: 37,
                    },
                    Comparable::Exact {
                        coefficient: 10i128.pow(37),
                        scale: 37,
                    },
                ],
                [1, 0],
            ),
        ] {
            let env = environment(&records[i]);
            let calls = scalar_calls(&parsed(&records[i]));
            for (j, c) in calls.iter().enumerate() {
                let p = call(c, &env).unwrap();
                assert_eq!(
                    p.select(&keys.iter().cloned().map(Ok).collect::<Vec<_>>()),
                    Ok(Some(indices[j]))
                );
            }
        }
        assert_eq!(
            records[190]["result"]["sets"][0]["rows"][0],
            json!([huge.to_string(), "1"])
        );
        for (i, keys, indices) in [
            (103, [2, 1], [0, 1]),
            (105, [2, 1], [0, 1]),
            (107, [1230000, 1234567], [1, 0]),
            (109, [360000000000, 359999970000], [0, 1]),
            (111, [33333, 20000], [0, 1]),
            (113, [8, 9], [1, 0]),
            (115, [8, 9], [1, 0]),
            (117, [5000000, 2500000], [0, 1]),
            (119, [2, 5], [1, 0]),
            (121, [5, 2], [0, 1]),
            (125, [20240102, 19000102], [0, 1]),
            (129, [10, 20240102], [1, 0]),
        ] {
            let env = environment(&records[i]);
            let calls = scalar_calls(&parsed(&records[i]));
            for (j, c) in calls.iter().enumerate() {
                let p = call(c, &env).unwrap();
                assert_eq!(
                    p.select(&keys.map(|n| Ok(Comparable::Temporal(n)))),
                    Ok(Some(indices[j]))
                );
                let row = &records[i]["result"]["sets"][0]["rows"][0];
                assert_ne!(row[0], row[1]);
            }
        }
        let calls = scalar_calls(&parsed(&records[193]));
        for c in calls {
            let p = call(&c, &HashMap::new()).unwrap();
            assert_eq!(
                p.select(&[Ok(Comparable::Temporal(8)), Ok(Comparable::Temporal(8))]),
                Ok(Some(0))
            );
        }
        for (i, keys) in [
            (135, vec![vec![1], vec![2]]),
            (137, vec![vec![1, 0], vec![2, 0]]),
            (151, vec![vec![1], vec![1, 2]]),
        ] {
            let env = environment(&records[i]);
            let calls = scalar_calls(&parsed(&records[i]));
            for (j, c) in calls.iter().enumerate() {
                let p = call(c, &env).unwrap();
                let n = p
                    .select(
                        &keys
                            .iter()
                            .cloned()
                            .map(|b| Ok(Comparable::Binary(b)))
                            .collect::<Vec<_>>(),
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    records[i]["result"]["sets"][0]["rows"][0][j],
                    json!({"kind":"binary","value":keys[n].iter().map(|b|format!("{b:02x}")).collect::<String>()})
                );
            }
        }
        let mut a = [0u8; 16];
        a[15] = 2;
        let mut b = [0u8; 16];
        b[3] = 1;
        for i in [131, 133] {
            let env = environment(&records[i]);
            let calls = scalar_calls(&parsed(&records[i]));
            for (j, c) in calls.iter().enumerate() {
                let p = call(c, &env).unwrap();
                let n = p
                    .select(&[Ok(Comparable::Guid(a)), Ok(Comparable::Guid(b))])
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    records[i]["result"]["sets"][0]["rows"][0][j],
                    json!(
                        [
                            "00000000-0000-0000-0000-000000000002",
                            "01000000-0000-0000-0000-000000000000"
                        ][n]
                    )
                );
            }
        }
        let keys = [
            Comparable::Variant {
                base_type: Type::Int,
                value: Box::new(Comparable::Exact {
                    coefficient: 1,
                    scale: 0,
                }),
            },
            Comparable::Variant {
                base_type: character(Family::Varchar, 1),
                value: Box::new(Comparable::Character {
                    units: vec![97],
                    sort_key: vec![97],
                    collation: CI.into(),
                }),
            },
            Comparable::Variant {
                base_type: decimal(2, 1),
                value: Box::new(Comparable::Exact {
                    coefficient: 25,
                    scale: 1,
                }),
            },
        ];
        for (j, c) in scalar_calls(&parsed(&records[147])).iter().enumerate() {
            let p = call(c, &HashMap::new()).unwrap();
            let n = p.select(&keys.clone().map(Ok)).unwrap().unwrap();
            assert_eq!(
                records[147]["result"]["sets"][0]["rows"][0][j],
                [json!(1), json!("a"), json!(2.5)][n]
            );
        }
        for i in [179, 181] {
            let error = &records[i]["result"]["errors"][0];
            let e = SqlError::new(8134, 1, "Divide by zero error encountered.");
            error_matches(&e, error);
            for c in scalar_calls(&parsed(&records[i])) {
                let p = call(&c, &HashMap::new()).unwrap();
                assert_eq!(
                    p.select(&[
                        Ok(if i == 181 {
                            Comparable::Null
                        } else {
                            Comparable::Exact {
                                coefficient: 1,
                                scale: 0,
                            }
                        }),
                        Err(e.clone())
                    ]),
                    Err(Error::Sql(e.clone()))
                );
            }
        }
    }
}
#[test]
fn unknowns_and_invalid_shapes_remain_explicit_and_no_values_choose_metadata() {
    assert_eq!(
        plan(Function::Greatest, &[Argument::typed(Type::Int, None)])
            .unwrap()
            .declaration
            .nullable,
        None
    );
    let unknown = Argument {
        data_type: None,
        ..Argument::typed(Type::Int, Some(false))
    };
    assert_eq!(
        plan(Function::Least, &[unknown]),
        Err(Error::Unsupported(Unsupported::UnknownDeclaration))
    );
    let p = plan(
        Function::Greatest,
        &[Argument::typed(Type::Int, Some(true))],
    )
    .unwrap();
    assert_eq!(
        p.select(&[]),
        Err(Error::Unsupported(Unsupported::ComparableShape))
    );
    assert_eq!(
        p.select(&[Ok(Comparable::Float(1.0))]),
        Err(Error::Unsupported(Unsupported::ComparableShape))
    );
    assert_eq!(
        p.select(&[Ok(Comparable::Exact {
            coefficient: i64::MAX as i128,
            scale: 0
        })]),
        Err(Error::Unsupported(Unsupported::ComparableShape))
    );
    let p = plan(
        Function::Least,
        &vec![Argument::typed(Type::Variant, Some(true)); 2],
    )
    .unwrap();
    let zero = |scale| {
        Ok(Comparable::Variant {
            base_type: decimal(4, scale),
            value: Box::new(Comparable::Exact {
                coefficient: 0,
                scale,
            }),
        })
    };
    assert_eq!(p.select(&[zero(0), zero(2)]), Ok(Some(0)));
    let mut a = Argument::typed(character(Family::Varchar, 1), Some(false));
    a.collation = Some(Label::CoercibleDefault("unfamiliar".into()));
    assert_eq!(
        plan(Function::Greatest, &[a]).unwrap().declaration.flags(),
        None
    );
    assert!(matches!(
        plan(
            Function::Greatest,
            &[
                Argument::typed(Type::Float, Some(false)),
                Argument::typed(Type::Date, Some(false))
            ]
        ),
        Err(Error::Unsupported(Unsupported::UncapturedConversion { .. }))
    ));
    let p = plan(
        Function::Greatest,
        &[Argument::typed(Type::Float, Some(false))],
    )
    .unwrap();
    assert_eq!(
        p.select(&[Ok(Comparable::Float(f64::NAN))]),
        Err(Error::Unsupported(Unsupported::NonFiniteFloat))
    );
    let mut fixed = Argument::typed(character(Family::Char, 3), Some(false));
    fixed.collation = Some(Label::CoercibleDefault(CI.into()));
    let p = plan(Function::Greatest, &[fixed]).unwrap();
    assert_eq!(
        p.select(&[Ok(Comparable::Character {
            units: vec![97],
            sort_key: vec![97],
            collation: CI.into()
        })]),
        Err(Error::Unsupported(Unsupported::ComparableShape))
    );
    let f = fixture();
    for records in runs(&f) {
        for (i, len) in [(195, 9000), (197, 5000)] {
            let calls = scalar_calls(&parsed(&records[i]));
            let p = call(&calls[0], &HashMap::new()).unwrap();
            let c = p
                .declaration
                .collation
                .as_ref()
                .and_then(Label::name)
                .unwrap();
            let oversized = Comparable::Character {
                units: vec![120; len],
                sort_key: vec![120],
                collation: c.into(),
            };
            let valid = Comparable::Character {
                units: vec![97],
                sort_key: vec![97],
                collation: c.into(),
            };
            let Err(Error::Sql(e)) = p.select(&[Ok(oversized), Ok(valid)]) else {
                panic!("truncation")
            };
            error_matches(&e, &records[i]["result"]["errors"][0]);
        }
    }
}

#[test]
fn catalog_rows_aggregates_binary_integer_and_variant_inputs_match_captures() {
    let f = fixture();
    for records in runs(&f) {
        for i in [159, 161, 163, 165, 169, 171, 187] {
            let record = &records[i];
            let env = environment(record);
            let calls = scalar_calls(&parsed(record));
            let source = if i == 187 {
                vec![(0, 0, json!(4), json!(3), "", "")]
            } else if [159, 163, 169, 171].contains(&i) {
                vec![(1, 2, Json::Null, json!(5), "a", "a")]
            } else {
                vec![
                    (1, 2, Json::Null, json!(5), "a", "a"),
                    (3, 4, json!(4), Json::Null, "b", "B"),
                    (5, 6, json!(1), json!(3), "c", "c"),
                ]
            };
            for (row, (nn1, nn2, n1, n2, cs, bin)) in source.into_iter().enumerate() {
                let values = HashMap::from([
                    ("nn1".into(), json!(nn1)),
                    ("nn2".into(), json!(nn2)),
                    ("n1".into(), n1),
                    ("n2".into(), n2),
                    ("cs".into(), json!(cs)),
                    ("bin".into(), json!(bin)),
                ]);
                for (j, c) in calls.iter().enumerate() {
                    let p = call(c, &env).unwrap();
                    let raw: Vec<_> = args(c)
                        .iter()
                        .map(|e| {
                            if i == 187
                                && let Expr::Function(fun) = e
                            {
                                raw_value(args(fun)[0], &env, &values)
                            } else {
                                raw_value(e, &env, &values)
                            }
                        })
                        .collect();
                    let keys: Vec<_> = raw.iter().map(|v| Ok(convert(v, &p.declaration))).collect();
                    let got = p.select(&keys).unwrap().map_or(Json::Null, |n| {
                        captured_value(keys[n].as_ref().unwrap(), p.declaration.data_type)
                    });
                    assert_eq!(
                        got, record["result"]["sets"][0]["rows"][row][j],
                        "{} row {row} col {j}",
                        record["name"]
                    );
                }
            }
        }
        for i in [139, 143, 145, 216] {
            for (j, c) in scalar_calls(&parsed(&records[i])).iter().enumerate() {
                let env = environment(&records[i]);
                let p = call(c, &env).unwrap();
                let keys: Vec<_> = (1..=2)
                    .map(|n| {
                        Ok(match i {
                            143 | 145 => Comparable::Variant {
                                base_type: Type::Int,
                                value: Box::new(Comparable::Exact {
                                    coefficient: n,
                                    scale: 0,
                                }),
                            },
                            216 => Comparable::Binary(vec![n as u8]),
                            _ => Comparable::Exact {
                                coefficient: n,
                                scale: 0,
                            },
                        })
                    })
                    .collect();
                let n = p.select(&keys).unwrap().unwrap();
                let got = if i == 216 {
                    json!({"kind":"binary","value":format!("{:02x}",n+1)})
                } else {
                    json!(n + 1)
                };
                assert_eq!(got, records[i]["result"]["sets"][0]["rows"][0][j]);
            }
        }
        // Caller-supplied exact ticks and converted payloads remain separate.
        // These text observations retain offsets and legacy datetime widening
        // which JavaScript Date observations alone cannot express.
        for (i, keys, payloads) in [
            (
                192,
                vec![8, 9],
                vec![
                    "2024-01-01 10:00:00.000 +02:00",
                    "2024-01-01 09:00:00.000 +00:00",
                ],
            ),
            (
                194,
                vec![33333, 20000],
                vec!["2024-01-01 00:00:00.0033333", "2024-01-01 00:00:00.0020000"],
            ),
        ] {
            for (j, c) in scalar_calls(&parsed(&records[i])).iter().enumerate() {
                let p = call(c, &HashMap::new()).unwrap();
                let n = p
                    .select(
                        &keys
                            .iter()
                            .map(|n| Ok(Comparable::Temporal(*n)))
                            .collect::<Vec<_>>(),
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    json!(payloads[n]),
                    records[i]["result"]["sets"][0]["rows"][0][j]
                );
            }
        }
        for (j, c) in scalar_calls(&parsed(&records[193])).iter().enumerate() {
            let p = call(c, &HashMap::new()).unwrap();
            let n = p
                .select(&[Ok(Comparable::Temporal(8)), Ok(Comparable::Temporal(8))])
                .unwrap()
                .unwrap();
            let payloads = if j == 0 {
                ["2024-01-01 10:00:00 +02:00", "2024-01-01 08:00:00 +00:00"]
            } else {
                ["2024-01-01 08:00:00 +00:00", "2024-01-01 10:00:00 +02:00"]
            };
            assert_eq!(
                json!(payloads[n]),
                records[193]["result"]["sets"][0]["rows"][0][j]
            );
        }
        for (j, c) in scalar_calls(&parsed(&records[191])).iter().enumerate() {
            let p = call(c, &HashMap::new()).unwrap();
            let coefficients = if j == 0 { vec![1, 1, 1] } else { vec![2, -1] };
            let keys: Vec<_> = coefficients
                .iter()
                .map(|n| {
                    Ok(Comparable::Exact {
                        coefficient: *n,
                        scale: 0,
                    })
                })
                .collect();
            let n = p.select(&keys).unwrap().unwrap();
            assert_eq!(
                json!(coefficients[n].to_string()),
                records[191]["result"]["sets"][0]["rows"][0][j]
            );
        }
    }
}
