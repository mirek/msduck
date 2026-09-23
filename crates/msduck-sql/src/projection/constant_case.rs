//! Compile-time CASE selection. Unknown runtime values never become constants.
use super::*;
use msduck_core::result::Properties;

pub(super) fn properties(
    catalog: &CatalogSnapshot,
    expr: &Expr,
    sources: &[Source],
    scope: &Scope,
    grouping: &crate::grouping_properties::Plan,
    depth: usize,
) -> Properties {
    crate::result_properties::expression_with(expr, sources, &scope.rows, &|value| {
        if let Some(properties) = grouping.properties(value, sources, &scope.rows) {
            return Some(properties);
        }
        if depth >= 64 {
            return None;
        }
        let selected = selected(value)?;
        let Some(selected) = selected else {
            return Some(Properties::expression(true));
        };
        if conditional::literal_null(selected) {
            return Some(Properties::expression(true));
        }
        // CASE resolves its declaration using every arm, even unreachable ones.
        // A conversion introduced by that declaration must not inherit stored
        // provenance or a literal's NOT NULL metadata.
        let result = member_expression(catalog, value, sources, scope)?;
        let input = member_expression(catalog, selected, sources, scope)?;
        if result != input {
            return Some(Properties::expression(true));
        }
        Some(properties(
            catalog,
            selected,
            sources,
            scope,
            grouping,
            depth + 1,
        ))
    })
}

fn selected(expr: &Expr) -> Option<Option<&Expr>> {
    let Expr::Case {
        operand,
        conditions,
        else_result,
        ..
    } = expr
    else {
        return None;
    };
    let mut budget = 4096;
    for branch in conditions {
        let condition = if let Some(operand) = operand {
            compare(
                operand,
                &BinaryOperator::Eq,
                &branch.condition,
                &mut budget,
                0,
            )?
        } else {
            truth(&branch.condition, &mut budget, 0)?
        };
        if condition == Some(true) {
            return Some(Some(&branch.result));
        }
    }
    Some(else_result.as_deref())
}

// The outer Option means whether a constant is known; the inner is SQL NULL.
fn integer(expr: &Expr, budget: &mut usize, depth: usize) -> Option<Option<i32>> {
    step(budget, depth)?;
    match expr {
        Expr::Nested(e) => integer(e, budget, depth + 1),
        Expr::Value(v) => match &v.value {
            Value::Null => Some(None),
            Value::Number(n, false) => Some(Some(n.parse().ok()?)),
            _ => None,
        },
        Expr::UnaryOp { op, expr } => {
            let value = integer(expr, budget, depth + 1)?;
            match op {
                UnaryOperator::Plus => Some(value),
                UnaryOperator::Minus => match value {
                    Some(value) => Some(Some(value.checked_neg()?)),
                    None => Some(None),
                },
                UnaryOperator::BitwiseNot => Some(value.map(|v| !v)),
                _ => None,
            }
        }
        Expr::BinaryOp { left, op, right } => {
            let left = integer(left, budget, depth + 1)?;
            let right = integer(right, budget, depth + 1)?;
            let apply = |a: i32, b: i32| match op {
                BinaryOperator::Plus => a.checked_add(b),
                BinaryOperator::Minus => a.checked_sub(b),
                BinaryOperator::Multiply => a.checked_mul(b),
                BinaryOperator::Divide => a.checked_div(b),
                BinaryOperator::Modulo => a.checked_rem(b),
                BinaryOperator::BitwiseAnd => Some(a & b),
                BinaryOperator::BitwiseOr => Some(a | b),
                BinaryOperator::BitwiseXor => Some(a ^ b),
                _ => None,
            };
            match (left, right) {
                (Some(a), Some(b)) => Some(Some(apply(a, b)?)),
                // Unsupported operators and errors must not be hidden by NULL.
                _ => None,
            }
        }
        _ => None,
    }
}

fn step(budget: &mut usize, depth: usize) -> Option<()> {
    if depth >= 128 {
        return None;
    }
    *budget = budget.checked_sub(1)?;
    Some(())
}

fn compare(
    left: &Expr,
    op: &BinaryOperator,
    right: &Expr,
    budget: &mut usize,
    depth: usize,
) -> Option<Option<bool>> {
    let left = integer(left, budget, depth + 1)?;
    let right = integer(right, budget, depth + 1)?;
    let test = |order| {
        Some(match op {
            BinaryOperator::Eq => order == std::cmp::Ordering::Equal,
            BinaryOperator::NotEq => order != std::cmp::Ordering::Equal,
            BinaryOperator::Lt => order == std::cmp::Ordering::Less,
            BinaryOperator::LtEq => order != std::cmp::Ordering::Greater,
            BinaryOperator::Gt => order == std::cmp::Ordering::Greater,
            BinaryOperator::GtEq => order != std::cmp::Ordering::Less,
            _ => return None,
        })
    };
    test(std::cmp::Ordering::Equal)?;
    match (left, right) {
        (Some(a), Some(b)) => Some(Some(test(a.cmp(&b))?)),
        _ => Some(None),
    }
}

fn truth(expr: &Expr, budget: &mut usize, depth: usize) -> Option<Option<bool>> {
    step(budget, depth)?;
    match expr {
        Expr::Nested(e) => truth(e, budget, depth + 1),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => Some(truth(expr, budget, depth + 1)?.map(|v| !v)),
        Expr::BinaryOp {
            left,
            op: op @ (BinaryOperator::And | BinaryOperator::Or),
            right,
        } => {
            let left = truth(left, budget, depth + 1)?;
            let right = truth(right, budget, depth + 1)?;
            Some(match (op, left, right) {
                (BinaryOperator::And, Some(false), _) | (BinaryOperator::And, _, Some(false)) => {
                    Some(false)
                }
                (BinaryOperator::And, Some(true), Some(true)) => Some(true),
                (BinaryOperator::Or, Some(true), _) | (BinaryOperator::Or, _, Some(true)) => {
                    Some(true)
                }
                (BinaryOperator::Or, Some(false), Some(false)) => Some(false),
                _ => None,
            })
        }
        Expr::BinaryOp { left, op, right } => compare(left, op, right, budget, depth + 1),
        Expr::IsNull(e) => Some(Some(integer(e, budget, depth + 1)?.is_none())),
        Expr::IsNotNull(e) => Some(Some(integer(e, budget, depth + 1)?.is_some())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use msduck_core::result::Origin;

    #[test]
    fn case_properties_match_live_reference_without_using_runtime_values() {
        let mut catalog = CatalogSnapshot::default();
        for (name, id, width, precision) in [("int", 56, 4, 10), ("bigint", 127, 8, 19)] {
            catalog.types.insert(
                name.into(),
                Info {
                    system_type_id: Some(id),
                    user_type_id: Some(i32::from(id)),
                    max_length: Some(width),
                    precision: Some(precision),
                    scale: Some(0),
                    collation_name: None,
                },
            );
        }
        catalog.tables.insert(
            "case_props".into(),
            [("id", false), ("v", true)]
                .into_iter()
                .map(|(name, nullable)| Field {
                    name: name.into(),
                    info: Some(catalog.types["int"].clone()),
                    collation: None,
                    json_fragment: false,
                    properties: Properties {
                        nullable: Some(nullable),
                        origin: Origin::Stored,
                    },
                })
                .collect(),
        );
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/case-constant-properties.json"
        ))
        .unwrap();
        for entry in fixture["results"].as_array().unwrap() {
            let statements = crate::batch::parse(entry["query"].as_str().unwrap()).unwrap();
            let Statement::Query(query) = statements.last().unwrap() else {
                panic!()
            };
            let before = query.clone();
            let fields = query_fields(&catalog, query, &Scope::default()).unwrap();
            let flags = entry["reference"]["sets"][0]["columns"][0]["flags"]
                .as_u64()
                .unwrap();
            assert_eq!(
                fields[0].properties,
                Properties {
                    nullable: Some(flags & 1 != 0),
                    origin: if flags & 32 != 0 {
                        Origin::Expression
                    } else {
                        Origin::Stored
                    },
                },
                "{}",
                entry["name"]
            );
            assert_eq!(*query, before);
        }
    }

    #[test]
    fn bounded_constant_selection_keeps_errors_and_ambient_values_unknown() {
        for predicate in [
            "1/0=0",
            "2147483647+1=0",
            "@p=1",
            "@@ROWCOUNT=1",
            "ABS(1)=1",
            "RAND()>0",
        ] {
            let Statement::Query(query) = crate::batch::parse(&format!(
                "SELECT CASE WHEN {predicate} THEN 7 ELSE NULL END"
            ))
            .unwrap()
            .remove(0) else {
                panic!()
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                panic!()
            };
            let SelectItem::UnnamedExpr(expr) = &select.projection[0] else {
                panic!()
            };
            assert!(selected(expr).is_none(), "{predicate}");
        }
        let mut expr = Expr::Value(Value::Number("1".into(), false).into());
        for _ in 0..140 {
            expr = Expr::Nested(Box::new(expr));
        }
        assert_eq!(integer(&expr, &mut 4096, 0), None);
        assert_eq!(
            integer(
                &Expr::Value(Value::Number("1".into(), false).into()),
                &mut 0,
                0
            ),
            None
        );
    }
}
