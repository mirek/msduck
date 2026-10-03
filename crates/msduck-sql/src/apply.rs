//! Lower APPLY at every join-tree level while retaining join parentheses.
use sqlparser::ast::*;

mod full_join;

pub fn lower(table: &mut TableWithJoins) {
    fn nested(factor: &mut TableFactor) {
        if let TableFactor::NestedJoin {
            table_with_joins, ..
        } = factor
        {
            lower(table_with_joins);
        }
    }
    nested(&mut table.relation);
    for join in &mut table.joins {
        nested(&mut join.relation);
        let outer = match join.join_operator {
            JoinOperator::CrossApply => false,
            JoinOperator::OuterApply => true,
            _ => continue,
        };
        if let TableFactor::Derived {
            lateral, subquery, ..
        } = &mut join.relation
        {
            *lateral = true;
            // DuckDB cannot flatten a correlated FULL JOIN in the lateral body.
            full_join::rewrite(subquery);
        }
        join.join_operator = if outer {
            JoinOperator::LeftOuter(JoinConstraint::On(Expr::Value(Value::Boolean(true).into())))
        } else {
            JoinOperator::CrossJoin(JoinConstraint::None)
        };
    }
}
