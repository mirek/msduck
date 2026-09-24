//! Preserve explicit integer conversion intent before backend lowering.
use sqlparser::ast::*;
const MARKER: &str = "__msduck_explicit_integer_source";

pub fn mark(statement: &mut Statement) {
    struct Mark;
    impl VisitorMut for Mark {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            let argument = match expr {
                Expr::Cast {
                    expr, data_type, ..
                } if crate::sql_type::integral_type(data_type)
                    || matches!(data_type, DataType::Bit(_)) =>
                {
                    Some(expr)
                }
                Expr::Convert {
                    expr,
                    data_type: Some(data_type),
                    charset: None,
                    styles,
                    ..
                } if (crate::sql_type::integral_type(data_type)
                    || matches!(data_type, DataType::Bit(_)))
                    && styles.is_empty() =>
                {
                    Some(expr)
                }
                _ => None,
            };
            if let Some(argument) = argument {
                **argument = crate::expr::unary_function(MARKER, *argument.clone());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = statement.visit(&mut Mark);
}
/// Borrow the original source without consuming the parser's conversion marker.
pub fn source(argument: &Expr) -> Option<&Expr> {
    let Expr::Function(f) = argument else {
        return None;
    };
    if f.name.to_string() != MARKER {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
        return None;
    };
    Some(value)
}
pub fn take(argument: &mut Expr) -> bool {
    if let Some(value) = source(argument).cloned() {
        *argument = value;
        true
    } else {
        false
    }
}
