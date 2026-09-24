//! Add execution observation after logical binding and backend aggregate lowering.
//! The caller supplies resolved aggregate names and a bound statement ticket.
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub fn operand(value: Expr, ticket: Expr) -> Expr {
    // Only this fixed backend template is parsed. User expressions are retained
    // as AST, outside the lambda, and occur exactly once in the singleton list.
    let mut result = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
        .try_with_sql("list_extract(list_transform([__msduck_operand], __msduck_null_value -> CASE WHEN __msduck_observe_null(__msduck_ticket, __msduck_null_value IS NULL) THEN NULL ELSE __msduck_null_value END), 1)")
        .expect("static diagnostic template").parse_expr().expect("static diagnostic expression");
    struct Replace {
        value: Expr,
        ticket: Expr,
    }
    impl VisitorMut for Replace {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(id) = expr {
                match id.value.as_str() {
                    "__msduck_operand" => *expr = self.value.clone(),
                    "__msduck_ticket" => *expr = self.ticket.clone(),
                    _ => {}
                }
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(&mut result, &mut Replace { value, ticket });
    result
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
                *value = operand(value.clone(), self.ticket.clone());
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
