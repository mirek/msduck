//! Late lowering of operations over Unicode carriers (see the parent
//! module). It runs on the backend AST, after the built-in lowerings, and
//! dispatches on `typeof`, which DuckDB folds while binding: every rewrite
//! keeps the original expression as the branch for operands that are not
//! carriers, so other types keep their exact behavior.
use sqlparser::ast::{helpers::attached_token::AttachedToken, *};
use std::ops::ControlFlow;

pub(super) const CARRIER: &str = "STRUCT(__msduck_utf16le BLOB)";
pub(super) const ORDER_KEY: &str = "__msduck_unicode_order_key";
pub(super) const TEXT: &str = "__msduck_unicode_text";
pub(super) const LIKE: &str = "__msduck_unicode_like";
/// ORDER BY markers placed by [`super::order`].
pub(super) const SORT: &str = "__msduck_unicode_sort";
pub(super) const TIE: &str = "__msduck_unicode_tie";
/// Unicode operand marker placed by [`super::mark`].
pub(super) const MARK: &str = "__msduck_unicode_value";
/// IN (subquery) marker placed by [`super::mark`].
pub(super) const MEMBER: &str = "__msduck_unicode_in";
pub(super) const INPUT: &str = "__msduck_unicode_input";
/// [`INPUT`] for marked operands, failing for types that are not text.
pub(super) const OPERAND: &str = "__msduck_unicode_operand";
/// The engine's carrier conversion, which carrier consumers wrap their
/// operands in.
const CARRIER_INPUT: &str = "__msduck_carrier_input";

fn call(name: &str, args: Vec<Expr>) -> Expr {
    let mut function = msduck_sql::expr::binary_function(name, null(), null());
    if let Expr::Function(f) = &mut function
        && let FunctionArguments::List(list) = &mut f.args
    {
        list.args = args
            .into_iter()
            .map(|arg| FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)))
            .collect();
    }
    function
}

fn text(value: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(value.into()).into())
}

fn null() -> Expr {
    Expr::Value(Value::Null.into())
}

fn input(value: Expr) -> Expr {
    call(INPUT, vec![value])
}

fn key(value: Expr) -> Expr {
    call(ORDER_KEY, vec![input(value)])
}

/// `typeof(value) IN (types)`.
fn type_in(value: &Expr, types: &[&str]) -> Expr {
    Expr::InList {
        expr: Box::new(call("typeof", vec![value.clone()])),
        list: types.iter().map(|t| text(t)).collect(),
        negated: false,
    }
}

fn any(mut conditions: Vec<Expr>) -> Expr {
    let mut result = conditions.remove(0);
    for condition in conditions {
        result = Expr::BinaryOp {
            left: Box::new(result),
            op: BinaryOperator::Or,
            right: Box::new(condition),
        };
    }
    Expr::Nested(Box::new(result))
}

/// Whether any operand is a carrier. DuckDB refuses to bind ordering
/// comparisons, IN and LIKE between a carrier and another type, so then
/// all operands compare as Unicode text: carriers by their code units,
/// other values by their VARCHAR text.
fn textual(operands: &[&Expr]) -> Expr {
    any(operands.iter().map(|o| type_in(o, &[CARRIER])).collect())
}

fn case(condition: Expr, result: Expr, otherwise: Expr) -> Expr {
    Expr::Case {
        case_token: AttachedToken::empty(),
        end_token: AttachedToken::empty(),
        operand: None,
        conditions: vec![CaseWhen { condition, result }],
        else_result: Some(Box::new(otherwise)),
    }
}

fn not(value: Expr, negated: bool) -> Expr {
    if negated {
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: Box::new(Expr::Nested(Box::new(value))),
        }
    } else {
        value
    }
}

fn function_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Function(f) => Some(f.name.to_string()),
        _ => None,
    }
}

fn arguments(expr: &Expr) -> Option<Vec<&Expr>> {
    let Expr::Function(f) = expr else {
        return None;
    };
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    list.args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect()
}

/// A generated `typeof` dispatch: its first condition tests `typeof`.
fn dispatch(expr: &Expr) -> bool {
    fn tests_type(condition: &Expr) -> bool {
        match condition {
            Expr::Nested(e) => tests_type(e),
            Expr::BinaryOp { left, op, right } => match op {
                BinaryOperator::And | BinaryOperator::Or => tests_type(left) || tests_type(right),
                _ => is_typeof(left) || is_typeof(right),
            },
            Expr::InList { expr, .. } => is_typeof(expr),
            _ => false,
        }
    }
    matches!(expr, Expr::Case { operand: None, conditions, .. }
        if conditions.first().is_some_and(|c| tests_type(&c.condition)))
}

/// A dispatch made here: its type tests are `typeof(…) IN (…)`.
fn own(expr: &Expr) -> bool {
    fn tests(condition: &Expr) -> bool {
        match condition {
            Expr::Nested(e) => tests(e),
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Or | BinaryOperator::And,
                right,
            } => tests(left) || tests(right),
            Expr::InList { expr, .. } => is_typeof(expr),
            _ => false,
        }
    }
    matches!(expr, Expr::Case { operand: None, conditions, .. }
        if conditions.first().is_some_and(|c| tests(&c.condition)))
}

fn is_typeof(expr: &Expr) -> bool {
    function_name(expr).is_some_and(|name| name.eq_ignore_ascii_case("typeof"))
}

fn has_subquery(expr: &Expr) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            ControlFlow::Break(())
        }
    }
    expr.visit(&mut Find).is_break()
}

/// Whether a backend operand may be a Unicode carrier: column references,
/// function results, subqueries and conditional expressions may; literals,
/// parameters, non-STRUCT casts and arithmetic may not.
fn carries(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(e) => carries(e),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Subquery(_) => true,
        // This module's dispatches give text, keys or booleans.
        Expr::Case { .. } => !own(expr),
        Expr::Cast { data_type, .. } => {
            matches!(data_type, DataType::Struct(..))
                || data_type.to_string().eq_ignore_ascii_case(CARRIER)
        }
        // Function results may be carriers, except this module's keys,
        // text and tests.
        Expr::Function(f) => {
            let name = f.name.to_string();
            ![ORDER_KEY, TEXT, LIKE, MEMBER, INPUT, OPERAND, "typeof"]
                .iter()
                .any(|own| name.eq_ignore_ascii_case(own))
        }
        _ => false,
    }
}

fn collated(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(e) => collated(e),
        Expr::Collate { .. } => true,
        _ => false,
    }
}

fn comparison(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::Gt
            | BinaryOperator::LtEq
            | BinaryOperator::GtEq
    )
}

fn marked(expr: &Expr) -> bool {
    function_name(expr).as_deref() == Some(MARK)
}

/// A marked operation's operand as a carrier.
fn strip(expr: &Expr) -> Expr {
    if marked(expr)
        && let Some(args) = arguments(expr)
        && let [value] = args.as_slice()
    {
        return call(OPERAND, vec![(*value).clone()]);
    }
    call(OPERAND, vec![expr.clone()])
}

fn strict_key(expr: &Expr) -> Expr {
    call(ORDER_KEY, vec![strip(expr)])
}

/// Whether an operation over `operands` gets a `typeof` dispatch: some
/// operand may be a carrier, and none has an explicit COLLATE or is itself
/// a type test. Predicates over subqueries are not dispatched: DuckDB plans
/// a subquery in every branch, even one `typeof` rules out, so a filter
/// would run it once per branch ([`super::mark`] marks those that resolve
/// to carrier columns instead). Conversions do dispatch them.
fn dispatched(operands: &[&Expr]) -> bool {
    operands.iter().any(|o| carries(o))
        && !operands
            .iter()
            .any(|o| has_subquery(o) || collated(o) || is_typeof(o))
}

/// The rewrite of one comparison, or `None` when it needs none.
fn compare(left: &Expr, op: &BinaryOperator, right: &Expr) -> Option<Expr> {
    if comparison(op) && (marked(left) || marked(right)) {
        return Some(Expr::BinaryOp {
            left: Box::new(strict_key(left)),
            op: op.clone(),
            right: Box::new(strict_key(right)),
        });
    }
    if !comparison(op) || !dispatched(&[left, right]) {
        return None;
    }
    Some(case(
        textual(&[left, right]),
        Expr::BinaryOp {
            left: Box::new(key(left.clone())),
            op: op.clone(),
            right: Box::new(key(right.clone())),
        },
        Expr::BinaryOp {
            left: Box::new(left.clone()),
            op: op.clone(),
            right: Box::new(right.clone()),
        },
    ))
}

/// The carrier conversion matching a backend character cast macro.
fn carrier_cast(name: &str) -> Option<(&'static str, bool)> {
    Some(match name {
        "__msduck_cast_nvarchar" | "__msduck_try_nvarchar" => {
            ("__msduck_cast_carrier_nvarchar", true)
        }
        "__msduck_cast_varchar" | "__msduck_try_varchar" => {
            ("__msduck_cast_carrier_varchar", false)
        }
        "__msduck_cast_char" | "__msduck_try_char" => ("__msduck_cast_carrier_char", false),
        _ => return None,
    })
}

fn rewrite(expr: &mut Expr) {
    let replacement = match &*expr {
        Expr::BinaryOp { left, op, right } => compare(left, op, right),
        Expr::Like {
            negated,
            any: false,
            expr: value,
            pattern,
            escape_char,
        } if marked(value) || marked(pattern) => {
            let mut args = vec![strip(value), strip(pattern)];
            if let Some(escape) = escape_char {
                args.push(strip(escape));
            }
            Some(not(call(LIKE, args), *negated))
        }
        Expr::Like {
            negated,
            any: false,
            expr: value,
            pattern,
            escape_char,
        } if dispatched(&[value, pattern]) => {
            let mut operands = vec![value.as_ref(), pattern.as_ref()];
            let mut args = vec![input(*value.clone()), input(*pattern.clone())];
            if let Some(escape) = escape_char {
                operands.push(escape);
                args.push(input(*escape.clone()));
            }
            // DuckDB binds LIKE only over VARCHAR, so the branch for other
            // types must bind for carriers too.
            let varchar = |e: &Expr| {
                if carries(e) {
                    Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(e.clone()),
                        data_type: DataType::Varchar(None),
                        format: None,
                    }
                } else {
                    e.clone()
                }
            };
            Some(case(
                textual(&operands),
                not(call(LIKE, args), *negated),
                Expr::Like {
                    negated: *negated,
                    any: false,
                    expr: Box::new(varchar(value)),
                    pattern: Box::new(varchar(pattern)),
                    escape_char: escape_char.as_deref().map(|e| Box::new(varchar(e))),
                },
            ))
        }
        Expr::InList {
            expr: value,
            list,
            negated,
        } if marked(value) => Some(Expr::InList {
            expr: Box::new(strict_key(value)),
            list: list.iter().map(strict_key).collect(),
            negated: *negated,
        }),
        Expr::InList {
            expr: value,
            list,
            negated,
        } if dispatched(&[value]) && !list.iter().any(has_subquery) => Some(case(
            textual(&[value]),
            Expr::InList {
                expr: Box::new(key(*value.clone())),
                list: list.iter().cloned().map(key).collect(),
                negated: *negated,
            },
            expr.clone(),
        )),
        Expr::Between {
            expr: value,
            negated,
            low,
            high,
        } if [value, low, high].iter().any(|e| marked(e)) => Some(Expr::Between {
            expr: Box::new(strict_key(value)),
            negated: *negated,
            low: Box::new(strict_key(low)),
            high: Box::new(strict_key(high)),
        }),
        Expr::Between {
            expr: value,
            negated,
            low,
            high,
        } if dispatched(&[value, low, high]) => Some(case(
            textual(&[value, low, high]),
            Expr::Between {
                expr: Box::new(key(*value.clone())),
                negated: *negated,
                low: Box::new(key(*low.clone())),
                high: Box::new(key(*high.clone())),
            },
            expr.clone(),
        )),
        Expr::InSubquery {
            expr: value,
            subquery,
            negated,
        } if function_name(value).as_deref() == Some(MEMBER) => arguments(value)
            .and_then(|args| args.first().map(|v| (*v).clone()))
            .and_then(|value| membership(&value, subquery))
            .map(|test| not(test, *negated)),
        Expr::Case {
            operand: Some(operand),
            conditions,
            else_result,
            ..
        } if dispatched(
            &std::iter::once(operand.as_ref())
                .chain(conditions.iter().map(|c| &c.condition))
                .collect::<Vec<_>>(),
        ) || conditions.iter().any(|c| marked(&c.condition))
            || marked(operand) =>
        {
            Some(Expr::Case {
                case_token: AttachedToken::empty(),
                end_token: AttachedToken::empty(),
                operand: None,
                conditions: conditions
                    .iter()
                    .map(|c| CaseWhen {
                        condition: compare(operand, &BinaryOperator::Eq, &c.condition)
                            .unwrap_or_else(|| Expr::BinaryOp {
                                left: operand.clone(),
                                op: BinaryOperator::Eq,
                                right: Box::new(c.condition.clone()),
                            }),
                        result: c.result.clone(),
                    })
                    .collect(),
                else_result: else_result.clone(),
            })
        }
        Expr::Cast {
            kind: CastKind::Cast,
            expr: value,
            data_type: DataType::Varchar(None) | DataType::Text | DataType::String(None),
            format: None,
        } if carries(value) => Some(case(
            type_in(value, &[CARRIER]),
            call(TEXT, vec![input(*value.clone())]),
            expr.clone(),
        )),
        Expr::Substring { expr: value, .. } if carries(value) => {
            let mut lowered = expr.clone();
            if let Expr::Substring { expr: value, .. } = &mut lowered {
                **value = as_text(value);
            }
            Some(lowered)
        }
        Expr::Function(f) => function(f.name.to_string().as_str(), expr),
        _ => None,
    };
    if let Some(replacement) = replacement {
        *expr = replacement;
    }
}

fn function(name: &str, expr: &Expr) -> Option<Expr> {
    let args = arguments(expr)?;
    if let Some((cast, unicode)) = carrier_cast(name)
        && let [value, width] = args.as_slice()
        && carries(value)
    {
        let converted = call(cast, vec![input((*value).clone()), (*width).clone()]);
        return Some(case(
            type_in(value, &[CARRIER]),
            if unicode {
                call(TEXT, vec![converted])
            } else {
                converted
            },
            expr.clone(),
        ));
    }
    match name {
        // A carrier conversion feeding a carrier consumer stays a carrier,
        // so isolated surrogates survive.
        CARRIER_INPUT => {
            let [
                Expr::Case {
                    conditions,
                    else_result: Some(otherwise),
                    ..
                },
            ] = args.as_slice()
            else {
                return None;
            };
            let [CaseWhen { condition, result }] = conditions.as_slice() else {
                return None;
            };
            if !dispatch(args[0]) || function_name(result).as_deref() != Some(TEXT) {
                return None;
            }
            let inner = arguments(result)?;
            Some(case(
                condition.clone(),
                inner[0].clone(),
                call(CARRIER_INPUT, vec![*otherwise.clone()]),
            ))
        }
        SORT => {
            let [value] = args.as_slice() else {
                return None;
            };
            Some(case(
                type_in(value, &[CARRIER]),
                key((*value).clone()),
                null(),
            ))
        }
        TIE => {
            let [value] = args.as_slice() else {
                return None;
            };
            Some(case(type_in(value, &[CARRIER]), null(), (*value).clone()))
        }
        _ => {
            let positions = text_arguments(name)?;
            let wrap =
                |(i, a): (usize, &&Expr)| positions.is_none_or(|p| p.contains(&i)) && carries(a);
            if !args.iter().enumerate().any(wrap) {
                return None;
            }
            let mut lowered = expr.clone();
            let Expr::Function(f) = &mut lowered else {
                return None;
            };
            let FunctionArguments::List(list) = &mut f.args else {
                return None;
            };
            for (i, arg) in list.args.iter_mut().enumerate() {
                if let FunctionArg::Unnamed(FunctionArgExpr::Expr(a)) = arg
                    && wrap((i, &&*a))
                {
                    *a = as_text(a);
                }
            }
            Some(lowered)
        }
    }
}

/// The VARCHAR-only DuckDB functions that SQL Server character functions
/// pass through to, with their text argument positions (`None`: all).
fn text_arguments(name: &str) -> Option<Option<&'static [usize]>> {
    Some(match name.to_ascii_lowercase().as_str() {
        "concat" | "concat_ws" | "replace" => None,
        "substring" | "substr" | "reverse" | "ascii" => Some(&[0]),
        "string_agg" => Some(&[0, 1]),
        _ => return None,
    })
}

/// A carrier as its text; other values cast to VARCHAR, which is what a
/// VARCHAR-only function binds them as.
fn as_text(value: &Expr) -> Expr {
    case(
        type_in(value, &[CARRIER]),
        call(TEXT, vec![input(value.clone())]),
        Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(value.clone()),
            data_type: DataType::Varchar(None),
            format: None,
        },
    )
}

/// `value IN (subquery)` as SQL's three-valued membership over Unicode
/// text equality, for operands DuckDB cannot compare directly:
/// true when some row matches; otherwise false for an empty subquery, NULL
/// when `value` is NULL or the subquery has a NULL, and false.
fn membership(value: &Expr, subquery: &Query) -> Option<Expr> {
    const TEMPLATE: &str = "CASE WHEN EXISTS (SELECT 1 FROM (SELECT 1) AS __msduck_in(__msduck_v) WHERE __msduck_match) THEN true \
        WHEN NOT EXISTS (SELECT 1 FROM (SELECT 1) AS __msduck_in(__msduck_v)) THEN false \
        WHEN __msduck_value IS NULL OR EXISTS (SELECT 1 FROM (SELECT 1) AS __msduck_in(__msduck_v) WHERE __msduck_in.__msduck_v IS NULL) THEN NULL \
        ELSE false END";
    let SetExpr::Select(select) = subquery.body.as_ref() else {
        return None;
    };
    if select.projection.len() != 1
        || matches!(
            select.projection[0],
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
        )
    {
        return None;
    }
    let element =
        Expr::CompoundIdentifier(vec![Ident::new("__msduck_in"), Ident::new("__msduck_v")]);
    let matched = Expr::BinaryOp {
        left: Box::new(strict_key(value)),
        op: BinaryOperator::Eq,
        right: Box::new(strict_key(&element)),
    };
    let mut test = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
        .try_with_sql(TEMPLATE)
        .ok()?
        .parse_expr()
        .ok()?;
    struct Fill<'a> {
        value: &'a Expr,
        matched: &'a Expr,
        subquery: &'a Query,
    }
    impl VisitorMut for Fill<'_> {
        type Break = ();
        fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
            if let TableFactor::Derived { subquery, .. } = factor {
                **subquery = self.subquery.clone();
            }
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(ident) = expr {
                match ident.value.as_str() {
                    "__msduck_value" => *expr = self.value.clone(),
                    "__msduck_match" => *expr = self.matched.clone(),
                    _ => {}
                }
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(
        &mut test,
        &mut Fill {
            value,
            matched: &matched,
            subquery,
        },
    );
    Some(Expr::Nested(Box::new(test)))
}

/// Lower every carrier operation in `expr`, outside nested queries (the
/// translator lowers those separately) and outside earlier dispatches.
pub(super) fn lower(expr: &mut Expr) {
    struct Lower {
        /// Per open expression: whether it is a dispatch to leave alone.
        frozen: Vec<bool>,
        queries: usize,
    }
    impl VisitorMut for Lower {
        type Break = ();
        fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.queries += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<()> {
            self.queries -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            let frozen = self.frozen.last() == Some(&true) || dispatch(expr);
            self.frozen.push(frozen);
            ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            let frozen = self.frozen.pop().unwrap_or(false);
            if !frozen && self.queries == 0 {
                rewrite(expr);
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(
        expr,
        &mut Lower {
            frozen: Vec::new(),
            queries: 0,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(sql: &str) -> Expr {
        sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
            .try_with_sql(sql)
            .unwrap()
            .parse_expr()
            .unwrap()
    }

    fn lowered(sql: &str) -> String {
        let mut value = expr(sql);
        lower(&mut value);
        // Lowering is idempotent: ancestors lower their whole subtree again.
        let once = value.to_string();
        lower(&mut value);
        assert_eq!(value.to_string(), once, "{sql}");
        once
    }

    #[test]
    fn literals_parameters_and_subqueries_are_left_alone() {
        for sql in [
            "1 = 2",
            "'a' = 'b'",
            "$1 < 'x'",
            "CAST(x AS INTEGER) = 1",
            "x = (SELECT y FROM t)",
            "(SELECT y FROM t) LIKE 'a%'",
            "x COLLATE \"C\" = 'a'",
            "typeof(x) = 'VARCHAR'",
        ] {
            assert_eq!(lowered(sql), expr(sql).to_string(), "{sql}");
        }
    }

    #[test]
    fn possible_carriers_dispatch_on_type_and_keep_the_original() {
        let comparison = lowered("n = 'x'");
        assert_eq!(
            comparison,
            "CASE WHEN (typeof(n) IN ('STRUCT(__msduck_utf16le BLOB)') OR typeof('x') IN ('STRUCT(__msduck_utf16le BLOB)')) \
             THEN __msduck_unicode_order_key(__msduck_unicode_input(n)) = __msduck_unicode_order_key(__msduck_unicode_input('x')) \
             ELSE n = 'x' END"
        );
        let like = lowered("n NOT LIKE 'a!%' ESCAPE '!'");
        assert!(like.contains("THEN NOT (__msduck_unicode_like(__msduck_unicode_input(n), __msduck_unicode_input('a!%'), __msduck_unicode_input('!')))"), "{like}");
        assert!(
            like.ends_with("ELSE CAST(n AS VARCHAR) NOT LIKE 'a!%' ESCAPE '!' END"),
            "{like}"
        );
        let simple = lowered("CASE n WHEN 'a' THEN 1 END");
        assert!(simple.starts_with("CASE WHEN CASE WHEN"), "{simple}");
        let between = lowered("n BETWEEN 'a' AND 'b'");
        assert!(
            between.ends_with("ELSE n BETWEEN 'a' AND 'b' END"),
            "{between}"
        );
        let list = lowered("n NOT IN ('a', 'b')");
        assert!(
            list.contains("NOT IN (__msduck_unicode_order_key(__msduck_unicode_input('a'))"),
            "{list}"
        );
    }

    #[test]
    fn marked_operations_compare_text_without_dispatch() {
        assert_eq!(
            lowered("__msduck_unicode_value(n) < __msduck_unicode_value($1)"),
            "__msduck_unicode_order_key(__msduck_unicode_operand(n)) < __msduck_unicode_order_key(__msduck_unicode_operand($1))"
        );
        assert_eq!(
            lowered("__msduck_unicode_value(n) LIKE __msduck_unicode_value(p)"),
            "__msduck_unicode_like(__msduck_unicode_operand(n), __msduck_unicode_operand(p))"
        );
        let member = lowered("__msduck_unicode_in(n) NOT IN (SELECT v FROM t)");
        assert!(member.starts_with("NOT ((CASE WHEN EXISTS (SELECT 1 FROM (SELECT v FROM t) AS __msduck_in (__msduck_v) WHERE __msduck_unicode_order_key(__msduck_unicode_operand(n)) = __msduck_unicode_order_key(__msduck_unicode_operand(__msduck_in.__msduck_v))) THEN true"), "{member}");
    }

    #[test]
    fn character_conversions_read_carrier_units() {
        assert_eq!(
            lowered("__msduck_cast_nvarchar(n, 5)"),
            "CASE WHEN typeof(n) IN ('STRUCT(__msduck_utf16le BLOB)') \
             THEN __msduck_unicode_text(__msduck_cast_carrier_nvarchar(__msduck_unicode_input(n), 5)) \
             ELSE __msduck_cast_nvarchar(n, 5) END"
        );
        // Feeding a carrier consumer, the conversion stays a carrier.
        assert_eq!(
            lowered("__msduck_carrier_input(__msduck_cast_nvarchar(n, 5))"),
            "CASE WHEN typeof(n) IN ('STRUCT(__msduck_utf16le BLOB)') \
             THEN __msduck_cast_carrier_nvarchar(__msduck_unicode_input(n), 5) \
             ELSE __msduck_carrier_input(__msduck_cast_nvarchar(n, 5)) END"
        );
        assert!(
            lowered("__msduck_cast_varchar(n, 5)")
                .contains("THEN __msduck_cast_carrier_varchar(__msduck_unicode_input(n), 5)")
        );
        assert!(
            lowered("CAST(n AS VARCHAR)")
                .contains("THEN __msduck_unicode_text(__msduck_unicode_input(n))")
        );
        let concat = lowered("CONCAT(n, '-', 1)");
        assert!(concat.starts_with("CONCAT(CASE WHEN typeof(n)"), "{concat}");
        assert!(
            concat.ends_with("ELSE CAST(n AS VARCHAR) END, '-', 1)"),
            "{concat}"
        );
        let replace = lowered("REPLACE(n, 'a', 'b')");
        assert!(
            replace.starts_with("REPLACE(CASE WHEN typeof(n)"),
            "{replace}"
        );
        let substring = lowered("SUBSTRING(n, x, 2)");
        assert!(
            substring.ends_with("ELSE CAST(n AS VARCHAR) END, x, 2)"),
            "{substring}"
        );
    }

    #[test]
    fn order_markers_sort_carriers_by_key_and_others_by_value() {
        assert_eq!(
            lowered("__msduck_unicode_sort(n)"),
            "CASE WHEN typeof(n) IN ('STRUCT(__msduck_utf16le BLOB)') THEN __msduck_unicode_order_key(__msduck_unicode_input(n)) ELSE NULL END"
        );
        assert_eq!(
            lowered("__msduck_unicode_tie(n)"),
            "CASE WHEN typeof(n) IN ('STRUCT(__msduck_utf16le BLOB)') THEN NULL ELSE n END"
        );
    }
}
