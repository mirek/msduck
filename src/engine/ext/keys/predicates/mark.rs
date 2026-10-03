//! Operands known to be Unicode text before translation.
//!
//! The backend lowering ([`super::lower`]) dispatches on `typeof`, keeping
//! the original operation for operands that are not carriers. DuckDB binds
//! both branches, and refuses to bind an ordering comparison, IN or LIKE
//! between a carrier and a VARCHAR expression (only literals convert), so
//! the dispatch alone fails for `column < @variable` or `column = other`.
//! Operands that resolve to carrier columns, directly or through character
//! functions, are wrapped in a marker here; the lowering compares marked
//! operations as Unicode text without the dispatch.
use super::catalog::{Catalog, Resolution};
use super::lower::{MARK, MAYBE, MEMBER};
use crate::engine::Parameter;
use msduck_core::collation::{Conflict, Label, Operation};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::collections::HashMap;
use std::ops::ControlFlow;

type Parameters = HashMap<String, Parameter>;

fn marked(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(f) if f.name.to_string() == MARK)
}

fn maybe(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(f) if f.name.to_string() == MAYBE)
}

/// Whether `expr` is known not to be text: a column of another backend
/// type, or a number. A carrier compared with it fails either way, so it
/// needs no dispatch, and an outer join on it stays a hash join.
fn not_text(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => not_text(catalog, inner),
        Expr::Value(value) => matches!(value.value, Value::Number(..)),
        Expr::UnaryOp { expr, .. } => not_text(catalog, expr),
        Expr::Identifier(ident) if ident.value.starts_with('@') => false,
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            matches!(catalog.resolve(expr), Resolution::Plain { text: false })
        }
        _ => false,
    }
}

/// Whether `expr` is a column reference whose type is unknown here: a
/// derived-table, CTE or outer column. The backend lowering checks its
/// type; columns known not to be carriers are left alone.
fn unknown(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => unknown(catalog, inner),
        // Variables and parameters are VARCHAR text in the backend.
        Expr::Identifier(ident) if ident.value.starts_with('@') => false,
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            matches!(catalog.resolve(expr), Resolution::Unknown)
        }
        _ => false,
    }
}

fn first_argument(function: &Function) -> Option<&Expr> {
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    match list.args.first()? {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
        _ => None,
    }
}

/// Whether `expr` is Unicode text: a carrier column, a character function
/// of one (ISNULL and COALESCE take the type of their first argument), a
/// CASE whose results are such text or string literals, or a scalar
/// subquery selecting one.
fn unicode(catalog: &Catalog, expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => unicode(catalog, inner),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => catalog.carrier(expr).is_some(),
        Expr::Substring { expr, .. } | Expr::Trim { expr, .. } => unicode(catalog, expr),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            // Every result must be text: SQL Server types the CASE by
            // precedence, so `CASE … THEN n ELSE 0 END` is an int.
            let results: Vec<&Expr> = conditions
                .iter()
                .map(|c| &c.result)
                .chain(else_result.as_deref())
                .filter(|r| !matches!(r, Expr::Value(v) if matches!(v.value, Value::Null)))
                .collect();
            !results.is_empty()
                && results.iter().any(|r| unicode(catalog, r))
                && results.iter().all(|r| {
                    unicode(catalog, r)
                        || matches!(r, Expr::Value(v) if matches!(v.value,
                            Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)))
                })
        }
        Expr::Subquery(query) => match query.body.as_ref() {
            SetExpr::Select(select) => match select.projection.as_slice() {
                [SelectItem::UnnamedExpr(p) | SelectItem::ExprWithAlias { expr: p, .. }] => {
                    unicode(catalog, p)
                }
                _ => false,
            },
            _ => false,
        },
        Expr::Function(f) => {
            matches!(
                f.name.to_string().to_ascii_uppercase().as_str(),
                "LEFT"
                    | "RIGHT"
                    | "UPPER"
                    | "LOWER"
                    | "LTRIM"
                    | "RTRIM"
                    | "TRIM"
                    | "SUBSTRING"
                    | "REPLACE"
                    | "REVERSE"
                    | "ISNULL"
                    | "COALESCE"
            ) && first_argument(f).is_some_and(|a| unicode(catalog, a))
        }
        _ => false,
    }
}

fn collated(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => collated(inner),
        Expr::Collate { .. } => true,
        _ => false,
    }
}

/// What [`collate`] did with an operation.
enum Collated {
    /// No operand is a column with a rule of its own.
    No,
    /// Its column operands now carry their collation explicitly.
    Wrapped,
    /// Columns of different collations meet: their names, in operand order.
    Conflict(String, String),
}

/// Give the column operands of an operation their declared collation as an
/// explicit COLLATE, when its rule differs from the database default: SQL
/// Server's implicit column collation then wins over literals and
/// variables, and the explicit COLLATE lowering applies it. Columns of
/// different collations (by name, even when they compare alike) conflict.
fn collate(catalog: &Catalog, operands: &mut [&mut Expr]) -> Collated {
    use msduck_sql::dialect::ext::keys::collation::DEFAULT;
    let mut name: Option<String> = None;
    for operand in operands.iter() {
        if !catalog.textual(operand) {
            continue;
        }
        let found = catalog.collation(operand).unwrap_or(DEFAULT);
        match &name {
            Some(existing) if !existing.eq_ignore_ascii_case(found) => {
                return Collated::Conflict(existing.clone(), found.to_owned());
            }
            _ => name = Some(found.to_owned()),
        }
    }
    let Some(name) = name.filter(|name| own(name)) else {
        return Collated::No;
    };
    for operand in operands.iter_mut() {
        if catalog.textual(operand) && catalog.collation(operand).is_some() {
            let inner = std::mem::replace(&mut **operand, Expr::Value(Value::Null.into()));
            **operand = Expr::Collate {
                expr: Box::new(inner),
                collation: ObjectName::from(vec![Ident::new(name.clone())]),
            };
        }
    }
    Collated::Wrapped
}

/// The collation operation of a comparison operator.
fn operation(op: &BinaryOperator) -> Option<Operation> {
    Some(match op {
        BinaryOperator::Eq => Operation::Equal,
        BinaryOperator::NotEq => Operation::NotEqual,
        BinaryOperator::Lt => Operation::Less,
        BinaryOperator::Gt => Operation::Greater,
        BinaryOperator::LtEq => Operation::LessEqual,
        BinaryOperator::GtEq => Operation::GreaterEqual,
        _ => return None,
    })
}

/// Whether a collation compares differently from the database default.
fn own(name: &str) -> bool {
    use msduck_sql::dialect::ext::keys::collation::{Sensitivity, sensitivity};
    sensitivity(name) == Some(Sensitivity::Other)
}

/// The column's collation when it compares differently from the default.
fn own_collation<'a>(catalog: &'a Catalog, expr: &Expr) -> Option<&'a str> {
    catalog.collation(expr).filter(|name| own(name))
}

/// Whether `expr` is known to be character data: a text column, a string
/// literal, a character variable or parameter, or a character function or
/// conversion of such text.
fn text(catalog: &Catalog, parameters: &Parameters, expr: &Expr) -> bool {
    use msduck_core::types::Type;
    match expr {
        Expr::Nested(inner) => text(catalog, parameters, inner),
        Expr::Value(value) => matches!(
            value.value,
            Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)
        ),
        Expr::Identifier(ident) if ident.value.starts_with('@') => parameters
            .get(&ident.value.to_lowercase())
            .is_some_and(|parameter| {
                matches!(
                    parameter.data_type,
                    Type::Character(_) | Type::Text | Type::Ntext
                )
            }),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => catalog.textual(expr),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } => {
            matches!(
                msduck_sql::sql_type::declaration(data_type),
                Ok(Type::Character(_) | Type::Text | Type::Ntext)
            )
        }
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_uppercase();
            name == "CONCAT"
                || matches!(
                    name.as_str(),
                    "LEFT"
                        | "RIGHT"
                        | "UPPER"
                        | "LOWER"
                        | "LTRIM"
                        | "RTRIM"
                        | "TRIM"
                        | "SUBSTRING"
                        | "REPLACE"
                        | "REVERSE"
                        | "ISNULL"
                        | "COALESCE"
                ) && first_argument(f).is_some_and(|a| text(catalog, parameters, a))
        }
        _ => unicode(catalog, expr),
    }
}

/// Whether `expr` is known to be non-Unicode character data: a CHAR or
/// VARCHAR column, a string literal without N, or such a variable.
fn ansi(catalog: &Catalog, parameters: &Parameters, expr: &Expr) -> bool {
    use msduck_core::character::Family;
    use msduck_core::types::Type;
    match expr {
        Expr::Nested(inner) => ansi(catalog, parameters, inner),
        Expr::Value(value) => matches!(value.value, Value::SingleQuotedString(_)),
        Expr::Identifier(ident) if ident.value.starts_with('@') => parameters
            .get(&ident.value.to_lowercase())
            .is_some_and(|parameter| {
                matches!(parameter.data_type, Type::Text)
                    || matches!(parameter.data_type, Type::Character(c)
                        if matches!(c.family(), Family::Char | Family::Varchar))
            }),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => catalog.ansi(expr),
        _ => false,
    }
}

/// Mark the operands of one operation when any of them is Unicode text;
/// otherwise mark operands of unknown type for the backend `typeof`
/// dispatch. Explicitly collated operations are left to the COLLATE
/// handling.
fn mark(
    catalog: &Catalog,
    operation: Option<Operation>,
    mut operands: Vec<&mut Expr>,
) -> Result<(), SqlError> {
    if operands.iter().any(|o| collated(o) || marked(o)) {
        return Ok(());
    }
    match collate(catalog, &mut operands) {
        Collated::No => {}
        Collated::Wrapped => return Ok(()),
        // SQL Server's 468; binding reports it for queries, but DML
        // predicates would otherwise compare the raw values.
        Collated::Conflict(left, right) => {
            if let Some(operation) = operation
                && let Err(Conflict::Operation(error)) =
                    operation.resolve(&[Label::Implicit(left), Label::Implicit(right)])
            {
                return Err(error);
            }
            return Ok(());
        }
    }
    // A subquery pass can type a column the statement pass could not.
    let unwrap = |e: &Expr| -> Expr {
        match e {
            Expr::Function(f) if maybe(e) => match &f.args {
                FunctionArguments::List(list) => match list.args.as_slice() {
                    [FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))] => inner.clone(),
                    _ => e.clone(),
                },
                _ => e.clone(),
            },
            _ => e.clone(),
        }
    };
    let text = operands.iter().any(|o| unicode(catalog, &unwrap(o)));
    let typed = operands.iter().any(|o| not_text(catalog, o));
    for operand in operands {
        let name = if text {
            MARK
        } else if !typed && !maybe(operand) && unknown(catalog, operand) {
            MAYBE
        } else {
            continue;
        };
        let inner = unwrap(operand);
        *operand = msduck_sql::expr::unary_function(name, inner);
    }
    Ok(())
}

/// Mark one comparison, IN, BETWEEN, simple CASE or LIKE whose operands are
/// all literals, variables or other character data known without a
/// catalog, so it compares under the case-insensitive default collation.
/// Statements and scalar evaluations alike reach this for every node.
pub(super) fn literals(parameters: &Parameters, expr: &mut Expr) {
    let catalog = Catalog::default();
    // NULLIF(a, b) becomes CASE WHEN a = b …; marking `b` alone keeps `a`,
    // which gives the result its type, as written.
    if let Expr::Function(f) = expr
        && f.name.to_string().eq_ignore_ascii_case("NULLIF")
    {
        if let FunctionArguments::List(list) = &mut f.args
            && let [
                FunctionArg::Unnamed(FunctionArgExpr::Expr(first)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(second)),
            ] = list.args.as_mut_slice()
            && [&*first, &*second]
                .iter()
                .all(|o| !collated(o) && !marked(o) && text(&catalog, parameters, o))
        {
            let inner = std::mem::replace(second, Expr::Value(Value::Null.into()));
            *second = msduck_sql::expr::unary_function(MARK, inner);
        }
        return;
    }
    let like = matches!(expr, Expr::Like { any: false, .. });
    let operands: Vec<&mut Expr> = match expr {
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::Gt
                | BinaryOperator::LtEq
                | BinaryOperator::GtEq,
            right,
        } => vec![left, right],
        Expr::Between {
            expr, low, high, ..
        } => vec![expr, low, high],
        Expr::Like {
            expr,
            pattern,
            any: false,
            ..
        } => vec![expr, pattern],
        Expr::InList { expr, list, .. } => {
            let mut operands = vec![expr.as_mut()];
            operands.extend(list.iter_mut());
            operands
        }
        Expr::Case {
            operand: Some(operand),
            conditions,
            ..
        } => {
            let mut operands = vec![operand.as_mut()];
            operands.extend(conditions.iter_mut().map(|c| &mut c.condition));
            operands
        }
        _ => return,
    };
    if !operands
        .iter()
        .all(|o| !collated(o) && !marked(o) && text(&catalog, parameters, o))
    {
        return;
    }
    // ASCII pattern matching ignores the value's trailing blanks (the
    // pattern's count); Unicode matching keeps them.
    let ascii = like && operands.iter().all(|o| ansi(&catalog, parameters, o));
    for (index, operand) in operands.into_iter().enumerate() {
        let mut inner = std::mem::replace(operand, Expr::Value(Value::Null.into()));
        if ascii && index == 0 {
            inner = msduck_sql::expr::unary_function("RTRIM", inner);
        }
        *operand = msduck_sql::expr::unary_function(MARK, inner);
    }
}

pub(super) fn rewrite<T: VisitMut>(
    catalog: &Catalog,
    parameters: &Parameters,
    node: &mut T,
) -> Result<(), SqlError> {
    struct Mark<'a>(&'a Catalog, &'a Parameters);
    impl VisitorMut for Mark<'_> {
        type Break = SqlError;
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<SqlError> {
            let result = match expr {
                Expr::BinaryOp { left, op, right } => match operation(op) {
                    Some(operation) => mark(self.0, Some(operation), vec![left, right]),
                    None => Ok(()),
                },
                Expr::Between {
                    expr, low, high, ..
                } => mark(self.0, Some(Operation::GreaterEqual), vec![expr, low, high]),
                // The escape stays as written: its checks look for a literal.
                // LIKE over known character data matches with SQL Server's
                // Unicode LIKE under the case-insensitive default.
                Expr::Like {
                    expr,
                    pattern,
                    any: false,
                    ..
                } => {
                    if [&**expr, &**pattern]
                        .iter()
                        .all(|o| !collated(o) && !marked(o) && text(self.0, self.1, o))
                        && own_collation(self.0, expr).is_none()
                        && own_collation(self.0, pattern).is_none()
                    {
                        // ASCII pattern matching ignores the value's
                        // trailing blanks (the pattern's count); Unicode
                        // matching keeps them.
                        let ascii = [&**expr, &**pattern]
                            .iter()
                            .all(|o| ansi(self.0, self.1, o));
                        for (index, operand) in [expr, pattern].into_iter().enumerate() {
                            let mut inner = std::mem::replace(
                                operand.as_mut(),
                                Expr::Value(Value::Null.into()),
                            );
                            if ascii && index == 0 {
                                inner = msduck_sql::expr::unary_function("RTRIM", inner);
                            }
                            **operand = msduck_sql::expr::unary_function(MARK, inner);
                        }
                        Ok(())
                    } else {
                        mark(self.0, None, vec![expr, pattern])
                    }
                }
                Expr::InList { expr, list, .. } => {
                    let mut operands = vec![expr.as_mut()];
                    operands.extend(list.iter_mut());
                    mark(self.0, Some(Operation::Equal), operands)
                }
                Expr::Case {
                    operand: Some(operand),
                    conditions,
                    ..
                } => {
                    let mut operands = vec![operand.as_mut()];
                    operands.extend(conditions.iter_mut().map(|c| &mut c.condition));
                    mark(self.0, Some(Operation::Equal), operands)
                }
                // DISTINCT counts and extrema of a column with a collation
                // of its own keep it (explicitly), instead of the default's
                // keys.
                Expr::Function(f)
                    if matches!(
                        f.name.to_string().to_ascii_uppercase().as_str(),
                        "COUNT" | "COUNT_BIG" | "MIN" | "MAX"
                    ) =>
                {
                    let count = f.name.to_string().to_ascii_uppercase().starts_with("COUNT");
                    if let FunctionArguments::List(list) = &mut f.args
                        && (!count
                            || matches!(
                                list.duplicate_treatment,
                                Some(DuplicateTreatment::Distinct)
                            ))
                        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] =
                            list.args.as_mut_slice()
                        && let Some(name) = own_collation(self.0, value).map(str::to_owned)
                        // Binding gives Latin1_General_100_BIN2 extrema their
                        // own lossless UTF-16 path.
                        && (count || !name.eq_ignore_ascii_case("Latin1_General_100_BIN2"))
                    {
                        let inner = std::mem::replace(value, Expr::Value(Value::Null.into()));
                        *value = Expr::Collate {
                            expr: Box::new(inner),
                            collation: ObjectName::from(vec![Ident::new(name)]),
                        };
                    }
                    Ok(())
                }
                // NULLIF(a, b) becomes CASE WHEN a = b …; marking `b` alone
                // keeps `a`, which gives the result its type, as written.
                Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("NULLIF") => {
                    if let FunctionArguments::List(list) = &mut f.args
                        && let [
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(first)),
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(second)),
                        ] = list.args.as_mut_slice()
                        && !collated(first)
                        && !collated(second)
                        && !marked(second)
                        && unicode(self.0, first)
                    {
                        let inner = std::mem::replace(second, Expr::Value(Value::Null.into()));
                        *second = msduck_sql::expr::unary_function(MARK, inner);
                    }
                    Ok(())
                }
                Expr::InSubquery {
                    expr: value,
                    subquery,
                    ..
                } => {
                    let projected = match subquery.body.as_ref() {
                        SetExpr::Select(select) => match select.projection.as_slice() {
                            [
                                SelectItem::UnnamedExpr(p)
                                | SelectItem::ExprWithAlias { expr: p, .. },
                            ] => Some(p),
                            _ => None,
                        },
                        _ => None,
                    };
                    let member =
                        matches!(value.as_ref(), Expr::Function(f) if f.name.to_string() == MEMBER);
                    if !member
                        && !collated(value)
                        && (unicode(self.0, value) || projected.is_some_and(|p| unicode(self.0, p)))
                    {
                        let inner =
                            std::mem::replace(value.as_mut(), Expr::Value(Value::Null.into()));
                        **value = msduck_sql::expr::unary_function(MEMBER, inner);
                    }
                    Ok(())
                }
                _ => Ok(()),
            };
            match result {
                Ok(()) => ControlFlow::Continue(()),
                Err(error) => ControlFlow::Break(error),
            }
        }
    }
    match VisitMut::visit(node, &mut Mark(catalog, parameters)) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(()),
    }
}
