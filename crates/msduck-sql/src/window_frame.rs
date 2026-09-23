//! SQL Server restrictions on explicit window frame specifications.
use sqlparser::ast::*;
use std::ops::ControlFlow;

const ORDER_REQUIRED: &str = "Window frame with ROWS or RANGE must have an ORDER BY clause.";
const RANGE_OFFSETS: &str =
    "RANGE is only supported with UNBOUNDED and CURRENT ROW window frame delimiters.";

// Check syntax before parameter binding or numeric literal lowering, including
// unused named definitions. This also prevents earlier batch writes on failure.
pub fn validate_syntax(statement: &Statement) -> Result<(), String> {
    fn check(spec: &WindowSpec) -> ControlFlow<String> {
        if let Some(frame) = &spec.window_frame {
            for bound in std::iter::once(&frame.start_bound).chain(frame.end_bound.iter()) {
                if let WindowFrameBound::Preceding(Some(expr))
                | WindowFrameBound::Following(Some(expr)) = bound
                    && digits(expr).is_none()
                {
                    return ControlFlow::Break(format!("Incorrect syntax near '{expr}'."));
                }
            }
        }
        ControlFlow::Continue(())
    }
    struct Syntax;
    impl Visitor for Syntax {
        type Break = String;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if let Expr::Function(function) = expr
                && let Some(WindowType::WindowSpec(spec)) = &function.over
            {
                return check(spec);
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<String> {
            for definition in &select.named_window {
                if let NamedWindowExpr::WindowSpec(spec) = &definition.1 {
                    check(spec)?;
                }
            }
            ControlFlow::Continue(())
        }
    }
    match statement.visit(&mut Syntax) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

fn digits(expr: &Expr) -> Option<&str> {
    let Expr::Value(value) = expr else {
        return None;
    };
    let Value::Number(number, _) = &value.value else {
        return None;
    };
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| number.trim_start_matches('0'))
}

fn bound_order(left: &WindowFrameBound, right: &WindowFrameBound) -> std::cmp::Ordering {
    use WindowFrameBound::*;
    fn category(bound: &WindowFrameBound) -> u8 {
        match bound {
            Preceding(None) => 0,
            Preceding(Some(_)) => 1,
            CurrentRow => 2,
            Following(Some(_)) => 3,
            Following(None) => 4,
        }
    }
    match (left, right) {
        (Preceding(Some(a)), Preceding(Some(b))) | (Following(Some(a)), Following(Some(b))) => {
            // Decimal string comparison avoids narrowing arbitrarily large
            // literals. Syntax was checked before expression translation.
            let (a, b) = (digits(a).unwrap_or(""), digits(b).unwrap_or(""));
            let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
            if matches!(left, Preceding(_)) {
                order.reverse()
            } else {
                order
            }
        }
        _ => category(left).cmp(&category(right)),
    }
}

pub fn validate(function: &Function) -> Result<(), String> {
    let Some(WindowType::WindowSpec(spec)) = &function.over else {
        return Ok(());
    };
    let Some(frame) = &spec.window_frame else {
        return Ok(());
    };
    let name = function.name.to_string().to_ascii_lowercase();
    if matches!(name.as_str(), "percentile_cont" | "percentile_disc") {
        return Err(format!(
            "The function '{name}' may not have a window frame."
        ));
    }
    if frame.units == WindowFrameUnits::Groups {
        return Err("unsupported GROUPS window frame".into());
    }
    // Query translation expands named windows before function validation.
    if spec.order_by.is_empty() && spec.window_name.is_none() {
        return Err(ORDER_REQUIRED.into());
    }
    if frame.units == WindowFrameUnits::Range
        && std::iter::once(&frame.start_bound)
            .chain(frame.end_bound.iter())
            .any(|bound| {
                matches!(
                    bound,
                    WindowFrameBound::Preceding(Some(_)) | WindowFrameBound::Following(Some(_))
                )
            })
    {
        return Err(RANGE_OFFSETS.into());
    }
    let end = frame
        .end_bound
        .as_ref()
        .unwrap_or(&WindowFrameBound::CurrentRow);
    if matches!(frame.start_bound, WindowFrameBound::Following(None))
        || matches!(end, WindowFrameBound::Preceding(None))
        || bound_order(&frame.start_bound, end).is_gt()
    {
        let description = match &frame.end_bound {
            Some(end) => format!("{} BETWEEN {} AND {end}", frame.units, frame.start_bound),
            None => format!("{} {}", frame.units, frame.start_bound),
        };
        return Err(format!(
            "'{description}' is not a valid window frame and cannot be used with the OVER clause."
        ));
    }
    Ok(())
}

pub fn error_number(message: &str) -> Option<i32> {
    match message {
        _ if message.starts_with("Incorrect syntax near '") && message.ends_with("'.") => Some(102),
        ORDER_REQUIRED => Some(10756),
        RANGE_OFFSETS => Some(4194),
        _ if message
            .ends_with("is not a valid window frame and cannot be used with the OVER clause.") =>
        {
            Some(4193)
        }
        _ => None,
    }
}
