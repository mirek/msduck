//! Add execution observation after logical binding and backend aggregate lowering.
//! The caller supplies resolved aggregate names and a bound statement ticket.
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub fn operand(value: Expr, ticket: Expr) -> Expr {
    // Only this fixed backend template is parsed. User expressions are retained
    // as AST, outside the lambda, and occur exactly once in the singleton list.
    template(
        "list_extract(list_transform([__msduck_operand], __msduck_null_value -> CASE WHEN __msduck_observe_null(__msduck_ticket, __msduck_null_value IS NULL) THEN NULL ELSE __msduck_null_value END), 1)",
        &[("__msduck_operand", value), ("__msduck_ticket", ticket)],
    )
}

fn template(sql: &str, replacements: &[(&str, Expr)]) -> Expr {
    let mut result = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
        .try_with_sql(sql)
        .expect("static diagnostic template")
        .parse_expr()
        .expect("static diagnostic expression");
    struct Replace<'a> {
        replacements: &'a [(&'a str, Expr)],
    }
    impl VisitorMut for Replace<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(id) = expr
                && let Some((_, replacement)) =
                    self.replacements.iter().find(|(name, _)| *name == id.value)
            {
                *expr = replacement.clone();
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(&mut result, &mut Replace { replacements });
    result
}

fn window(mut function: Function, ticket: Expr) -> Expr {
    let name = function.name.to_string();
    if name.eq_ignore_ascii_case("count") {
        function.name = ObjectName::from(vec![Ident::new("__msduck_count_frame")]);
        // The native aggregate carries count and NULL presence in bounded
        // state. Observe the returned frame, never intermediate segment states.
        return template(
            "COALESCE(list_extract(list_transform([__msduck_window_pair], __msduck_count_pair -> CASE WHEN __msduck_observe_null(__msduck_ticket, __msduck_count_pair.eliminated) IS NOT NULL THEN __msduck_count_pair.value ELSE NULL END), 1), CAST(0 AS BIGINT))",
            &[
                ("__msduck_window_pair", Expr::Function(function)),
                ("__msduck_ticket", ticket),
            ],
        );
    }
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "__msduck_sum_int"
            | "__msduck_sum_big"
            | "__msduck_sum_money"
            | "__msduck_avg_int"
            | "__msduck_avg_big"
            | "__msduck_avg_money"
    ) {
        function.name = ObjectName::from(vec![Ident::new(format!(
            "{}_frame",
            name.to_ascii_lowercase()
        ))]);
        // The native pair retains the original value type and bounded arithmetic.
        // Empty/all-NULL values stay NULL; only COUNT needs a zero fallback.
        return template(
            "list_extract(list_transform([__msduck_window_pair], __msduck_aggregate_pair -> CASE WHEN __msduck_observe_null(__msduck_ticket, __msduck_aggregate_pair.eliminated) IS NOT NULL THEN __msduck_aggregate_pair.value ELSE NULL END), 1)",
            &[
                ("__msduck_window_pair", Expr::Function(function)),
                ("__msduck_ticket", ticket),
            ],
        );
    }
    function.name = ObjectName::from(vec![Ident::new("list")]);
    // LIST retains NULLs and uses the original partition, ordering and frame.
    // Its operand is evaluated once per input row, outside the lambda. Observe
    // each resulting frame once; an empty frame cannot eliminate NULL.
    template(
        "list_extract(list_transform([__msduck_window_values], __msduck_frame -> CASE WHEN __msduck_observe_null(__msduck_ticket, list_count(__msduck_frame) < len(__msduck_frame)) IS NOT NULL THEN list_aggregate(__msduck_frame, __msduck_aggregate_name) ELSE NULL END), 1)",
        &[
            ("__msduck_window_values", Expr::Function(function)),
            ("__msduck_ticket", ticket),
            (
                "__msduck_aggregate_name",
                Expr::Value(Value::SingleQuotedString(name.clone()).into()),
            ),
        ],
    )
}

/// Observe unary aggregate arguments. COUNT(*) and unrelated functions are
/// unchanged. Grouping, DISTINCT and window specifications remain on the call.
pub fn instrument<T: VisitMut>(
    node: &mut T,
    ticket: &Expr,
    selected: impl Fn(&str) -> bool,
) -> usize {
    struct Instrument<'a, F> {
        ticket: &'a Expr,
        selected: F,
        count: usize,
    }
    impl<F: Fn(&str) -> bool> VisitorMut for Instrument<'_, F> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Function(function) = expr
                && (self.selected)(&function.name.to_string().to_ascii_lowercase())
                && let FunctionArguments::List(args) = &mut function.args
                && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] =
                    args.args.as_mut_slice()
            {
                if function.over.is_some() {
                    *expr = window(function.clone(), self.ticket.clone());
                } else {
                    *value = operand(value.clone(), self.ticket.clone());
                }
                self.count += 1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut visitor = Instrument {
        ticket,
        selected,
        count: 0,
    };
    let _ = node.visit(&mut visitor);
    visitor.count
}
