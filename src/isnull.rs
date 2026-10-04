use sqlparser::ast::*;

pub use msduck_sql::expression_metadata::conditional::isnull_args as args;

pub use msduck_sql::expression_metadata::conditional::binary_args;

pub use msduck_sql::expression_metadata::conditional::literal_null;

// A scalar query already owns its aggregate and correlation scope. Bind it
// once before the dispatch macro repeats type prototypes. Other expression
// shapes keep their existing scope; wrapping an outer SUM, for example, would
// relocate it into a scalar query.
fn bind_scalar_subquery_once(first: &Expr, replacement: &Expr) -> bool {
    #[derive(Default)]
    struct ScalarScope {
        depth: usize,
        query: bool,
        outer_scope: bool,
    }
    impl Visitor for ScalarScope {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> std::ops::ControlFlow<()> {
            self.depth += 1;
            self.query = true;
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> std::ops::ControlFlow<()> {
            self.depth -= 1;
            std::ops::ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            if let Expr::Function(f) = expr {
                let name = f.name.to_string().to_ascii_lowercase();
                // Physical character dispatch may already surround the scalar
                // query with CASE/typeof/native conversion calls. Those calls
                // are scalar, while unknown or aggregate/window functions may
                // belong to the caller's row/group scope.
                let scalar = matches!(
                    name.as_str(),
                    "typeof"
                        | "cast_to_type"
                        | "coalesce"
                        | "isnull"
                        | "nullif"
                        | "__msduck_cast_carrier_nvarchar"
                        | "__msduck_cast_carrier_nchar"
                        | "__msduck_cast_nvarchar"
                        | "__msduck_cast_nchar"
                        | "__msduck_carrier_input"
                        | "__msduck_unicode_input"
                        | "__msduck_explicit_integer_source"
                        | "__msduck_integer_text"
                        | "__msduck_integer_input"
                );
                let scoped_aggregate = self.depth > 0
                    && matches!(
                        name.as_str(),
                        "min"
                            | "max"
                            | "sum"
                            | "avg"
                            | "count"
                            | "__msduck_extrema_input_unicode"
                            | "__msduck_extrema_input_ansi"
                    );
                // Unknown calls may be volatile even inside an existing scalar
                // query. The wrapper must not change their correlation/cache
                // boundary. Aggregate/window calls outside that query belong
                // to the caller and must never move into the wrapper.
                self.outer_scope |= !(scalar || scoped_aggregate)
                    || (self.depth == 0
                        && (f.over.is_some() || f.filter.is_some() || !f.within_group.is_empty()));
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut scope = ScalarScope::default();
    let _ = first.visit(&mut scope);
    let first_has_query = scope.query;
    let _ = replacement.visit(&mut scope);
    struct Collision;
    impl Visitor for Collision {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<()> {
            let reserved = |id: &Ident| {
                matches!(
                    id.value.to_ascii_lowercase().as_str(),
                    "__msduck_isnull_bound_first" | "__msduck_isnull_bound_input"
                )
            };
            let found = match expr {
                Expr::Identifier(id) => reserved(id),
                Expr::CompoundIdentifier(ids) => ids.iter().any(reserved),
                _ => false,
            };
            if found {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
    }
    first_has_query
        && !scope.outer_scope
        && first.visit(&mut Collision).is_continue()
        && replacement.visit(&mut Collision).is_continue()
}

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some([first, replacement]) = args(function)? else {
        return Ok(());
    };
    let text_type = crate::result_types::isnull_type(first, replacement);
    let scalar_subquery = bind_scalar_subquery_once(first, replacement);
    if literal_null(first) {
        *expr = if literal_null(replacement) {
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(replacement.clone()),
                data_type: DataType::Int(None),
                format: None,
            }
        } else {
            replacement.clone()
        };
        // A replacement can itself be ISNULL; pre-visit does not revisit this node.
        return lower(expr);
    }
    if let Some(crate::tds::Type::Char(width)) = &text_type {
        let converted = crate::engine::binary_function(
            "__msduck_cast_char",
            replacement.clone(),
            Expr::Value(Value::Number(width.to_string(), false).into()),
        );
        if let FunctionArguments::List(args) = &mut function.args {
            args.args[1] = FunctionArg::Unnamed(FunctionArgExpr::Expr(converted));
        }
    }
    function.name = ObjectName::from(vec![Ident::new(if scalar_subquery {
        "__msduck_isnull_subquery"
    } else {
        "__msduck_isnull"
    })]);
    if let Some(kind) = text_type {
        let (name, width) = match kind {
            // A carrier first argument keeps its carrier through the width.
            crate::tds::Type::Nchar(width) => ("__msduck_isnull_nchar_width", width),
            crate::tds::Type::Nvarchar(width) => ("__msduck_isnull_nvarchar_width", width),
            crate::tds::Type::Char(width) => ("__msduck_char_width", width),
            crate::tds::Type::Varchar(u16::MAX) => return Ok(()),
            crate::tds::Type::Varchar(width) => ("__msduck_varchar_width", width),
            _ => return Ok(()),
        };
        *expr = crate::engine::binary_function(
            name,
            expr.clone(),
            Expr::Value(Value::Number(width.to_string(), false).into()),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::{dialect::MsSqlDialect, parser::Parser};

    fn operands(sql: &str) -> [Expr; 2] {
        let statement = Parser::parse_sql(&MsSqlDialect {}, sql).unwrap().remove(0);
        let Statement::Query(query) = statement else {
            panic!("query")
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!("select")
        };
        let SelectItem::UnnamedExpr(Expr::Function(function)) = &select.projection[0] else {
            panic!("function")
        };
        let [first, replacement] = args(function).unwrap().unwrap();
        [first.clone(), replacement.clone()]
    }

    #[test]
    fn scalar_query_dispatch_keeps_aggregate_scope_and_avoids_name_capture() {
        for sql in [
            "SELECT ISNULL((SELECT MAX(v) FROM (VALUES (1),(2)) t(v)), 0)",
            "SELECT ISNULL(CAST((SELECT MAX(v) FROM t) AS INT), 0)",
            "SELECT ISNULL(CAST((SELECT MAX(v) FROM t) AS NVARCHAR(10)) COLLATE SQL_Latin1_General_CP1_CI_AS, N'')",
            "SELECT ISNULL(CAST(CASE WHEN typeof((SELECT MAX(v) FROM t))='VARCHAR' THEN (SELECT MAX(v) FROM t) ELSE NULL END AS NVARCHAR(10)), N'')",
        ] {
            let [first, replacement] = operands(sql);
            assert!(bind_scalar_subquery_once(&first, &replacement));
        }
        for sql in [
            "SELECT ISNULL(SUM(v), 0) FROM t",
            "SELECT ISNULL(SUM((SELECT v FROM t)), 0) FROM t",
            "SELECT ISNULL(FIRST_VALUE((SELECT v FROM t)) OVER (ORDER BY id), 0) FROM t",
            "SELECT ISNULL(v, 0) FROM t",
            "SELECT ISNULL((SELECT CAST(NULL AS INT)), SUM(v)) FROM t",
            "SELECT ISNULL((SELECT CAST(NULL AS INT)), SUM(v) OVER ()) FROM t",
            "SELECT ISNULL((SELECT CAST(NULL AS INT)), nextval('counter')) FROM t",
            "SELECT ISNULL((SELECT nextval('counter')), 0) FROM t",
            "SELECT ISNULL((SELECT CAST(NULL AS INT)), (SELECT nextval('counter'))) FROM t",
            "SELECT ISNULL((SELECT MAX(v) FROM t), NEWID())",
            "SELECT ISNULL((SELECT mystery(v) FROM t), 0)",
            "SELECT ISNULL((SELECT __msduck_isnull_bound_first FROM t), 0)",
            "SELECT ISNULL((SELECT MAX(v) FROM t), __msduck_isnull_bound_first)",
            "SELECT ISNULL((SELECT MAX(v) FROM t), __msduck_isnull_bound_input.v)",
        ] {
            let [first, replacement] = operands(sql);
            assert!(!bind_scalar_subquery_once(&first, &replacement));
        }
    }
}
