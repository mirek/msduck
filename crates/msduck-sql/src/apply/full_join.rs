//! Rewrite correlated FULL OUTER JOINs inside APPLY bodies.
//!
//! DuckDB cannot flatten a FULL OUTER JOIN whose operands or condition
//! reference the enclosing query, which is the usual shape of an inline
//! function joining `OPENJSON(@lhs)` and `OPENJSON(@rhs)` under APPLY. Inside
//! an APPLY body (including nested derived tables and subqueries) such a join
//!
//! ```sql
//! FROM A FULL JOIN B ON c [rest] WHERE w
//! ```
//!
//! becomes two row "sides" that only need LEFT joins and EXISTS tests:
//!
//! ```sql
//! FROM (SELECT 1 WHERE EXISTS (SELECT 1 FROM A)
//!       UNION ALL SELECT 2 WHERE EXISTS (SELECT 1 FROM B)) AS s(side)
//! LEFT JOIN A ON s.side = 1
//! LEFT JOIN LATERAL (SELECT * FROM B WHERE (s.side = 1 AND (c)) OR s.side = 2) AS b ON TRUE
//! [rest]
//! WHERE (s.side = 1 OR NOT EXISTS (SELECT 1 FROM A WHERE c)) AND (w)
//! ```
//!
//! Side 1 is `A LEFT JOIN B ON c`; side 2 is every row of B that no row of A
//! matches. An aliased B reads the condition in a lateral filter because
//! DuckDB rejects outer-join conditions that reference the enclosing query;
//! an unaliased B joins `ON (s.side = 1 AND (c)) OR s.side = 2` directly.
//! The side rows exist only when their operand has rows, so an empty
//! operand never produces a NULL-extended phantom row. Both operands keep
//! their own aliases, so qualified, unqualified and `alias.*` references in
//! the rest of the SELECT bind as before; the side column is excluded from an
//! unqualified `*`. Duplicates and NULL comparison results follow the
//! original condition, because `c` is evaluated unchanged in both places.
//!
//! A, B and `c` are evaluated more than once, so joins with volatile
//! functions are left unchanged, as are uncorrelated joins (DuckDB runs them
//! natively), joins with USING or NATURAL, and FROM items where a RIGHT or
//! FULL join follows (their NULL-extended rows would fail the side filter).
use sqlparser::ast::*;
use sqlparser::ast::{Visit, VisitMut, Visitor, VisitorMut};
use std::collections::HashSet;
use std::ops::ControlFlow;

const SIDES: &str = "__msduck_full_join";
const SIDE: &str = "__msduck_full_join_side";
const RIGHT: &str = "__msduck_full_join_right";
const VOLATILE: [&str; 4] = ["newid", "newsequentialid", "rand", "crypt_gen_random"];

/// Rewrites every eligible SELECT inside an APPLY body. Idempotent: rewritten
/// SELECTs no longer contain the FULL join.
pub fn rewrite(query: &mut Query) {
    struct Rewrite;
    impl VisitorMut for Rewrite {
        type Break = ();
        fn post_visit_select(&mut self, select: &mut Select) -> ControlFlow<()> {
            rewrite_select(select);
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(query, &mut Rewrite);
}

fn rewrite_select(select: &mut Select) {
    let mut taken = relation_names(select);
    let base_names = !long_names(select);
    let mut filters = Vec::new();
    let mut sides = Vec::new();
    for item in &mut select.from {
        let Some(index) = item
            .joins
            .iter()
            .position(|join| matches!(join.join_operator, JoinOperator::FullOuter(_)))
        else {
            continue;
        };
        let JoinOperator::FullOuter(JoinConstraint::On(condition)) =
            &item.joins[index].join_operator
        else {
            continue;
        };
        let rest_ok = item.joins[index + 1..].iter().all(|join| {
            !matches!(
                join.join_operator,
                JoinOperator::Right(_)
                    | JoinOperator::RightOuter(_)
                    | JoinOperator::FullOuter(_)
                    | JoinOperator::RightSemi(_)
                    | JoinOperator::RightAnti(_)
            )
        });
        let left = TableWithJoins {
            relation: item.relation.clone(),
            joins: item.joins[..index].to_vec(),
        };
        let right = &item.joins[index].relation;
        if !rest_ok || volatile(&left, right, condition) || !correlated(&left, right, condition) {
            continue;
        }
        let condition = condition.clone();
        let right = right.clone();
        let alias = fresh(&mut taken, SIDES);
        // The derived alias a lateral filter of B needs when B has none: an
        // unaliased table keeps its base name (unless the SELECT uses
        // three-part names, which a derived alias cannot match); other
        // unaliased factors are only referenced unqualified, so any fresh
        // name works. An unaliased parenthesized join has no usable alias.
        let fallback = match &right {
            TableFactor::Table {
                alias: None, name, ..
            } if base_names => name.0.last().and_then(|part| part.as_ident()).cloned(),
            TableFactor::Table { .. } | TableFactor::NestedJoin { .. } => None,
            factor if self::alias(factor).is_none() => Some(Ident::new(fresh(&mut taken, RIGHT))),
            _ => None,
        };
        let side = |n: i64| Expr::BinaryOp {
            left: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new(&alias),
                Ident::new(SIDE),
            ])),
            op: BinaryOperator::Eq,
            right: Box::new(number(n)),
        };
        let right_only = TableWithJoins {
            relation: right.clone(),
            joins: vec![],
        };
        let relation = TableFactor::Derived {
            lateral: false,
            subquery: Box::new(query(SetExpr::SetOperation {
                op: SetOperator::Union,
                set_quantifier: SetQuantifier::All,
                left: Box::new(SetExpr::Select(Box::new(side_row(
                    1,
                    exists(left.clone(), None, false),
                )))),
                right: Box::new(SetExpr::Select(Box::new(side_row(
                    2,
                    exists(right_only, None, false),
                )))),
            })),
            alias: Some(TableAlias {
                explicit: true,
                name: Ident::new(&alias),
                columns: vec![TableAliasColumnDef::from_name(SIDE)],
                at: None,
            }),
            sample: None,
        };
        let left_factor = if left.joins.is_empty() {
            left.relation.clone()
        } else {
            TableFactor::NestedJoin {
                table_with_joins: Box::new(left.clone()),
                alias: None,
            }
        };
        let mut joins = vec![
            Join {
                relation: left_factor,
                global: false,
                join_operator: JoinOperator::LeftOuter(JoinConstraint::On(side(1))),
            },
            right_join(right, &side, &condition, fallback),
        ];
        joins.extend(item.joins.drain(index + 1..));
        item.relation = relation;
        item.joins = joins;
        filters.push(Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(side(1)),
            op: BinaryOperator::Or,
            right: Box::new(exists(left, Some(condition), true)),
        })));
        sides.push(alias);
    }
    if sides.is_empty() {
        return;
    }
    if let Some(selection) = select.selection.take() {
        filters.push(Expr::Nested(Box::new(selection)));
    }
    select.selection = filters.into_iter().reduce(|left, right| Expr::BinaryOp {
        left: Box::new(left),
        op: BinaryOperator::And,
        right: Box::new(right),
    });
    // Hide the side columns from an unqualified `*`; `alias.*` never sees
    // them. The qualified name cannot exclude a user column of that name.
    for item in &mut select.projection {
        if let SelectItem::Wildcard(options) = item {
            let mut names = match options.opt_exclude.take() {
                Some(ExcludeSelectItem::Single(name)) => vec![name],
                Some(ExcludeSelectItem::Multiple(names)) => names,
                None => vec![],
            };
            for alias in &sides {
                let side = ObjectName::from(vec![Ident::new(alias), Ident::new(SIDE)]);
                if !names.contains(&side) {
                    names.push(side);
                }
            }
            options.opt_exclude = Some(ExcludeSelectItem::Multiple(names));
        }
    }
}

/// B joins on side 1 by the original condition and on side 2 to every row.
/// DuckDB rejects outer-join conditions that reference the enclosing query,
/// and an unqualified name in the condition may do so, so an aliased B reads
/// the condition in a lateral filter instead (`LEFT JOIN LATERAL (SELECT *
/// FROM B WHERE ...) AS b ON TRUE`); the alias keeps `b.column` and `b.*`.
fn right_join(
    right: TableFactor,
    side: &dyn Fn(i64) -> Expr,
    condition: &Expr,
    fallback: Option<Ident>,
) -> Join {
    let matched = Expr::BinaryOp {
        left: Box::new(Expr::Nested(Box::new(Expr::BinaryOp {
            left: Box::new(side(1)),
            op: BinaryOperator::And,
            right: Box::new(Expr::Nested(Box::new(condition.clone()))),
        }))),
        op: BinaryOperator::Or,
        right: Box::new(side(2)),
    };
    let name = alias(&right).map(|alias| alias.name.clone()).or(fallback);
    let Some(name) = name else {
        return Join {
            relation: right,
            global: false,
            join_operator: JoinOperator::LeftOuter(JoinConstraint::On(matched)),
        };
    };
    let mut filtered = select(
        number(1),
        vec![TableWithJoins {
            relation: right,
            joins: vec![],
        }],
        Some(matched),
    );
    filtered.projection = vec![SelectItem::Wildcard(WildcardAdditionalOptions::default())];
    Join {
        relation: TableFactor::Derived {
            lateral: true,
            subquery: Box::new(query(SetExpr::Select(Box::new(filtered)))),
            alias: Some(TableAlias {
                explicit: true,
                name,
                columns: vec![],
                at: None,
            }),
            sample: None,
        },
        global: false,
        join_operator: JoinOperator::LeftOuter(JoinConstraint::On(Expr::Value(
            Value::Boolean(true).into(),
        ))),
    }
}

fn alias(factor: &TableFactor) -> Option<&TableAlias> {
    match factor {
        TableFactor::Table { alias, .. }
        | TableFactor::Derived { alias, .. }
        | TableFactor::OpenJsonTable { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::TableFunction { alias, .. }
        | TableFactor::UNNEST { alias, .. }
        | TableFactor::NestedJoin { alias, .. } => alias.as_ref(),
        _ => None,
    }
}

/// Whether the SELECT names a column or wildcard with three or more parts
/// (`schema.table.column`).
fn long_names(select: &Select) -> bool {
    struct Long(bool);
    impl Visitor for Long {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if matches!(expr, Expr::CompoundIdentifier(parts) if parts.len() >= 3) {
                self.0 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut long = Long(false);
    let _ = select.visit(&mut long);
    long.0
        || select.projection.iter().any(|item| {
            matches!(item, SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) if name.0.len() >= 2)
        })
}

/// Lowercase aliases and table names of every relation in the SELECT, and
/// every qualifier it uses, so the side alias shadows neither a relation of
/// this SELECT nor a correlated outer alias.
fn relation_names(select: &Select) -> HashSet<String> {
    struct Names(HashSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if let Some(alias) = alias(factor) {
                self.0.insert(alias.name.value.to_lowercase());
            }
            if let TableFactor::Table { name, .. } = factor
                && let Some(last) = name.0.last().and_then(|part| part.as_ident())
            {
                self.0.insert(last.value.to_lowercase());
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::CompoundIdentifier(parts) = expr {
                for part in &parts[..parts.len().saturating_sub(1)] {
                    self.0.insert(part.value.to_lowercase());
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut names = Names(HashSet::new());
    let _ = select.visit(&mut names);
    // `alias.*` is not an expression, but it references `alias` too.
    for item in &select.projection {
        if let SelectItem::QualifiedWildcard(SelectItemQualifiedWildcardKind::ObjectName(name), _) =
            item
        {
            for part in &name.0 {
                if let Some(ident) = part.as_ident() {
                    names.0.insert(ident.value.to_lowercase());
                }
            }
        }
    }
    names.0
}

/// A name no relation or qualifier of the SELECT uses, reserved for this one.
fn fresh(taken: &mut HashSet<String>, base: &str) -> String {
    let name = (0..)
        .map(|n| {
            if n == 0 {
                base.to_string()
            } else {
                format!("{base}_{n}")
            }
        })
        .find(|name| !taken.contains(&name.to_lowercase()))
        .expect("an unused alias");
    taken.insert(name.to_lowercase());
    name
}

fn number(n: i64) -> Expr {
    Expr::Value(Value::Number(n.to_string(), false).into())
}

fn query(body: SetExpr) -> Query {
    Query {
        with: None,
        body: Box::new(body),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: vec![],
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: vec![],
    }
}

fn select(projection: Expr, from: Vec<TableWithJoins>, selection: Option<Expr>) -> Select {
    let mut select = match sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::GenericDialect {},
        "SELECT 1",
    )
    .ok()
    .and_then(|mut statements| statements.pop())
    {
        Some(Statement::Query(query)) => match *query.body {
            SetExpr::Select(select) => *select,
            _ => unreachable!("SELECT 1 parses as a SELECT"),
        },
        _ => unreachable!("SELECT 1 parses as a query"),
    };
    select.projection = vec![SelectItem::UnnamedExpr(projection)];
    select.from = from;
    select.selection = selection;
    select
}

fn side_row(n: i64, condition: Expr) -> Select {
    let mut row = select(number(n), vec![], Some(condition));
    row.projection = vec![SelectItem::ExprWithAlias {
        expr: number(n),
        alias: Ident::new(SIDE),
    }];
    row
}

fn exists(from: TableWithJoins, selection: Option<Expr>, negated: bool) -> Expr {
    Expr::Exists {
        subquery: Box::new(query(SetExpr::Select(Box::new(select(
            number(1),
            vec![from],
            selection,
        ))))),
        negated,
    }
}

fn volatile(left: &TableWithJoins, right: &TableFactor, condition: &Expr) -> bool {
    struct Find(bool);
    impl Visitor for Find {
        type Break = ();
        // A sample drawn more than once can disagree with itself.
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            if matches!(
                factor,
                TableFactor::Table {
                    sample: Some(_),
                    ..
                } | TableFactor::Derived {
                    sample: Some(_),
                    ..
                }
            ) {
                self.0 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr
                && let Some(name) = function.name.0.last().and_then(|part| part.as_ident())
                && VOLATILE.contains(&name.value.to_ascii_lowercase().as_str())
            {
                self.0 = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut find = Find(false);
    let _ = left.visit(&mut find);
    let _ = right.visit(&mut find);
    let _ = condition.visit(&mut find);
    find.0
}

/// Whether the join references a name its own operands do not define, or a
/// table-valued function argument names an unqualified column (siblings are
/// not visible to a function's arguments, so such a column is an outer
/// reference). Qualifiers resolve through the lexical scopes of the join and
/// of each nested query, so a name defined only inside an operand's subquery
/// does not hide an outer reference in the condition.
fn correlated(left: &TableWithJoins, right: &TableFactor, condition: &Expr) -> bool {
    struct Scopes {
        scopes: Vec<HashSet<String>>,
        outer: bool,
    }
    fn unqualified_columns(args: &impl Visit) -> bool {
        struct Columns(bool);
        impl Visitor for Columns {
            type Break = ();
            fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
                // A subquery argument resolves its own names.
                ControlFlow::Break(())
            }
            fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
                if matches!(expr, Expr::Identifier(ident) if !ident.value.starts_with('@')) {
                    self.0 = true;
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            }
        }
        let mut columns = Columns(false);
        let _ = args.visit(&mut columns);
        columns.0
    }
    impl Visitor for Scopes {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
            self.outer |= match factor {
                TableFactor::Table { args, .. } => args
                    .as_ref()
                    .is_some_and(|args| unqualified_columns(&args.args)),
                TableFactor::OpenJsonTable { json_expr, .. } => unqualified_columns(json_expr),
                TableFactor::Function { args, .. } => unqualified_columns(args),
                TableFactor::TableFunction { expr, .. } => unqualified_columns(expr),
                TableFactor::UNNEST { array_exprs, .. } => unqualified_columns(array_exprs),
                TableFactor::Derived { .. } | TableFactor::NestedJoin { .. } => false,
                // Unknown factor kinds count as outer references.
                _ => true,
            };
            ControlFlow::Continue(())
        }
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            let mut names = HashSet::new();
            if let Some(with) = &query.with {
                for cte in &with.cte_tables {
                    names.insert(cte.alias.name.value.to_lowercase());
                }
            }
            body_names(&query.body, &mut names);
            self.scopes.push(names);
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.scopes.pop();
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if let Expr::CompoundIdentifier(parts) = expr
                && parts.len() >= 2
            {
                let qualifier = parts[parts.len() - 2].value.to_lowercase();
                if !self.scopes.iter().any(|scope| scope.contains(&qualifier)) {
                    self.outer = true;
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut names = HashSet::new();
    join_names(left, &mut names);
    factor_names(right, &mut names);
    let mut scopes = Scopes {
        scopes: vec![names],
        outer: false,
    };
    let _ = left.visit(&mut scopes);
    let _ = right.visit(&mut scopes);
    let _ = condition.visit(&mut scopes);
    scopes.outer
}

/// Names a query's own FROM clauses define, without nested queries.
fn body_names(body: &SetExpr, names: &mut HashSet<String>) {
    match body {
        SetExpr::Select(select) => {
            for table in &select.from {
                join_names(table, names);
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            body_names(left, names);
            body_names(right, names);
        }
        _ => {}
    }
}

fn join_names(table: &TableWithJoins, names: &mut HashSet<String>) {
    factor_names(&table.relation, names);
    for join in &table.joins {
        factor_names(&join.relation, names);
    }
}

/// The alias of a relation, or the base name of an unaliased table (an alias
/// hides the base name: `t.c` then means an outer `t`).
fn factor_names(factor: &TableFactor, names: &mut HashSet<String>) {
    if let Some(alias) = alias(factor) {
        names.insert(alias.name.value.to_lowercase());
    } else if let TableFactor::Table { name, .. } = factor
        && let Some(last) = name.0.last().and_then(|part| part.as_ident())
    {
        names.insert(last.value.to_lowercase());
    }
    // An aliased parenthesized join hides the names inside it.
    if let TableFactor::NestedJoin {
        table_with_joins,
        alias: None,
    } = factor
    {
        join_names(table_with_joins, names);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;

    fn rewritten(sql: &str) -> String {
        let mut statement = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0);
        let Statement::Query(query) = &mut statement else {
            panic!("not a query")
        };
        rewrite(query);
        statement.to_string()
    }

    #[test]
    fn correlated_openjson_full_join_becomes_left_joins() {
        let sql = rewritten(
            "SELECT COALESCE(l.[key], r.[key]) AS k FROM OPENJSON(p.lhs) l FULL OUTER JOIN OPENJSON(p.rhs) r ON l.[key] = r.[key] WHERE r.[value] IS NULL",
        );
        assert!(!sql.contains("FULL"), "{sql}");
        assert!(sql.contains("UNION ALL"), "{sql}");
        assert!(
            sql.contains(&format!(
                "WHERE ({SIDES}.{SIDE} = 1 OR NOT EXISTS (SELECT 1 FROM OPENJSON(p.lhs) l WHERE l.[key] = r.[key])) AND (r.[value] IS NULL)"
            )),
            "{sql}"
        );
    }

    #[test]
    fn uncorrelated_and_volatile_joins_are_unchanged() {
        for sql in [
            "SELECT a.k, b.k FROM (VALUES (1)) a(k) FULL JOIN (VALUES (2)) b(k) ON a.k = b.k",
            "SELECT 1 FROM t a FULL JOIN u b ON a.k = b.k",
            "SELECT 1 FROM OPENJSON(p.lhs) l FULL JOIN OPENJSON(p.rhs) r ON l.[key] = r.[key] AND RAND() > 0.5",
            "SELECT 1 FROM OPENJSON(p.lhs) l FULL JOIN OPENJSON(p.rhs) r ON l.[key] = r.[key] RIGHT JOIN t ON 1 = 1",
            "SELECT 1 FROM OPENJSON(p.lhs) l FULL JOIN OPENJSON(p.rhs) r USING ([key])",
        ] {
            let before = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
                .to_string();
            assert_eq!(rewritten(sql), before, "{sql}");
        }
    }

    #[test]
    fn correlation_in_condition_or_unqualified_function_argument_counts() {
        for sql in [
            "SELECT 1 FROM (VALUES (1)) a(k) FULL JOIN (VALUES (2)) b(k) ON a.k = b.k AND p.id = 1",
            "SELECT 1 FROM OPENJSON(lhs) l FULL JOIN OPENJSON(N'[]') r ON l.[key] = r.[key]",
        ] {
            assert!(!rewritten(sql).contains("FULL"), "{sql}");
        }
    }

    #[test]
    fn side_alias_avoids_existing_relation_names_and_aliases_hide_base_names() {
        let sql = rewritten(
            "SELECT 1 FROM OPENJSON(p.lhs) __msduck_full_join FULL JOIN OPENJSON(p.rhs) r ON __msduck_full_join.[key] = r.[key]",
        );
        assert!(sql.contains(&format!("AS {SIDES}_1 ({SIDE})")), "{sql}");
        assert!(sql.contains(&format!("{SIDES}_1.{SIDE} = 1")), "{sql}");
        // An outer alias the body references is not shadowed either.
        let sql = rewritten(
            "SELECT 1 FROM OPENJSON(__msduck_full_join.lhs) l FULL JOIN OPENJSON(__msduck_full_join.rhs) r ON l.[key] = r.[key]",
        );
        assert!(sql.contains(&format!("AS {SIDES}_1 ({SIDE})")), "{sql}");
        let sql = rewritten(
            "SELECT __msduck_full_join.* FROM OPENJSON(lhs) l FULL JOIN OPENJSON(rhs) r ON l.[key] = r.[key]",
        );
        assert!(sql.contains(&format!("AS {SIDES}_1 ({SIDE})")), "{sql}");
        // `t.id` names the outer row, because `dbo.t AS a` hides `t`.
        let sql =
            rewritten("SELECT 1 FROM dbo.t AS a FULL JOIN dbo.u AS b ON a.k = b.k AND t.id = 1");
        assert!(!sql.contains("FULL"), "{sql}");
    }

    #[test]
    fn nested_join_operands_and_inner_scopes() {
        // An aliased parenthesized join filters the condition laterally.
        let sql = rewritten(
            "SELECT 1 FROM OPENJSON(p.lhs) l FULL JOIN (OPENJSON(p.rhs) a JOIN OPENJSON(p.rhs) b ON a.[key] = b.[key]) AS r ON l.[key] = a.[key]",
        );
        assert!(sql.contains("LEFT OUTER JOIN LATERAL (SELECT * FROM (OPENJSON(p.rhs) a JOIN OPENJSON(p.rhs) b ON a.[key] = b.[key]) AS r WHERE"), "{sql}");
        // `p` inside the left operand's subquery does not define the outer `p`.
        let sql = rewritten(
            "SELECT 1 FROM (SELECT p.k FROM t AS p) AS l FULL JOIN u AS r ON l.k = r.k AND p.id = 1",
        );
        assert!(!sql.contains("FULL"), "{sql}");
        // Names a subquery defines for itself are not outer references.
        let sql = "SELECT 1 FROM (SELECT p.k FROM t AS p) AS l FULL JOIN u AS r ON l.k = r.k AND EXISTS (SELECT 1 FROM v AS q WHERE q.k = r.k)";
        assert!(rewritten(sql).contains("FULL"), "{sql}");
    }

    #[test]
    fn unaliased_tables_filter_laterally_and_nested_aliases_hide_names() {
        let sql = rewritten("SELECT 1 FROM a FULL JOIN b ON a.k = b.k AND i.id = 1");
        assert!(
            sql.contains("LEFT OUTER JOIN LATERAL (SELECT * FROM b WHERE"),
            "{sql}"
        );
        assert!(sql.contains(") AS b ON true"), "{sql}");
        // Three-part names cannot bind to a derived alias.
        let sql = rewritten("SELECT dbo.b.k FROM a FULL JOIN dbo.b ON a.k = dbo.b.k AND i.id = 1");
        assert!(sql.contains("LEFT OUTER JOIN dbo.b ON"), "{sql}");
        // `p` inside `(...) AS r` is hidden, so `p.id` is an outer reference.
        let sql = rewritten(
            "SELECT 1 FROM a FULL JOIN (b AS p JOIN c ON p.k = c.k) AS r ON a.k = r.k AND p.id = 1",
        );
        assert!(!sql.contains("FULL"), "{sql}");
    }

    #[test]
    fn unaliased_functions_get_a_fresh_alias_and_samples_are_volatile() {
        let sql = rewritten(
            "SELECT 1 FROM OPENJSON(i.lhs) WITH (a INT) l FULL JOIN OPENJSON(i.rhs) WITH (b INT) ON l.a = b AND i.id = 1",
        );
        assert!(sql.contains(&format!(") AS {RIGHT} ON true")), "{sql}");
        let sql = "SELECT 1 FROM OPENJSON(i.lhs) l FULL JOIN t AS r TABLESAMPLE (10 PERCENT) ON l.[key] = r.k";
        assert!(rewritten(sql).contains("FULL JOIN"), "{sql}");
    }

    #[test]
    fn unqualified_star_excludes_the_side_column_and_rewrite_is_idempotent() {
        let mut statement = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "SELECT * FROM OPENJSON(p.lhs) l FULL JOIN OPENJSON(p.rhs) r ON l.[key] = r.[key] JOIN t ON t.k = l.[key]",
        )
        .unwrap()
        .remove(0);
        let Statement::Query(query) = &mut statement else {
            panic!("not a query")
        };
        rewrite(query);
        let once = query.to_string();
        assert!(
            once.contains(&format!("* EXCLUDE ({SIDES}.{SIDE})")),
            "{once}"
        );
        assert!(
            once.ends_with(&format!(
                "JOIN t ON t.k = l.[key] WHERE ({SIDES}.{SIDE} = 1 OR NOT EXISTS (SELECT 1 FROM OPENJSON(p.lhs) l WHERE l.[key] = r.[key]))"
            )),
            "{once}"
        );
        rewrite(query);
        assert_eq!(query.to_string(), once);
    }
}
