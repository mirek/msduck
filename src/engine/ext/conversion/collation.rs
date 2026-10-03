//! Expression COLLATE.
//!
//! Names are validated against SQL Server's collation grammar before binding
//! (448 for an unknown name, 447 for a numeric literal). After translation an
//! explicit linguistic collation becomes a DuckDB collation chain: `nocase`
//! for case insensitivity, `noaccent` for accent insensitivity and an ICU
//! locale for linguistic order (lowercase before uppercase, accented letters
//! next to their base letter). Binary collations use DuckDB's `C`
//! collation (code point order), which also overrides the `nocase`
//! collation CHAR and VARCHAR columns carry. Comparisons, IN and BETWEEN
//! with an explicit collation ignore trailing spaces, as every SQL Server
//! collation does, and apply the collation to every operand. Names resolve
//! through `msduck_sql::dialect::ext::keys::collation`.
use super::{call, sql_error};
use anyhow::Result;
use sqlparser::ast::{BinaryOperator, Expr, Ident, ObjectName, Value as Literal};

use msduck_sql::dialect::ext::keys::collation::{Rule, resolve};

fn single_name(name: &ObjectName) -> Option<String> {
    match name.0.as_slice() {
        [part] => Some(part.as_ident()?.value.clone()),
        _ => None,
    }
}

/// Reject unknown collation names and numeric literals before binding.
pub(super) fn validate(expr: &Expr) -> Result<()> {
    let Expr::Collate {
        expr: value,
        collation,
    } = expr
    else {
        return Ok(());
    };
    let text = collation.to_string();
    let Some(name) = single_name(collation) else {
        return Err(sql_error(448, 1, format!("Invalid collation '{text}'.")));
    };
    match resolve(&name) {
        Err(_) => return Err(sql_error(448, 1, format!("Invalid collation '{name}'."))),
        Ok(None) => anyhow::bail!("unsupported collation {name}"),
        Ok(Some(_)) => {}
    }
    let mut value = value.as_ref();
    while let Expr::Nested(inner) = value {
        value = inner;
    }
    if let Expr::Value(literal) = value
        && let Literal::Number(number, _) = &literal.value
    {
        let kind = if number.contains(['.', 'e', 'E']) {
            if number.contains(['e', 'E']) {
                "float"
            } else {
                "numeric"
            }
        } else if number.parse::<i32>().is_ok() {
            "int"
        } else if number.parse::<i64>().is_ok() {
            "bigint"
        } else {
            "numeric"
        };
        return Err(sql_error(
            447,
            0,
            format!("Expression type {kind} is invalid for COLLATE clause."),
        ));
    }
    Ok(())
}

const TEXT: &str = "__msduck_collation_text";
const KEY: &str = "__msduck_collation_key";
const BINARY: &str = "__msduck_collation_binary";
const RTRIM: &str = "__msduck_rtrim";

fn function_named(expr: &Expr, wanted: &str) -> bool {
    matches!(expr, Expr::Function(f) if f.name.to_string() == wanted)
}

/// An operand that carries an explicit collation after lowering: a DuckDB
/// COLLATE over the text adapter, or the binary marker.
fn collated(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => collated(inner),
        Expr::Collate { expr, .. } => function_named(expr, TEXT),
        other => function_named(other, BINARY) || padded(other).is_some_and(collated),
    }
}

/// The operand of the trailing-space trim that ANSI equality wraps around
/// character operands; the comparison key trims them itself.
fn padded(expr: &Expr) -> Option<&Expr> {
    match expr {
        Expr::Function(function) if function.name.to_string() == RTRIM => match &function.args {
            sqlparser::ast::FunctionArguments::List(list) => match list.args.first() {
                Some(sqlparser::ast::FunctionArg::Unnamed(
                    sqlparser::ast::FunctionArgExpr::Expr(value),
                )) => Some(value),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The comparison key of an operand: trailing spaces removed, keeping an
/// explicit collation in place.
fn key(expr: &mut Expr) {
    match expr {
        Expr::Nested(inner) => key(inner),
        Expr::Collate { expr: inner, .. } if function_named(inner, TEXT) => {
            if let Expr::Function(function) = inner.as_mut() {
                function.name = ObjectName::from(vec![Ident::new(KEY)]);
            }
        }
        Expr::Function(function) if function.name.to_string() == BINARY => {
            function.name = ObjectName::from(vec![Ident::new(KEY)]);
        }
        Expr::Function(function) if function.name.to_string() == KEY => {}
        other => match padded(other).filter(|value| collated(value)).cloned() {
            Some(mut value) => {
                key(&mut value);
                *other = value;
            }
            None => *other = call(KEY, vec![other.clone()]),
        },
    }
}

/// The DuckDB collation of a lowered, explicitly collated operand.
fn collation_of(expr: &Expr) -> Option<&ObjectName> {
    match expr {
        Expr::Nested(inner) => collation_of(inner),
        Expr::Collate { collation, .. } => Some(collation),
        other => padded(other).and_then(collation_of),
    }
}

/// The comparison keys of an operation's operands. Operands without the
/// explicit collation take it too: a CHAR or VARCHAR column carries its
/// own DuckDB collation, which would otherwise conflict.
fn keys(operands: Vec<&mut Expr>) {
    let collation = operands
        .iter()
        .find_map(|operand| collation_of(operand))
        .cloned();
    for operand in operands {
        let explicit = collated(operand);
        key(operand);
        if !explicit && let Some(collation) = &collation {
            let value = std::mem::replace(operand, Expr::Value(Literal::Null.into()));
            *operand = Expr::Collate {
                expr: Box::new(value),
                collation: collation.clone(),
            };
        }
    }
}

/// An equality key for an operand with an explicit collation: DuckDB's
/// DISTINCT aggregates compare raw values, so the key applies the collation.
fn equality_key(expr: &mut Expr) {
    match expr {
        Expr::Nested(inner) => equality_key(inner),
        Expr::Collate {
            expr: inner,
            collation,
        } if function_named(inner, TEXT) => {
            let parts: Vec<String> = collation
                .0
                .iter()
                .filter_map(|part| part.as_ident().map(|ident| ident.value.clone()))
                .collect();
            let Expr::Function(function) = inner.as_mut() else {
                return;
            };
            function.name = ObjectName::from(vec![Ident::new(KEY)]);
            let mut key = (**inner).clone();
            if parts.iter().any(|p| p == "nocase") {
                key = call("lower", vec![key]);
            }
            if parts.iter().any(|p| p == "noaccent") {
                key = call("strip_accents", vec![key]);
            }
            *expr = key;
        }
        Expr::Function(function) if function.name.to_string() == BINARY => {
            function.name = ObjectName::from(vec![Ident::new(KEY)]);
        }
        _ => {}
    }
}

/// The value inside the NULL-observing wrapper aggregate arguments get,
/// `list_extract(list_transform([value], …), 1)`, or `expr` itself.
fn observed(expr: &mut Expr) -> &mut Expr {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
    let wrapped = matches!(expr, Expr::Function(f) if f.name.to_string() == "list_extract");
    if !wrapped {
        return expr;
    }
    let shaped = if let Expr::Function(f) = &*expr
        && let FunctionArguments::List(list) = &f.args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(inner)))) =
            list.args.first()
        && inner.name.to_string() == "list_transform"
        && let FunctionArguments::List(inner) = &inner.args
        && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Array(array)))) =
            inner.args.first()
    {
        array.elem.len() == 1
    } else {
        false
    };
    if !shaped {
        return expr;
    }
    let Expr::Function(f) = expr else {
        unreachable!("checked")
    };
    let FunctionArguments::List(list) = &mut f.args else {
        unreachable!("checked")
    };
    let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(inner)))) =
        list.args.first_mut()
    else {
        unreachable!("checked")
    };
    let FunctionArguments::List(inner) = &mut inner.args else {
        unreachable!("checked")
    };
    let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Array(array)))) =
        inner.args.first_mut()
    else {
        unreachable!("checked")
    };
    &mut array.elem[0]
}

fn distinct_aggregate(expr: &mut Expr) {
    match expr {
        Expr::Cast { expr: inner, .. } | Expr::Nested(inner) => distinct_aggregate(inner),
        Expr::Function(function)
            if matches!(
                function.name.to_string().to_ascii_lowercase().as_str(),
                "count" | "count_big"
            ) =>
        {
            if let sqlparser::ast::FunctionArguments::List(list) = &mut function.args
                && matches!(
                    list.duplicate_treatment,
                    Some(sqlparser::ast::DuplicateTreatment::Distinct)
                )
            {
                for arg in &mut list.args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(value),
                    ) = arg
                    {
                        equality_key(observed(value));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Lower explicit collations after translation.
pub(super) fn lower(expr: &mut Expr) -> Result<(), String> {
    distinct_aggregate(expr);
    match expr {
        Expr::Collate {
            expr: value,
            collation,
        } => {
            if function_named(value, TEXT) {
                return Ok(());
            }
            let Some(name) = single_name(collation) else {
                return Ok(());
            };
            match resolve(&name) {
                // DuckDB's "C" collation compares code points; without an
                // explicit collation the session's case-insensitive default
                // would apply.
                Ok(Some(Rule::Binary)) => {
                    *expr = Expr::Collate {
                        expr: Box::new(call(TEXT, vec![(**value).clone()])),
                        collation: ObjectName::from(vec![Ident::with_quote('"', "C")]),
                    };
                }
                Ok(Some(Rule::Linguistic {
                    locale,
                    case_sensitive,
                    accent_sensitive,
                })) => {
                    let mut parts = Vec::new();
                    if !case_sensitive {
                        parts.push("nocase");
                    }
                    if !accent_sensitive {
                        parts.push("noaccent");
                    }
                    if let Some(locale) = locale {
                        parts.push(locale);
                    }
                    *expr = Expr::Collate {
                        expr: Box::new(call(TEXT, vec![(**value).clone()])),
                        collation: ObjectName::from(
                            parts
                                .into_iter()
                                .map(|part| Ident::with_quote('"', part))
                                .collect::<Vec<_>>(),
                        ),
                    };
                }
                Ok(None) => return Err(format!("unsupported collation {name}")),
                Err(_) => {}
            }
        }
        Expr::BinaryOp { left, op, right }
            if matches!(
                op,
                BinaryOperator::Eq
                    | BinaryOperator::NotEq
                    | BinaryOperator::Lt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::GtEq
            ) && (collated(left) || collated(right)) =>
        {
            keys(vec![left, right]);
        }
        Expr::InList {
            expr: value, list, ..
        } if collated(value) || list.iter().any(collated) => {
            let mut operands = vec![value.as_mut()];
            operands.extend(list.iter_mut());
            keys(operands);
        }
        Expr::Between {
            expr: value,
            low,
            high,
            ..
        } if collated(value) || collated(low) || collated(high) => {
            keys(vec![value, low, high]);
            // DuckDB's grammar takes COLLATE in BETWEEN operands only
            // parenthesized.
            for operand in [value, low, high] {
                if matches!(operand.as_ref(), Expr::Collate { .. }) {
                    let inner =
                        std::mem::replace(operand.as_mut(), Expr::Value(Literal::Null.into()));
                    **operand = Expr::Nested(Box::new(inner));
                }
            }
        }
        _ => {}
    }
    Ok(())
}
