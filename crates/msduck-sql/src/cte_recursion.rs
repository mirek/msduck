//! Recursive CTE structure and self-reference analysis. No execution or catalog I/O.
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// Check the anchor/recursive-member boundary before a backend can bind a
/// same-named base table. Successful validation does not enable execution.
pub fn validate(cte: &Cte) -> Result<(), SqlError> {
    let name = &cte.alias.name.value;
    if references(&cte.query, name) == 0 {
        return Ok(());
    }
    if cte.query.limit_clause.is_some() || cte.query.fetch.is_some() {
        return Err(diagnostic(461, name));
    }
    let mut members = Vec::new();
    union_members(&cte.query.body, &mut members);
    if members.len() < 2 {
        return Err(diagnostic(252, name));
    }
    let mut recursive = false;
    for (index, member) in members.into_iter().enumerate() {
        let count = references(member, name);
        if count == 0 {
            if recursive {
                return Err(diagnostic(247, name));
            }
        } else {
            if index == 0 {
                return Err(diagnostic(246, name));
            }
            if count > 1 {
                return Err(diagnostic(253, name));
            }
            // A recursive member may not hide a UNION/INTERSECT/EXCEPT
            // behind the top-level UNION ALL chain. Anchor sets are allowed.
            if matches!(member, SetExpr::SetOperation { .. }) {
                return Err(diagnostic(252, name));
            }
            validate_member(member, name)?;
            recursive = true;
        }
    }
    Ok(())
}

/// Extract the complete anchor prefix without consulting a catalog or walking
/// recursive member output types. Works for T-SQL and generated native CTEs.
/// This is inference input, not a substitute for structural validation.
pub fn anchor(cte: &Cte) -> Option<Query> {
    let mut members = Vec::new();
    union_members(&cte.query.body, &mut members);
    let first = members
        .iter()
        .position(|m| references(*m, &cte.alias.name.value) > 0)?;
    if first == 0 {
        return None;
    }
    let mut anchors = members[..first].iter();
    let body = (*anchors.next()?).clone();
    let body = anchors.fold(body, |left, right| SetExpr::SetOperation {
        op: SetOperator::Union,
        set_quantifier: SetQuantifier::All,
        left: Box::new(left),
        right: Box::new((*right).clone()),
    });
    let mut query = *cte.query.clone();
    *query.body = body;
    Some(query)
}

fn validate_member(member: &SetExpr, name: &str) -> Result<(), SqlError> {
    // Parenthesized set branches are query wrappers, not scalar subqueries.
    if let SetExpr::Query(query) = member {
        if query.limit_clause.is_some() || query.fetch.is_some() {
            return Err(diagnostic(461, name));
        }
        return validate_member(&query.body, name);
    }
    struct Check<'a> {
        name: &'a str,
        nested: usize,
    }
    fn outer_join(table: &TableWithJoins) -> bool {
        table.joins.iter().any(|join| {
            matches!(
                join.join_operator,
                JoinOperator::Left(_)
                    | JoinOperator::LeftOuter(_)
                    | JoinOperator::Right(_)
                    | JoinOperator::RightOuter(_)
                    | JoinOperator::FullOuter(_)
            )
        })
    }
    impl Visitor for Check<'_> {
        type Break = SqlError;
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<SqlError> {
            if references(query, self.name) > 0 {
                return ControlFlow::Break(diagnostic(465, self.name));
            }
            self.nested += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<SqlError> {
            self.nested -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<SqlError> {
            if self.nested > 0 {
                return ControlFlow::Continue(());
            }
            let number = if select.distinct.is_some() {
                Some(460)
            } else if select.top.is_some() {
                Some(461)
            } else if select.having.is_some()
                || !matches!(&select.group_by,GroupByExpr::Expressions(e,_) if e.is_empty())
            {
                Some(467)
            } else if select.from.iter().any(outer_join) {
                Some(462)
            } else {
                None
            };
            match number {
                Some(n) => ControlFlow::Break(diagnostic(n, self.name)),
                None => ControlFlow::Continue(()),
            }
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<SqlError> {
            if self.nested == 0 {
                if matches!(factor, TableFactor::Pivot { .. }) {
                    return ControlFlow::Break(diagnostic(4190, self.name));
                }
                if let TableFactor::Table { with_hints, .. } = factor
                    && !with_hints.is_empty()
                    && self_reference(factor, self.name)
                {
                    return ControlFlow::Break(diagnostic(4150, self.name));
                }
            }
            if self.nested == 0
                && let TableFactor::NestedJoin {
                    table_with_joins, ..
                } = factor
                && outer_join(table_with_joins)
            {
                return ControlFlow::Break(diagnostic(462, self.name));
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if self.nested == 0
                && let Expr::Function(function) = expr
                && function.over.is_none()
                && function.name.0.len() == 1
            {
                let name = function.name.0[0]
                    .as_ident()
                    .map(|id| id.value.to_ascii_uppercase())
                    .unwrap_or_default();
                if matches!(
                    name.as_str(),
                    "AVG"
                        | "COUNT"
                        | "COUNT_BIG"
                        | "MIN"
                        | "MAX"
                        | "SUM"
                        | "STDEV"
                        | "STDEVP"
                        | "VAR"
                        | "VARP"
                        | "STRING_AGG"
                        | "CHECKSUM_AGG"
                        | "APPROX_COUNT_DISTINCT"
                        | "APPROX_PERCENTILE_CONT"
                        | "APPROX_PERCENTILE_DISC"
                ) {
                    return ControlFlow::Break(diagnostic(467, self.name));
                }
            }
            ControlFlow::Continue(())
        }
    }
    match member.visit(&mut Check { name, nested: 0 }) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(()),
    }
}

fn union_members<'a>(body: &'a SetExpr, members: &mut Vec<&'a SetExpr>) {
    match body {
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier: SetQuantifier::All,
            left,
            right,
        } => {
            union_members(left, members);
            union_members(right, members);
        }
        _ => members.push(body),
    }
}

pub(crate) fn references<T: Visit>(node: &T, name: &str) -> usize {
    struct Count {
        name: String,
        hidden: Vec<bool>,
        count: usize,
    }
    impl Visitor for Count {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            let hidden = self.hidden.last().copied().unwrap_or(false)
                || query.with.as_ref().is_some_and(|with| {
                    with.cte_tables
                        .iter()
                        .any(|cte| cte.alias.name.value.to_lowercase() == self.name)
                });
            self.hidden.push(hidden);
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.hidden.pop();
            ControlFlow::Continue(())
        }
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if !self.hidden.last().copied().unwrap_or(false) && self_reference(factor, &self.name) {
                self.count += 1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut count = Count {
        name: name.to_lowercase(),
        hidden: Vec::new(),
        count: 0,
    };
    let _ = node.visit(&mut count);
    count.count
}

// Keep recursive binding and restrictions on that binding consistent. A
// schema-qualified table and a table function are not CTE self references.
fn self_reference(factor: &TableFactor, cte_name: &str) -> bool {
    matches!(factor, TableFactor::Table { name, args: None, .. }
        if name.0.len() == 1 && name.0[0].as_ident()
            .is_some_and(|id| id.value.to_lowercase() == cte_name.to_lowercase()))
}

fn diagnostic(number: i32, name: &str) -> SqlError {
    let message = match number {
        246 => format!("No anchor member was specified for recursive query \"{name}\"."),
        247 => format!(
            "An anchor member was found in the recursive part of recursive query \"{name}\"."
        ),
        252 => format!(
            "Recursive common table expression '{name}' does not contain a top-level UNION ALL operator."
        ),
        253 => format!(
            "Recursive member of a common table expression '{name}' has multiple recursive references."
        ),
        460 => format!(
            "DISTINCT operator is not allowed in the recursive part of a recursive common table expression '{name}'."
        ),
        461 => format!(
            "The TOP or OFFSET operator is not allowed in the recursive part of a recursive common table expression '{name}'."
        ),
        462 => format!(
            "Outer join is not allowed in the recursive part of a recursive common table expression '{name}'."
        ),
        465 => "Recursive references are not allowed in subqueries.".into(),
        467 => format!(
            "GROUP BY, HAVING, or aggregate functions are not allowed in the recursive part of a recursive common table expression '{name}'."
        ),
        4150 => format!(
            "Hints are not allowed on recursive common table expression (CTE) references. Consider removing hint from recursive CTE reference '{name}'."
        ),
        4190 => format!(
            "PIVOT operator is not allowed in the recursive part of a recursive common table expression '{name}'."
        ),
        _ => unreachable!(),
    };
    SqlError::new(number, 1, message)
}

pub fn error_number(message: &str) -> Option<i32> {
    if message == "Recursive references are not allowed in subqueries." {
        return Some(465);
    }
    [246, 247, 252, 253, 460, 461, 462, 467, 4150, 4190]
        .into_iter()
        .find(|number| {
            let template = diagnostic(*number, "\0").message;
            let (prefix, suffix) = template.split_once('\0').unwrap();
            message.starts_with(prefix) && message.ends_with(suffix)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cte(sql: &str) -> Cte {
        let Statement::Query(mut query) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            panic!()
        };
        query.with.take().unwrap().cte_tables.remove(0)
    }
    #[test]
    fn table_restrictions_follow_recursive_binding_and_member_scope() {
        for (sql, number) in [
            (
                "WITH r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WITH (NOLOCK) WHERE n<2) SELECT * FROM r",
                4150,
            ),
            (
                "WITH [R](n) AS (SELECT 1 UNION ALL SELECT q.n+1 FROM [r] q WITH (HOLDLOCK) WHERE q.n<2) SELECT * FROM [R]",
                4150,
            ),
            (
                "WITH r(n,k) AS (SELECT 1,1 UNION ALL SELECT [1],1 FROM r PIVOT(SUM(n) FOR k IN ([1])) p) SELECT * FROM r",
                4190,
            ),
            (
                "WITH r(n) AS (SELECT 1 UNION ALL SELECT r.n FROM r JOIN t PIVOT(SUM(v) FOR k IN ([1])) p ON r.n=p.[1]) SELECT * FROM r",
                4190,
            ),
        ] {
            let cte = cte(sql);
            let before = cte.to_string();
            let error = validate(&cte).unwrap_err();
            assert_eq!(error.number, number, "{sql}");
            assert_eq!(error_number(&error.message), Some(number));
            assert_eq!(cte.to_string(), before);
        }
        for sql in [
            "WITH r(n) AS (SELECT n FROM t WITH (NOLOCK) UNION ALL SELECT r.n+1 FROM r JOIN t WITH (HOLDLOCK) ON r.n=t.n WHERE r.n<2) SELECT * FROM r",
            "WITH r(n) AS (SELECT 1 UNION ALL SELECT r.n+1 FROM r JOIN dbo.r b WITH (NOLOCK) ON r.n=b.n WHERE r.n<2) SELECT * FROM r",
            "WITH r(n) AS (SELECT [1] FROM t PIVOT(SUM(v) FOR k IN ([1])) p UNION ALL SELECT n+1 FROM r WHERE n<2) SELECT * FROM r",
            "WITH r(n) AS (SELECT n FROM dbo.r WITH (NOLOCK)) SELECT * FROM r",
        ] {
            assert!(validate(&cte(sql)).is_ok(), "{sql}");
        }
    }

    #[test]
    fn restrictions_apply_to_recursive_members_not_anchors_or_windows() {
        for (member, number) in [
            ("SELECT DISTINCT n FROM r", 460),
            ("SELECT TOP(1) n FROM r", 461),
            ("SELECT n FROM r GROUP BY n", 467),
            ("SELECT SUM(n) FROM r", 467),
            ("SELECT n FROM r HAVING COUNT(*)>0", 467),
            ("SELECT r.n FROM r LEFT JOIN t ON r.n=t.n", 462),
            ("SELECT r.n FROM r RIGHT OUTER JOIN t ON r.n=t.n", 462),
            ("SELECT (SELECT n FROM r)", 465),
            ("SELECT n FROM (SELECT n FROM r) d", 465),
        ] {
            let cte = cte(&format!(
                "WITH r(n) AS (SELECT 1 UNION ALL {member}) SELECT * FROM r"
            ));
            let error = validate(&cte).unwrap_err();
            assert_eq!(error.number, number, "{member}");
            assert_eq!(error_number(&error.message), Some(number));
        }
        for sql in [
            "WITH r(n) AS (SELECT DISTINCT n FROM t UNION ALL SELECT n+1 FROM r WHERE n<3) SELECT * FROM r",
            "WITH r(n) AS (SELECT MAX(n) FROM t UNION ALL SELECT n+1 FROM r WHERE n<3) SELECT * FROM r",
            "WITH r(n) AS (SELECT 1 UNION ALL SELECT SUM(n) OVER() FROM r WHERE n<3) SELECT * FROM r",
        ] {
            assert!(validate(&cte(sql)).is_ok(), "{sql}");
        }
    }

    #[test]
    fn self_reference_structure_ignores_qualified_tables_and_nested_shadowing() {
        for (sql, number) in [
            ("WITH r(n) AS (SELECT n FROM r) SELECT * FROM r", 252),
            (
                "WITH r(n) AS (SELECT 1 UNION SELECT n FROM r) SELECT * FROM r",
                252,
            ),
            (
                "WITH r(n) AS (SELECT n FROM r UNION ALL SELECT n FROM r) SELECT * FROM r",
                246,
            ),
            (
                "WITH r(n) AS (SELECT 1 UNION ALL SELECT n FROM r UNION ALL SELECT 2) SELECT * FROM r",
                247,
            ),
            (
                "WITH r(n) AS (SELECT 1 UNION ALL SELECT a.n FROM r a JOIN r b ON a.n=b.n) SELECT * FROM r",
                253,
            ),
        ] {
            let cte = cte(sql);
            let before = cte.to_string();
            let error = validate(&cte).unwrap_err();
            assert_eq!(error.number, number);
            assert_eq!(error_number(&error.message), Some(number));
            assert_eq!(cte.to_string(), before);
        }
        for sql in [
            "WITH r(n) AS (SELECT n FROM dbo.r) SELECT * FROM r",
            "WITH r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<3) SELECT * FROM r",
            "WITH r(n) AS (SELECT 1 UNION SELECT 2 UNION ALL SELECT n+1 FROM r WHERE n<3) SELECT * FROM r",
            "WITH r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r UNION ALL SELECT n+2 FROM r) SELECT * FROM r",
            "WITH r(n) AS (SELECT r.n FROM (SELECT 1 AS n) r) SELECT * FROM r",
            "WITH r(n) AS (WITH r(n) AS (SELECT 1) SELECT n FROM r) SELECT * FROM r",
        ] {
            assert!(validate(&cte(sql)).is_ok(), "{sql}");
        }
    }
}
