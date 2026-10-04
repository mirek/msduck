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
/// Marker of an operand of unknown type placed by [`super::mark`]: only
/// these, and expressions that can produce carriers, get a `typeof`
/// dispatch.
pub(super) const MAYBE: &str = "__msduck_unicode_maybe";
/// Grouping key marker placed by [`super::group`].
pub(super) const GROUP: &str = "__msduck_unicode_group";
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

/// Whether `expr` contains a dispatch made here, outside subqueries.
/// Dispatching again over it would copy it into every branch, which grows
/// exponentially with nesting.
fn lowered(expr: &Expr) -> bool {
    struct Find(usize);
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.0 += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.0 -= 1;
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
            if self.0 == 0 && own(expr) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    expr.visit(&mut Find(0)).is_break()
}

/// How far the value-path checks below look into nested CASE results and
/// passed-through arguments. They run for every node the translator
/// lowers, so they must not walk whole subtrees.
const REACH: usize = 12;

/// Whether a dispatch made here produces `expr`'s value, directly or as a
/// CASE result or passed-through argument. Conditions do not count.
fn lowered_value(expr: &Expr) -> bool {
    fn at(expr: &Expr, reach: usize) -> bool {
        if reach == 0 {
            // Unknown that deep: assume so, which only skips a conversion.
            return true;
        }
        match expr {
            Expr::Nested(e) => at(e, reach - 1),
            Expr::Case {
                conditions,
                else_result,
                ..
            } => {
                own(expr)
                    || conditions.iter().any(|c| at(&c.result, reach - 1))
                    || else_result.as_deref().is_some_and(|e| at(e, reach - 1))
            }
            Expr::Function(_) if passes(expr) => {
                arguments(expr).is_some_and(|args| args.iter().any(|a| at(a, reach - 1)))
            }
            _ => false,
        }
    }
    at(expr, REACH)
}

/// Functions whose result may be one of their arguments.
fn passes(expr: &Expr) -> bool {
    function_name(expr).is_some_and(|name| {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "coalesce"
                | "ifnull"
                | "nullif"
                | "greatest"
                | "least"
                | "min"
                | "max"
                | "first"
                | "last"
                | "any_value"
                | "arg_min"
                | "arg_max"
                | "__msduck_isnull"
                | "__msduck_isnull_subquery"
                | "__msduck_isnull_nvarchar_width"
                | "__msduck_isnull_nchar_width"
        )
    })
}

/// Whether a conversion of `value` dispatches on its type. A value that a
/// dispatch here already produced is text; converting it again would copy
/// it into every branch.
fn converted(value: &Expr) -> bool {
    carries(value) && !lowered_value(value)
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

/// Whether a backend value may be a Unicode carrier, for conversions:
/// column references, subqueries, marked operands, carrier-producing
/// functions, and CASE results or passed-through arguments that may be.
fn carries(expr: &Expr) -> bool {
    may_be_carrier(expr, true)
}

/// Whether a predicate operand may be a carrier. Plain column references
/// and subqueries are not: the first stage marks the columns whose type it
/// cannot tell, and predicates over subqueries are not dispatched.
fn narrow(expr: &Expr) -> bool {
    may_be_carrier(expr, false)
}

fn may_be_carrier(expr: &Expr, columns: bool) -> bool {
    fn at(expr: &Expr, columns: bool, reach: usize) -> bool {
        if reach == 0 {
            return false;
        }
        match expr {
            Expr::Nested(e) => at(e, columns, reach - 1),
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Subquery(_) => columns,
            // This module's dispatches give text, keys or booleans.
            Expr::Case {
                conditions,
                else_result,
                ..
            } => {
                !own(expr)
                    && (conditions.iter().any(|c| at(&c.result, columns, reach - 1))
                        || else_result
                            .as_deref()
                            .is_some_and(|e| at(e, columns, reach - 1)))
            }
            Expr::Cast { data_type, .. } => {
                matches!(data_type, DataType::Struct(..))
                    || data_type.to_string().eq_ignore_ascii_case(CARRIER)
            }
            Expr::Function(f) => {
                let name = f.name.to_string().to_ascii_lowercase();
                if name == MARK || name == MAYBE {
                    return true;
                }
                if [ORDER_KEY, TEXT, LIKE, MEMBER, INPUT, OPERAND].contains(&name.as_str()) {
                    return false;
                }
                if passes(expr) {
                    return arguments(expr)
                        .is_some_and(|args| args.iter().any(|a| at(a, columns, reach - 1)));
                }
                if let Some(element) = observed(expr) {
                    return at(element, columns, reach - 1);
                }
                name.starts_with("__msduck_")
                    && ["unicode", "carrier", "json"]
                        .iter()
                        .any(|k| name.contains(k))
            }
            _ => false,
        }
    }
    at(expr, columns, REACH)
}

/// An operand without its marker.
fn bare(expr: &Expr) -> Expr {
    match expr {
        Expr::Nested(inner) if function_name(inner).as_deref() == Some(MAYBE) => bare(inner),
        Expr::Function(_) if function_name(expr).as_deref() == Some(MAYBE) => arguments(expr)
            .and_then(|args| args.first().map(|v| (*v).clone()))
            .unwrap_or_else(|| expr.clone()),
        _ => expr.clone(),
    }
}

fn collated(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(e) => collated(e),
        Expr::Collate { .. } => true,
        _ => observed(expr).is_some_and(collated),
    }
}

/// The value inside the NULL-observing wrapper aggregate arguments get:
/// `list_extract(list_transform([value], …), 1)`.
fn observed(expr: &Expr) -> Option<&Expr> {
    if function_name(expr).as_deref() != Some("list_extract") {
        return None;
    }
    let args = arguments(expr)?;
    if function_name(args.first()?).as_deref() != Some("list_transform") {
        return None;
    }
    match arguments(args[0])?.first().copied()? {
        Expr::Array(array) => match array.elem.as_slice() {
            [element] => Some(element),
            _ => None,
        },
        _ => None,
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

/// An operand as a carrier, failing for types that are not text.
fn strip(expr: &Expr) -> Expr {
    if marked(expr)
        && let Some(args) = arguments(expr)
        && let [value] = args.as_slice()
    {
        return call(OPERAND, vec![(*value).clone()]);
    }
    call(OPERAND, vec![bare(expr)])
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
    operands.iter().any(|o| narrow(o))
        && !operands
            .iter()
            .any(|o| has_subquery(o) || collated(o) || is_typeof(o) || lowered(o))
}

/// The rewrite of one comparison, or `None` when it needs none.
fn compare(left: &Expr, op: &BinaryOperator, right: &Expr) -> Option<Expr> {
    // Width adapters return Unicode but may contain a scalar query. For
    // text peers, key each operand once without duplicating that query in a
    // typeof CASE. Numeric/unknown peers retain the old backend coercion
    // through text; passing an integer to a Unicode key would reject it.
    let unicode_width = |value: &Expr| {
        function_name(value).is_some_and(|name| {
            matches!(
                name.as_str(),
                "__msduck_isnull_nvarchar_width" | "__msduck_isnull_nchar_width"
            )
        })
    };
    fn text_peer(value: &Expr) -> bool {
        match value {
            Expr::Nested(inner) | Expr::Collate { expr: inner, .. } => text_peer(inner),
            Expr::Value(v) => matches!(
                v.value,
                Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)
            ),
            Expr::Cast {
                data_type: DataType::Varchar(_) | DataType::Text | DataType::String(_),
                ..
            } => true,
            Expr::Function(f) => matches!(
                f.name.to_string().as_str(),
                MARK | TEXT
                    | INPUT
                    | OPERAND
                    | CARRIER_INPUT
                    | "__msduck_pack_unicode"
                    | "__msduck_unicode_from_le"
                    | "__msduck_concat_unicode"
                    | "__msduck_cast_carrier_nvarchar"
                    | "__msduck_cast_carrier_nchar"
                    | "__msduck_isnull_nvarchar_width"
                    | "__msduck_isnull_nchar_width"
            ),
            // MAYBE and generic names containing "unicode" are not type
            // declarations: e.g. __msduck_unicode itself returns an integer.
            _ => false,
        }
    }
    let width_text =
        unicode_width(left) && text_peer(right) || unicode_width(right) && text_peer(left);
    if comparison(op) && (marked(left) || marked(right) || width_text) {
        return Some(Expr::BinaryOp {
            left: Box::new(strict_key(left)),
            op: op.clone(),
            right: Box::new(strict_key(right)),
        });
    }
    if comparison(op) && (unicode_width(left) || unicode_width(right)) {
        let text = |value: &Expr| {
            if unicode_width(value) {
                call(TEXT, vec![input(value.clone())])
            } else {
                value.clone()
            }
        };
        return Some(Expr::BinaryOp {
            left: Box::new(text(left)),
            op: op.clone(),
            right: Box::new(text(right)),
        });
    }
    if !comparison(op) || !dispatched(&[left, right]) {
        return None;
    }
    let (left, right) = (bare(left), bare(right));
    Some(case(
        textual(&[&left, &right]),
        Expr::BinaryOp {
            left: Box::new(strict_key(&left)),
            op: op.clone(),
            right: Box::new(strict_key(&right)),
        },
        Expr::BinaryOp {
            left: Box::new(left),
            op: op.clone(),
            right: Box::new(right),
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
            let (value, pattern) = (bare(value), bare(pattern));
            let mut operands = vec![value.clone(), pattern.clone()];
            let mut args = vec![strip(&value), strip(&pattern)];
            if let Some(escape) = escape_char {
                operands.push(*escape.clone());
                args.push(strip(escape));
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
                textual(&operands.iter().collect::<Vec<_>>()),
                not(call(LIKE, args), *negated),
                Expr::ILike {
                    negated: *negated,
                    any: false,
                    expr: Box::new(varchar(&value)),
                    pattern: Box::new(varchar(&pattern)),
                    escape_char: escape_char.as_deref().map(|e| Box::new(varchar(e))),
                },
            ))
        }
        // Other LIKE matches case-insensitively, as under the database's
        // default collation; an explicit COLLATE keeps DuckDB's LIKE.
        Expr::Like {
            negated,
            any: false,
            expr: value,
            pattern,
            escape_char,
        } if !collated(value) && !collated(pattern) => Some(Expr::ILike {
            negated: *negated,
            any: false,
            expr: value.clone(),
            pattern: pattern.clone(),
            escape_char: escape_char.clone(),
        }),
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
        } if dispatched(&[value]) && !list.iter().any(has_subquery) => {
            let value = bare(value);
            Some(case(
                textual(&[&value]),
                Expr::InList {
                    expr: Box::new(strict_key(&value)),
                    list: list.iter().map(strict_key).collect(),
                    negated: *negated,
                },
                Expr::InList {
                    expr: Box::new(value.clone()),
                    list: list.clone(),
                    negated: *negated,
                },
            ))
        }
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
        } if dispatched(&[value, low, high]) => {
            let (value, low, high) = (bare(value), bare(low), bare(high));
            Some(case(
                textual(&[&value, &low, &high]),
                Expr::Between {
                    expr: Box::new(strict_key(&value)),
                    negated: *negated,
                    low: Box::new(strict_key(&low)),
                    high: Box::new(strict_key(&high)),
                },
                Expr::Between {
                    expr: Box::new(value),
                    negated: *negated,
                    low: Box::new(low),
                    high: Box::new(high),
                },
            ))
        }
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
        } if converted(value) => Some(case(
            type_in(value, &[CARRIER]),
            call(TEXT, vec![input(*value.clone())]),
            expr.clone(),
        )),
        Expr::Substring { expr: value, .. } if converted(value) => {
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

/// `count(DISTINCT x)` and `min`/`max` over character data, under the
/// case-insensitive default: DISTINCT counts compare the sort keys of text
/// (DuckDB's DISTINCT ignores collations), and extrema of carriers take the
/// value with the least or greatest key (DuckDB orders the STRUCT by its
/// bytes). VARCHAR extrema follow the column's DuckDB collation.
fn aggregate(name: &str, expr: &Expr) -> Option<Expr> {
    let Expr::Function(f) = expr else {
        return None;
    };
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    if f.over.is_some()
        || f.filter.is_some()
        || !f.within_group.is_empty()
        || !list.clauses.is_empty()
    {
        return None;
    }
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = list.args.as_slice() else {
        return None;
    };
    if collated(value) {
        return None;
    }
    let distinct = matches!(list.duplicate_treatment, Some(DuplicateTreatment::Distinct));
    let with = |name: &str, args: Vec<Expr>| {
        let mut lowered = expr.clone();
        if let Expr::Function(f) = &mut lowered {
            let extremum = matches!(name.to_ascii_lowercase().as_str(), "min" | "max");
            f.name = ObjectName::from(vec![Ident::new(name)]);
            if let FunctionArguments::List(list) = &mut f.args {
                if extremum {
                    list.duplicate_treatment = None;
                }
                list.args = args
                    .into_iter()
                    .map(|arg| FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)))
                    .collect();
            }
        }
        lowered
    };
    // The type test reads the value through an aggregate, so that it binds
    // where the value itself may not appear (grouped queries).
    let sample = call("any_value", vec![value.clone()]);
    match name.to_ascii_lowercase().as_str() {
        // Carriers count Unicode keys; VARCHAR (where no unit is ignored)
        // counts its lowercased text without trailing spaces.
        "count" if distinct => Some(Expr::Case {
            case_token: AttachedToken::empty(),
            end_token: AttachedToken::empty(),
            operand: None,
            conditions: vec![
                CaseWhen {
                    condition: type_in(&sample, &[CARRIER]),
                    result: with("count", vec![key(value.clone())]),
                },
                CaseWhen {
                    condition: type_in(&sample, &["VARCHAR"]),
                    result: with("count", vec![ansi_key(value.clone())]),
                },
            ],
            else_result: Some(Box::new(expr.clone())),
        }),
        // The extremum of (key, value) pairs; NULL values stay NULL so the
        // aggregate (and the NULL-elimination warning) still sees them.
        // DISTINCT cannot change an extremum.
        "min" | "max" if converted(value) => {
            let pair = keyed_pair(value)?;
            Some(case(
                Expr::BinaryOp {
                    left: Box::new(call("typeof", vec![sample])),
                    op: BinaryOperator::Eq,
                    right: Box::new(text(CARRIER)),
                },
                call(
                    "struct_extract",
                    vec![with(&f.name.to_string(), vec![pair]), text("__msduck_v")],
                ),
                expr.clone(),
            ))
        }
        _ => None,
    }
}

/// The equality key of VARCHAR text: lowercased, without trailing spaces.
/// The trim binds for every type (`__msduck_collation_key`), so it can sit in
/// a `typeof` branch that DuckDB binds for other types too.
fn ansi_key(value: Expr) -> Expr {
    call("lower", vec![call("__msduck_collation_key", vec![value])])
}

/// `{key, value}` of a carrier for MIN/MAX, NULL for NULL, evaluating the
/// value once per row (it may be volatile).
fn keyed_pair(value: &Expr) -> Option<Expr> {
    const TEMPLATE: &str = "list_extract(list_transform([__msduck_value], __msduck_m -> \
        CASE WHEN __msduck_m IS NULL THEN NULL ELSE {'__msduck_k': __msduck_key, '__msduck_v': __msduck_m} END), 1)";
    let mut pair = sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
        .try_with_sql(TEMPLATE)
        .ok()?
        .parse_expr()
        .ok()?;
    let element = Expr::Identifier(Ident::new("__msduck_m"));
    let keyed = key(element);
    struct Fill<'a> {
        value: &'a Expr,
        key: &'a Expr,
    }
    impl VisitorMut for Fill<'_> {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::Identifier(ident) = expr {
                match ident.value.as_str() {
                    "__msduck_value" => *expr = self.value.clone(),
                    "__msduck_key" => *expr = self.key.clone(),
                    _ => {}
                }
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(&mut pair, &mut Fill { value, key: &keyed });
    Some(pair)
}

fn function(name: &str, expr: &Expr) -> Option<Expr> {
    if let Some(lowered) = aggregate(name, expr) {
        return Some(lowered);
    }
    let args = arguments(expr)?;
    if let Some((cast, unicode)) = carrier_cast(name)
        && let [value, width] = args.as_slice()
        && converted(value)
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
        // VARCHAR groups by its lowercased text without trailing spaces (no
        // unit is ignored); carriers by their Unicode key.
        GROUP => {
            let [value] = args.as_slice() else {
                return None;
            };
            Some(case(
                type_in(value, &["VARCHAR"]),
                call("encode", vec![ansi_key((*value).clone())]),
                key((*value).clone()),
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
                |(i, a): (usize, &&Expr)| positions.is_none_or(|p| p.contains(&i)) && converted(a);
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
    // The subquery becomes a one-column derived table, whatever its shape.
    const TEMPLATE: &str = "CASE WHEN EXISTS (SELECT 1 FROM (SELECT 1) AS __msduck_in(__msduck_v) WHERE __msduck_match) THEN true \
        WHEN NOT EXISTS (SELECT 1 FROM (SELECT 1) AS __msduck_in(__msduck_v)) THEN false \
        WHEN __msduck_value IS NULL OR EXISTS (SELECT 1 FROM (SELECT 1) AS __msduck_in(__msduck_v) WHERE __msduck_in.__msduck_v IS NULL) THEN NULL \
        ELSE false END";
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

/// How deep below the expression the translator hands over [`lower`] looks.
/// The translator lowers every node bottom-up, so deeper nodes were lowered
/// already; only what a built-in lowering of this node created is new. The
/// deepest such shape seen is NULLIF's `CASE WHEN a = b THEN NULL ELSE a
/// END` (the comparison two levels down); IN and BETWEEN are not expanded. Bounding the
/// walk keeps lowering linear in the size of deeply nested expressions.
const DEPTH: usize = 4;

/// Lower the carrier operations in `expr` and the nodes just below it,
/// outside nested queries (the translator lowers those separately) and
/// outside earlier dispatches.
pub(super) fn lower(expr: &mut Expr) {
    walk(expr, 1);
}

fn walk(expr: &mut Expr, depth: usize) {
    if depth > DEPTH || dispatch(expr) {
        return;
    }
    for child in children(expr) {
        walk(child, depth + 1);
    }
    rewrite(expr);
}

/// The operands of the expressions the rewrites look at, and of those that
/// built-in lowerings create around them. Nested queries are lowered by the
/// translator separately.
fn children(expr: &mut Expr) -> Vec<&mut Expr> {
    match expr {
        Expr::Nested(e)
        | Expr::UnaryOp { expr: e, .. }
        | Expr::Cast { expr: e, .. }
        | Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsFalse(e)
        | Expr::Collate { expr: e, .. } => vec![e.as_mut()],
        Expr::BinaryOp { left, right, .. } => vec![left.as_mut(), right.as_mut()],
        Expr::Like {
            expr,
            pattern,
            escape_char,
            ..
        } => {
            let mut all = vec![expr.as_mut(), pattern.as_mut()];
            all.extend(escape_char.as_deref_mut());
            all
        }
        Expr::Between {
            expr, low, high, ..
        } => vec![expr.as_mut(), low.as_mut(), high.as_mut()],
        Expr::InList { expr, list, .. } => {
            let mut all = vec![expr.as_mut()];
            all.extend(list.iter_mut());
            all
        }
        Expr::InSubquery { expr, .. } => vec![expr.as_mut()],
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            let mut all = vec![expr.as_mut()];
            all.extend(substring_from.as_deref_mut());
            all.extend(substring_for.as_deref_mut());
            all
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let mut all: Vec<&mut Expr> = operand.as_deref_mut().into_iter().collect();
            for c in conditions.iter_mut() {
                all.push(&mut c.condition);
                all.push(&mut c.result);
            }
            all.extend(else_result.as_deref_mut());
            all
        }
        Expr::Function(f) => match &mut f.args {
            FunctionArguments::List(list) => list
                .args
                .iter_mut()
                .filter_map(|arg| match arg {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                    | FunctionArg::Named {
                        arg: FunctionArgExpr::Expr(e),
                        ..
                    } => Some(e),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
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

    /// Lower like the translator: every node, bottom-up.
    fn translate(value: &mut Expr) {
        struct Translate;
        impl VisitorMut for Translate {
            type Break = ();
            fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                lower(expr);
                ControlFlow::Continue(())
            }
        }
        let _ = VisitMut::visit(value, &mut Translate);
    }

    fn lowered(sql: &str) -> String {
        let mut value = expr(sql);
        translate(&mut value);
        // Lowering is idempotent: ancestors lower the nodes below them again.
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
            "x COLLATE \"C\" = 'a'",
            "typeof(x) = 'VARCHAR'",
        ] {
            assert_eq!(lowered(sql), expr(sql).to_string(), "{sql}");
        }
    }

    #[test]
    fn other_like_matches_case_insensitively() {
        assert_eq!(lowered("n LIKE 'a%'"), "n ILIKE 'a%'");
        assert_eq!(
            lowered("(SELECT y FROM t) NOT LIKE 'a%'"),
            "(SELECT y FROM t) NOT ILIKE 'a%'"
        );
        assert_eq!(
            lowered("n COLLATE \"C\" LIKE 'a%'"),
            "n COLLATE \"C\" LIKE 'a%'"
        );
    }

    #[test]
    fn text_aggregates_compare_keys() {
        let count = lowered("count(DISTINCT n)");
        assert_eq!(
            count,
            "CASE WHEN typeof(any_value(n)) IN ('STRUCT(__msduck_utf16le BLOB)') \
             THEN count(DISTINCT __msduck_unicode_order_key(__msduck_unicode_input(n))) \
             WHEN typeof(any_value(n)) IN ('VARCHAR') THEN count(DISTINCT lower(__msduck_collation_key(n))) \
             ELSE count(DISTINCT n) END"
        );
        let least = lowered("min(n)");
        assert_eq!(
            least,
            "CASE WHEN typeof(any_value(n)) = 'STRUCT(__msduck_utf16le BLOB)' \
             THEN struct_extract(min(list_extract(list_transform([n], __msduck_m -> CASE WHEN __msduck_m IS NULL THEN NULL ELSE {'__msduck_k': __msduck_unicode_order_key(__msduck_unicode_input(__msduck_m)), '__msduck_v': __msduck_m} END), 1)), '__msduck_v') \
             ELSE min(n) END"
        );
        // Window extrema and explicitly collated values keep DuckDB's.
        assert_eq!(lowered("max(n) OVER ()"), "max(n) OVER ()");
        assert_eq!(
            lowered("count(DISTINCT n COLLATE \"C\")"),
            "count(DISTINCT n COLLATE \"C\")"
        );
        assert_eq!(
            lowered("__msduck_unicode_group(n)"),
            "CASE WHEN typeof(n) IN ('VARCHAR') THEN encode(lower(__msduck_collation_key(n))) \
             ELSE __msduck_unicode_order_key(__msduck_unicode_input(n)) END"
        );
    }

    #[test]
    fn plain_columns_are_left_alone() {
        // Outer join conditions must stay comparisons for hash joins.
        for sql in ["a.id = b.id", "n = 'x'", "n IN (1, 2)", "n BETWEEN 1 AND 2"] {
            assert_eq!(lowered(sql), expr(sql).to_string(), "{sql}");
        }
    }

    #[test]
    fn possible_carriers_dispatch_on_type_and_keep_the_original() {
        let comparison = lowered("__msduck_unicode_maybe(n) = 'x'");
        assert_eq!(
            comparison,
            "CASE WHEN (typeof(n) IN ('STRUCT(__msduck_utf16le BLOB)') OR typeof('x') IN ('STRUCT(__msduck_utf16le BLOB)')) \
             THEN __msduck_unicode_order_key(__msduck_unicode_operand(n)) = __msduck_unicode_order_key(__msduck_unicode_operand('x')) \
             ELSE n = 'x' END"
        );
        assert!(lowered("__msduck_json_value(j, 'p') < x").starts_with("CASE WHEN"));
        let like = lowered("__msduck_unicode_maybe(n) NOT LIKE 'a!%' ESCAPE '!'");
        assert!(
            like.contains("THEN NOT (__msduck_unicode_like(__msduck_unicode_operand(n), __msduck_unicode_operand('a!%'), __msduck_unicode_operand('!')))"),
            "{like}"
        );
        assert!(
            like.ends_with("ELSE CAST(n AS VARCHAR) NOT ILIKE 'a!%' ESCAPE '!' END"),
            "{like}"
        );
        let simple = lowered("CASE __msduck_unicode_maybe(n) WHEN 'a' THEN 1 END");
        assert!(simple.starts_with("CASE WHEN CASE WHEN"), "{simple}");
        let between = lowered("__msduck_unicode_maybe(n) BETWEEN 'a' AND 'b'");
        assert!(
            between.ends_with("ELSE n BETWEEN 'a' AND 'b' END"),
            "{between}"
        );
        let list = lowered("__msduck_unicode_maybe(n) NOT IN ('a', 'b')");
        assert!(
            list.contains("NOT IN (__msduck_unicode_order_key(__msduck_unicode_operand('a'))"),
            "{list}"
        );
        assert!(list.ends_with("ELSE n NOT IN ('a', 'b') END"), "{list}");
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
        assert!(
            lowered("__msduck_unicode_in(n) IN (SELECT v FROM t UNION SELECT w FROM u)").contains(
                "FROM (SELECT v FROM t UNION SELECT w FROM u) AS __msduck_in (__msduck_v)"
            )
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
    fn nesting_does_not_copy_lowered_operands() {
        let mut nested = "n".to_string();
        for _ in 0..20 {
            nested = format!(
                "CASE WHEN __msduck_unicode_maybe({nested}) = 'a' THEN __msduck_left_unicode(n, 1) ELSE m END"
            );
        }
        let mut cast = "n".to_string();
        for _ in 0..20 {
            cast = format!("__msduck_cast_nvarchar({cast}, 5)");
        }
        // Each level adds a bounded amount, not a multiple of the inner size.
        assert!(
            lowered(&nested).len() < 40 * nested.len(),
            "{}",
            lowered(&nested).len()
        );
        assert!(
            lowered(&cast).len() < 10 * cast.len(),
            "{}",
            lowered(&cast).len()
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
