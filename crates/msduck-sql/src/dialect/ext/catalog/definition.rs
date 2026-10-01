//! SQL Server catalog text, independent of backend SQL and evaluation.
use sqlparser::ast::*;

pub fn expression_definition(expr: &Expr) -> Option<String> {
    let mut budget = Budget {
        nodes: 4096,
        bytes: 1_048_576,
    };
    Some(format!("({})", render(expr, false, 0, &mut budget)?))
}

struct Budget {
    nodes: usize,
    bytes: usize,
}

fn unnest(mut expr: &Expr) -> &Expr {
    loop {
        match expr {
            Expr::Nested(inner) => expr = inner,
            _ => match crate::variant_cast::source(expr) {
                Some(source) => expr = source,
                None => return expr,
            },
        }
    }
}

fn number(source: &str) -> Option<String> {
    if source.len() > 1024 {
        return None;
    }
    if source.contains(['e', 'E']) {
        let value = source.parse::<f64>().ok()?;
        if !value.is_finite() {
            return None;
        }
        let scientific = format!("{value:.16e}");
        let (mantissa, exponent) = scientific.split_once('e')?;
        let exponent = exponent.parse::<i32>().ok()?;
        return Some(format!("{mantissa}e{exponent:+04}"));
    }
    if let Some((integer, fraction)) = source.split_once('.') {
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let integer = if integer.is_empty() { "0" } else { integer };
        if !integer.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return Some(format!("{}.{fraction}", integer.parse::<i128>().ok()?));
    }
    if source.is_empty() || !source.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(source.parse::<i128>().ok()?.to_string())
}

fn type_name(kind: &DataType) -> Option<&'static str> {
    match kind {
        DataType::Int(None) => Some("[int]"),
        DataType::BigInt(None) => Some("[bigint]"),
        _ => None,
    }
}

fn render(expr: &Expr, group: bool, depth: usize, budget: &mut Budget) -> Option<String> {
    if depth >= 128 || budget.nodes == 0 {
        return None;
    }
    budget.nodes -= 1;
    let expr = unnest(expr);
    let mut child = |expr: &Expr, group| render(expr, group, depth + 1, budget);
    let text = match expr {
        Expr::Value(value) => match &value.value {
            Value::Number(value, false) => format!("({})", number(value)?),
            Value::SingleQuotedString(value) if value.len() <= 1_048_576 => {
                format!("'{}'", value.replace('\'', "''"))
            }
            Value::NationalStringLiteral(value) if value.len() <= 1_048_576 => {
                format!("N'{}'", value.replace('\'', "''"))
            }
            Value::HexStringLiteral(value) if value.len() <= 1_048_576 => format!("0x{value}"),
            Value::Null => "NULL".into(),
            _ => return None,
        },
        Expr::Identifier(ident) => format!("[{}]", ident.value.replace(']', "]]")),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => {
            let Expr::Value(value) = unnest(expr) else {
                return None;
            };
            let Value::Number(value, false) = &value.value else {
                return None;
            };
            format!("(-{})", number(value)?)
        }
        Expr::BinaryOp { left, op, right } => {
            let operator = match op {
                BinaryOperator::Plus => "+",
                BinaryOperator::Minus => "-",
                BinaryOperator::Multiply => "*",
                BinaryOperator::Divide => "/",
                BinaryOperator::Gt => ">",
                _ => return None,
            };
            let sql = format!("{}{operator}{}", child(left, true)?, child(right, true)?);
            if group { format!("({sql})") } else { sql }
        }
        Expr::Function(function) => {
            let name = function.name.to_string().to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "getdate" | "newid" | "coalesce" | "isnull" | "lower"
            ) || function.over.is_some()
                || function.filter.is_some()
                || function.null_treatment.is_some()
                || !function.within_group.is_empty()
                || !matches!(function.parameters, FunctionArguments::None)
            {
                return None;
            }
            let FunctionArguments::List(arguments) = &function.args else {
                return None;
            };
            if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
                return None;
            }
            let values = arguments
                .args
                .iter()
                .map(|argument| match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => child(expr, true),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            format!("{name}({})", values.join(","))
        }
        Expr::Cast {
            kind: CastKind::Cast,
            expr,
            data_type,
            format: None,
        } => format!("CONVERT({},{})", type_name(data_type)?, child(expr, true)?),
        Expr::Convert {
            is_try: false,
            expr,
            data_type: Some(kind),
            charset: None,
            styles,
            ..
        } if styles.is_empty() => format!("CONVERT({},{})", type_name(kind)?, child(expr, true)?),
        Expr::Case {
            operand: None,
            conditions,
            else_result,
            ..
        } => {
            let mut sql = String::from("case");
            for condition in conditions {
                sql.push_str(&format!(
                    " when {} then {}",
                    child(&condition.condition, false)?,
                    child(&condition.result, false)?
                ));
            }
            if let Some(value) = else_result {
                sql.push_str(&format!(" else {}", child(value, false)?));
            }
            sql.push_str(" end");
            sql
        }
        _ => return None,
    };
    budget.bytes = budget.bytes.checked_sub(text.len())?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_conversion_is_unknown_instead_of_changing_failure_semantics() {
        let Statement::CreateTable(table) =
            crate::batch::parse("CREATE TABLE t(a INT DEFAULT(TRY_CONVERT(INT,'x')))")
                .unwrap()
                .remove(0)
        else {
            panic!()
        };
        let ColumnOption::Default(expr) = &table.columns[0].options[0].option else {
            panic!()
        };
        assert_eq!(expression_definition(expr), None);
    }

    #[test]
    fn defaults_and_computed_expressions_match_complete_retained_profile() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../reference/gaps-catalog.json"
        ))
        .unwrap();
        let records = fixture["definitionProfile"]["runs"][0].as_array().unwrap();
        for (setup, rows) in [
            ("setup default definitions", "default definitions"),
            ("setup computed definitions", "computed definitions"),
        ] {
            let setup = records.iter().find(|r| r["name"] == setup).unwrap()["sql"]
                .as_str()
                .unwrap();
            let Statement::CreateTable(table) = crate::batch::parse(setup).unwrap().remove(0)
            else {
                panic!()
            };
            let rows =
                &records.iter().find(|r| r["name"] == rows).unwrap()["result"]["sets"][0]["rows"];
            for row in rows.as_array().unwrap() {
                let (column, expected) = if row.as_array().unwrap().len() == 4 {
                    (
                        row[1].as_u64().unwrap() as usize - 1,
                        row[2].as_str().unwrap(),
                    )
                } else {
                    (
                        row[1].as_u64().unwrap() as usize - 1,
                        row[8].as_str().unwrap(),
                    )
                };
                let definition = &table.columns[column];
                let expr = definition
                    .options
                    .iter()
                    .find_map(|o| match &o.option {
                        ColumnOption::Default(expr) => Some(expr),
                        _ => None,
                    })
                    .or_else(|| {
                        crate::dialect::computed_column::computed(definition).map(|(expr, _)| expr)
                    })
                    .unwrap();
                assert_eq!(
                    expression_definition(expr).as_deref(),
                    Some(expected),
                    "{}",
                    definition.name
                );
            }
        }
    }
}
