//! CHECK column relationships over an explicit column snapshot.
use sqlparser::ast::{Expr, visit_expressions};
use std::{collections::HashSet, ops::ControlFlow};

/// SQL Server attributes the retained single-column table CHECK to that
/// column. Multi-column/constant CHECKs belong to the table (zero); unresolved
/// references stay unknown rather than receiving a fabricated column ID.
pub fn parent_column(expression: &Expr, columns: &[(String, i32)]) -> Option<i32> {
    let mut referenced = HashSet::new();
    let unresolved = visit_expressions(expression, |expression| {
        let name = match expression {
            Expr::Identifier(name) => &name.value,
            Expr::CompoundIdentifier(names) => {
                let Some(name) = names.last() else {
                    return ControlFlow::Break(());
                };
                &name.value
            }
            _ => return ControlFlow::Continue(()),
        };
        let matches = columns
            .iter()
            .filter(|(column, _)| column.eq_ignore_ascii_case(name))
            .collect::<Vec<_>>();
        let [(_, id)] = matches.as_slice() else {
            return ControlFlow::Break(());
        };
        referenced.insert(*id);
        ControlFlow::Continue(())
    })
    .is_break();
    if unresolved {
        None
    } else if referenced.len() == 1 {
        referenced.into_iter().next()
    } else {
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::{SelectItem, SetExpr, Statement};
    fn expression(sql: &str) -> Expr {
        let Statement::Query(query) = crate::batch::parse(&format!("SELECT {sql}"))
            .unwrap()
            .remove(0)
        else {
            panic!()
        };
        let SetExpr::Select(select) = *query.body else {
            panic!()
        };
        let SelectItem::UnnamedExpr(expression) = select.projection.into_iter().next().unwrap()
        else {
            panic!()
        };
        expression
    }
    #[test]
    fn single_column_check_matches_retained_relationship() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../reference/gaps-catalog.json"
        ))
        .unwrap();
        let record = reference["runs"][0]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "checks")
            .unwrap();
        assert_eq!(
            parent_column(&expression("a>0"), &[("a".into(), 1)]),
            record["result"]["sets"][0]["rows"][0][2]
                .as_i64()
                .map(|id| id as i32)
        );
        assert_eq!(
            parent_column(&expression("missing>0"), &[("a".into(), 1)]),
            None
        );
    }
}
