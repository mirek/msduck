//! Logical result shapes and character normalization, independent of transport.
use msduck_core::{
    character::{Family, Length},
    money::MoneyType,
};

use sqlparser::ast::*;

/// Partial expression-result metadata, distinct from validated column declarations.
/// A bounded result capacity can be zero; SPACE itself has a minimum of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultType {
    Character { family: Family, length: Length },
    Time(u8),
    Money(MoneyType),
}
impl ResultType {
    fn character(family: Family, length: Length) -> Self {
        Self::Character { family, length }
    }
}

#[derive(Clone, Copy)]
enum Descriptor {
    Unknown,
    Null,
    Known(ResultType),
}

/// Shared expression declaration, without wire types or backend values.
pub fn expression_type(expr: &Expr) -> Option<ResultType> {
    match expression(expr) {
        Descriptor::Known(kind) => Some(kind),
        Descriptor::Unknown | Descriptor::Null => None,
    }
}

pub fn projection(statement: &Statement) -> Vec<Option<ResultType>> {
    let Statement::Query(query) = statement else {
        return vec![];
    };
    body(&query.body)
        .unwrap_or_default()
        .into_iter()
        .map(|descriptor| match descriptor {
            Descriptor::Known(kind) => Some(kind),
            _ => None,
        })
        .collect()
}

fn body(expr: &SetExpr) -> Option<Vec<Descriptor>> {
    match expr {
        SetExpr::Query(query) => body(&query.body),
        SetExpr::SetOperation { left, right, .. } => {
            let left = body(left)?;
            let right = body(right)?;
            if left.len() != right.len() {
                return None;
            }
            Some(
                left.into_iter()
                    .zip(right)
                    .map(|(a, b)| common(a, b))
                    .collect(),
            )
        }
        SetExpr::Select(select) => select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    // A projected literal has its own bounded declaration even
                    // when no runtime column or cast supplies a type.
                    Some(projected(expr))
                }
                // Wildcards change output positions after binding. Do not guess.
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn common(left: Descriptor, right: Descriptor) -> Descriptor {
    use Descriptor::*;
    match (left, right) {
        (Unknown, _) | (_, Unknown) => Unknown,
        (Null, other) | (other, Null) => other,
        (Known(ResultType::Time(a)), Known(ResultType::Time(b))) => {
            Known(ResultType::Time(a.max(b)))
        }
        (Known(ResultType::Money(a)), Known(ResultType::Money(b))) => Known(ResultType::Money(
            if a == MoneyType::Money || b == MoneyType::Money {
                MoneyType::Money
            } else {
                MoneyType::SmallMoney
            },
        )),
        (
            Known(ResultType::Character {
                family: a,
                length: x,
            }),
            Known(ResultType::Character {
                family: b,
                length: y,
            }),
        ) => {
            let family = match (a, b) {
                (Family::Char, Family::Char) => Family::Char,
                (Family::Nchar, Family::Nchar) => Family::Nchar,
                (Family::Varchar, Family::Varchar | Family::Char)
                | (Family::Char, Family::Varchar) => Family::Varchar,
                (Family::Nvarchar, Family::Nvarchar | Family::Nchar)
                | (Family::Nchar, Family::Nvarchar) => Family::Nvarchar,
                _ => return Unknown,
            };
            let length = match (x, y) {
                (Length::Bounded(x), Length::Bounded(y)) => Length::Bounded(x.max(y)),
                _ => Length::Max,
            };
            Known(ResultType::Character { family, length })
        }
        _ => Unknown,
    }
}

/// Share character-family/width merging with operand inference over explicit
/// catalog declarations. NULL selection is handled by the caller.
pub(crate) fn common_character(
    values: impl Iterator<Item = msduck_core::character::CharacterType>,
) -> Option<msduck_core::character::CharacterType> {
    match values
        .map(|kind| Descriptor::Known(ResultType::character(kind.family(), kind.length())))
        .fold(Descriptor::Null, common)
    {
        Descriptor::Known(ResultType::Character { family, length }) => {
            msduck_core::character::CharacterType::new(family, length).ok()
        }
        _ => None,
    }
}

// Keep unconverted ANSI literals on the existing unknown adapter path until
// best-fit conversion runs before the wire's strict Windows-1252 encoder.
fn projected(expr: &Expr) -> Descriptor {
    fn unconverted_ansi(expr: &Expr) -> bool {
        match expr {
            Expr::Nested(inner) | Expr::Collate { expr: inner, .. } => unconverted_ansi(inner),
            Expr::Value(value) => {
                matches!(&value.value, Value::SingleQuotedString(s) if msduck_core::encoding::encode_cp1252(s).is_err())
            }
            _ => false,
        }
    }
    if unconverted_ansi(expr) {
        expression(expr)
    } else {
        operand(expr)
    }
}

// Projected and conditional operands need literal declarations. Reuse the
// shared storage rule so their widths agree with other SQL binding paths.
fn operand(expr: &Expr) -> Descriptor {
    if let Expr::Nested(inner) | Expr::Collate { expr: inner, .. } = expr {
        return operand(inner);
    }
    if matches!(expr, Expr::Value(v) if matches!(v.value, Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)))
        && let Some(kind) =
            crate::expression_metadata::storage::kind(expr, &Default::default(), &|_| None)
    {
        let (family, length) = match kind {
            DataType::Varchar(length) => (Family::Varchar, length),
            DataType::Nvarchar(length) => (Family::Nvarchar, length),
            _ => return Descriptor::Unknown,
        };
        let length = match length {
            Some(CharacterLength::Max) => Length::Max,
            Some(CharacterLength::IntegerLength { length, .. }) => {
                let Ok(width) = u16::try_from(length) else {
                    return Descriptor::Unknown;
                };
                Length::Bounded(width.max(1))
            }
            _ => return Descriptor::Unknown,
        };
        return Descriptor::Known(ResultType::character(family, length));
    }
    expression(expr)
}

fn concatenate(left: Descriptor, right: Descriptor) -> Descriptor {
    use Descriptor::*;
    let ((a, x), (b, y)) = match (left, right) {
        (
            Known(ResultType::Character {
                family: a,
                length: x,
            }),
            Known(ResultType::Character {
                family: b,
                length: y,
            }),
        ) => ((a, x), (b, y)),
        (Null, Known(ResultType::Character { family, length }))
        | (Known(ResultType::Character { family, length }), Null) => {
            ((family, length), (family, Length::Bounded(1)))
        }
        _ => return Unknown,
    };
    let (family, length) = msduck_core::concat::shape((a, x), (b, y));
    Known(ResultType::Character { family, length })
}

// Only these source forms have a captured, value-independent NULL decision.
// Other constant expressions may have conversions or diagnostics to preserve.
fn literal_nullness(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Nested(inner) => literal_nullness(inner),
        Expr::Cast { expr: inner, .. } if literal_nullness(inner) == Some(true) => Some(true),
        Expr::Value(value) => match &value.value {
            Value::Null => Some(true),
            Value::SingleQuotedString(_) | Value::NationalStringLiteral(_) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn literal_truth(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Nested(inner) => literal_truth(inner),
        Expr::BinaryOp { left, op, right } => {
            let left = literal_count(left)?;
            let right = literal_count(right)?;
            match op {
                BinaryOperator::Eq => Some(left == right),
                BinaryOperator::NotEq => Some(left != right),
                BinaryOperator::Lt => Some(left < right),
                BinaryOperator::LtEq => Some(left <= right),
                BinaryOperator::Gt => Some(left > right),
                BinaryOperator::GtEq => Some(left >= right),
                _ => None,
            }
        }
        _ => None,
    }
}

fn has_max_operand(values: &[&Expr]) -> bool {
    values.iter().any(|value| {
        matches!(
            expression(value),
            Descriptor::Known(ResultType::Character {
                length: Length::Max,
                ..
            })
        )
    })
}

// SQL Server keeps the selected Unicode literal's width. If an ANSI branch
// wins while another branch is Unicode, conversion to the common Unicode
// family retains the widest branch declaration before constant selection,
// capped at NVARCHAR's 4,000-character bound.
fn folded_character_result(selected: Option<&Expr>, values: &[&Expr]) -> Option<Descriptor> {
    let declarations: Vec<_> = values.iter().map(|value| projected(value)).collect();
    let mut ansi = false;
    let mut unicode = false;
    let mut widest = 1;
    for declaration in &declarations {
        match declaration {
            Descriptor::Null => {}
            Descriptor::Known(ResultType::Character {
                family: family @ (Family::Varchar | Family::Nvarchar),
                length: Length::Bounded(width),
            }) => {
                ansi |= *family == Family::Varchar;
                unicode |= *family == Family::Nvarchar;
                widest = widest.max(*width);
            }
            _ => return None,
        }
    }
    let selected = selected.map(projected).unwrap_or(Descriptor::Null);
    if ansi && unicode {
        return match selected {
            Descriptor::Known(ResultType::Character {
                family: Family::Nvarchar,
                ..
            }) => Some(selected),
            Descriptor::Known(ResultType::Character {
                family: Family::Varchar,
                ..
            })
            | Descriptor::Null => Some(Descriptor::Known(ResultType::character(
                Family::Nvarchar,
                Length::Bounded(widest.min(4000)),
            ))),
            _ => None,
        };
    }
    Some(match selected {
        Descriptor::Null => declarations.into_iter().fold(Descriptor::Null, common),
        other => other,
    })
}

fn folded_case(conditions: &[CaseWhen], else_result: Option<&Expr>) -> Option<Descriptor> {
    let values: Vec<_> = conditions
        .iter()
        .map(|branch| &branch.result)
        .chain(else_result)
        .collect();
    if values.iter().any(|value| literal_nullness(value).is_none())
        || has_max_operand(&values)
        || conditions
            .iter()
            .any(|branch| literal_truth(&branch.condition).is_none())
    {
        return None;
    }
    let mut selected = None;
    for branch in conditions {
        if literal_truth(&branch.condition)? {
            selected = Some(&branch.result);
            break;
        }
    }
    folded_character_result(selected.or(else_result), &values)
}

fn folded_coalesce(values: &[&Expr]) -> Option<Descriptor> {
    if values.iter().any(|value| literal_nullness(value).is_none()) || has_max_operand(values) {
        return None;
    }
    let selected = values
        .iter()
        .copied()
        .find(|value| literal_nullness(value) == Some(false))?;
    folded_character_result(Some(selected), values)
}

// Runtime conditions must combine branch declarations even when every result
// expression is a literal. Constant conditions can instead select a captured
// literal branch before result metadata is declared.
fn common_operands<'a>(values: impl Iterator<Item = &'a Expr>, force_literals: bool) -> Descriptor {
    fn constant(expr: &Expr) -> bool {
        match expr {
            Expr::Value(_) => true,
            Expr::Nested(value) | Expr::Cast { expr: value, .. } => constant(value),
            _ => false,
        }
    }
    let values: Vec<_> = values.collect();
    let has_runtime_operand = force_literals || values.iter().any(|value| !constant(value));
    // MAX survives SQL Server's literal folding and fixes the family/capacity.
    let has_max_operand = has_max_operand(&values);
    values
        .into_iter()
        .map(|value| {
            if has_runtime_operand || has_max_operand {
                operand(value)
            } else {
                expression(value)
            }
        })
        .fold(Descriptor::Null, common)
}

fn expression(expr: &Expr) -> Descriptor {
    if let Some(kind) = crate::replicate::result_type(expr, &Default::default(), &|_| None)
        .or_else(|| crate::left_right::result_type(expr, &Default::default(), &|_| None))
    {
        return Descriptor::Known(ResultType::character(kind.family(), kind.length()));
    }
    if let Some(kind) =
        crate::expression_metadata::currency::kind(expr, &Default::default(), &|_| None)
    {
        return Descriptor::Known(ResultType::Money(kind));
    }
    if matches!(expr, Expr::Subquery(query) if matches!(query.for_clause, Some(ForClause::Json { .. })))
    {
        return Descriptor::Known(ResultType::character(Family::Nvarchar, Length::Max));
    }
    if crate::expression_metadata::storage::string_escape_call(expr) {
        return Descriptor::Known(ResultType::character(Family::Nvarchar, Length::Max));
    }
    if let Expr::Function(f) = expr
        && matches!(
            f.name.to_string().to_ascii_lowercase().as_str(),
            "json_value" | "__msduck_json_value"
        )
    {
        return Descriptor::Known(ResultType::character(
            Family::Nvarchar,
            Length::Bounded(4000),
        ));
    }

    if crate::datetimeoffset_compare::scale(expr, &Default::default()).is_none()
        && crate::datetime2_compare::scale(expr, &Default::default()).is_none()
        && let Some(scale) = crate::expression_metadata::temporal::time_scale(expr)
    {
        return Descriptor::Known(ResultType::Time(scale));
    }
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Plus | BinaryOperator::StringConcat,
            right,
        } => concatenate(operand(left), operand(right)),
        Expr::Nested(expr)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => expression(expr),
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } if crate::money_cast::money_type(data_type).is_some() => Descriptor::Known(
            ResultType::Money(crate::money_cast::money_type(data_type).unwrap()),
        ),
        Expr::Cast {
            data_type: DataType::Time(scale, TimezoneInfo::None),
            ..
        }
        | Expr::Convert {
            data_type: Some(DataType::Time(scale, TimezoneInfo::None)),
            ..
        } if scale.is_none_or(|s| s <= 7) => {
            Descriptor::Known(ResultType::Time(scale.unwrap_or(7) as u8))
        }
        Expr::Cast { data_type, .. }
        | Expr::Convert {
            data_type: Some(data_type),
            ..
        } if crate::expression_metadata::character::nchar_cast_width(data_type)
            .ok()
            .flatten()
            .is_some() =>
        {
            Descriptor::Known(ResultType::character(
                Family::Nchar,
                Length::Bounded(
                    crate::expression_metadata::character::nchar_cast_width(data_type)
                        .unwrap()
                        .unwrap(),
                ),
            ))
        }

        Expr::Cast {
            data_type: kind @ DataType::Nvarchar(_),
            ..
        }
        | Expr::Convert {
            data_type: Some(kind @ DataType::Nvarchar(_)),
            ..
        } => match crate::expression_metadata::character::nvarchar_cast_width(kind) {
            Ok(Some(width)) => Descriptor::Known(ResultType::character(
                Family::Nvarchar,
                Length::Bounded(width),
            )),
            Ok(None) => Descriptor::Known(ResultType::character(Family::Nvarchar, Length::Max)),
            Err(_) => Descriptor::Unknown,
        },

        Expr::Cast {
            data_type: kind @ DataType::Varchar(_),
            ..
        }
        | Expr::Convert {
            data_type: Some(kind @ DataType::Varchar(_)),
            ..
        } => match crate::expression_metadata::character::varchar_cast_width(kind) {
            Ok(width) => Descriptor::Known(ResultType::character(
                Family::Varchar,
                if width == u16::MAX {
                    Length::Max
                } else {
                    Length::Bounded(width)
                },
            )),
            Err(_) => Descriptor::Unknown,
        },
        Expr::Cast {
            data_type: kind @ (DataType::Char(_) | DataType::Character(_)),
            ..
        }
        | Expr::Convert {
            data_type: Some(kind @ (DataType::Char(_) | DataType::Character(_))),
            ..
        } => match crate::expression_metadata::character::varchar_cast_width(kind) {
            Ok(width) => {
                Descriptor::Known(ResultType::character(Family::Char, Length::Bounded(width)))
            }
            Err(_) => Descriptor::Unknown,
        },
        Expr::Value(value) if matches!(value.value, Value::Null) => Descriptor::Null,
        Expr::Case {
            operand: None,
            conditions,
            else_result,
            ..
        } => folded_case(conditions, else_result.as_deref()).unwrap_or_else(|| {
            common_operands(
                conditions
                    .iter()
                    .map(|branch| &branch.result)
                    .chain(else_result.iter().map(|value| value.as_ref())),
                conditions
                    .iter()
                    .any(|branch| literal_truth(&branch.condition).is_none()),
            )
        }),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => common_operands(
            conditions
                .iter()
                .map(|branch| &branch.result)
                .chain(else_result.iter().map(|value| value.as_ref())),
            false,
        ),
        Expr::Function(function) => {
            if let Some(kind @ DataType::Nvarchar(_)) =
                crate::session_function::result_type(function)
                && let Ok(Some(width)) =
                    crate::expression_metadata::character::nvarchar_cast_width(&kind)
            {
                return Descriptor::Known(ResultType::character(
                    Family::Nvarchar,
                    Length::Bounded(width),
                ));
            }
            if matches!(
                function.name.to_string().to_ascii_uppercase().as_str(),
                "SCHEMA_NAME" | "OBJECT_NAME" | "OBJECT_SCHEMA_NAME" | "COL_NAME" | "TYPE_NAME"
            ) {
                return Descriptor::Known(ResultType::character(
                    Family::Nvarchar,
                    Length::Bounded(128),
                ));
            }
            if crate::function_args::unary(function, "NCHAR")
                .ok()
                .flatten()
                .is_some()
            {
                return Descriptor::Known(ResultType::character(Family::Nchar, Length::Bounded(1)));
            }
            if crate::expression_metadata::datepart::datename_args(function)
                .ok()
                .flatten()
                .is_some()
            {
                return Descriptor::Known(ResultType::character(
                    Family::Nvarchar,
                    Length::Bounded(30),
                ));
            }
            if let Ok(Some([first, replacement])) =
                crate::expression_metadata::conditional::isnull_args(function)
            {
                return isnull_type(first, replacement)
                    .map_or(Descriptor::Unknown, Descriptor::Known);
            }
            if let Ok(Some(values)) = crate::case_types::coalesce_args(function) {
                return folded_coalesce(&values)
                    .unwrap_or_else(|| common_operands(values.into_iter(), false));
            }
            if let Ok(Some([predicate, yes, no])) = crate::predicate::iif_args(function) {
                let values = [yes, no];
                if !has_max_operand(&values)
                    && literal_nullness(yes).is_some()
                    && literal_nullness(no).is_some()
                    && let Some(truth) = literal_truth(predicate)
                    && let Some(folded) =
                        folded_character_result(Some(if truth { yes } else { no }), &values)
                {
                    return folded;
                }
                return common_operands(values.into_iter(), literal_truth(predicate).is_none());
            }
            if let Ok(Some(values)) = crate::choose::args(function) {
                return values
                    .into_iter()
                    .skip(1)
                    .map(operand)
                    .fold(Descriptor::Null, common);
            }
            if let Ok(Some([first, _])) = crate::nullif::args(function) {
                return expression(first);
            }
            if crate::function_args::unary(function, "CHAR")
                .ok()
                .flatten()
                .is_some()
            {
                return Descriptor::Known(ResultType::character(Family::Char, Length::Bounded(1)));
            }
            if let Some(count) = crate::function_args::unary(function, "SPACE")
                .ok()
                .flatten()
            {
                return Descriptor::Known(ResultType::character(
                    Family::Varchar,
                    Length::Bounded(
                        literal_count(count)
                            .map(|n| n.clamp(1, 8000) as u16)
                            .unwrap_or(8000),
                    ),
                ));
            }
            Descriptor::Unknown
        }
        _ => Descriptor::Unknown,
    }
}

fn literal_count(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Nested(expr)
        | Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => literal_count(expr),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => literal_count(expr)?.checked_neg(),
        Expr::Value(value) => match &value.value {
            Value::Number(n, _) => n.parse::<i64>().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Known Unicode and fixed CHAR first arguments retain their width.
/// Code-page text still needs known replacement conversion before promising width.
pub fn isnull_type(first: &Expr, replacement: &Expr) -> Option<ResultType> {
    use Descriptor::*;
    match (operand(first), operand(replacement)) {
        (Known(kind @ ResultType::Time(_)), _) => Some(kind),
        (
            Known(
                kind @ ResultType::Character {
                    family: Family::Char | Family::Nchar | Family::Nvarchar,
                    length: Length::Bounded(_),
                },
            ),
            _,
        ) => Some(kind),
        (Null, Known(kind)) => Some(kind),
        (Known(kind), Null | Known(_)) => Some(kind),
        _ => None,
    }
}

/// Adjust only result branches, preserving each condition/index and branch laziness.
pub fn lower_fixed_results(expr: &mut Expr) {
    let skip = |function: &sqlparser::ast::Function| {
        if crate::case_types::coalesce_args(function)
            .ok()
            .flatten()
            .is_some()
        {
            Some(0)
        } else if crate::predicate::iif_args(function)
            .ok()
            .flatten()
            .is_some()
            || crate::choose::args(function).ok().flatten().is_some()
        {
            Some(1)
        } else {
            None
        }
    };
    let oversized_ansi = |value: &Expr| {
        matches!(
            projected(value),
            Descriptor::Known(ResultType::Character {
                family: Family::Varchar,
                length: Length::Bounded(4001..=u16::MAX),
            })
        )
    };
    let has_oversized_ansi = match expr {
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            conditions
                .iter()
                .any(|branch| oversized_ansi(&branch.result))
                || else_result.as_deref().is_some_and(oversized_ansi)
        }
        Expr::Function(function) => {
            if let (Some(skip), FunctionArguments::List(args)) = (skip(function), &function.args) {
                args.args.iter().skip(skip).any(|arg| {
                    matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) if oversized_ansi(value))
                })
            } else {
                false
            }
        }
        _ => false,
    };
    // A folded mixed-family VARCHAR(5000) branch becomes NVARCHAR(4000) in
    // SQL Server. Limit the branch value as well as its descriptor.
    let (width, function) = match expression(expr) {
        Descriptor::Known(ResultType::Character {
            family: Family::Nvarchar,
            length: Length::Bounded(4000),
        }) if has_oversized_ansi => (4000, "__msduck_nvarchar_width"),
        Descriptor::Known(ResultType::Character {
            family: Family::Nchar,
            length: Length::Bounded(width),
        }) => (width, "__msduck_nchar_width"),
        Descriptor::Known(ResultType::Character {
            family: Family::Char,
            length: Length::Bounded(width),
        }) => (width, "__msduck_char_width"),
        _ => return,
    };
    let adjust = |value: &mut Expr| {
        *value = crate::expr::binary_function(
            function,
            value.clone(),
            Expr::Value(Value::Number(width.to_string(), false).into()),
        );
    };
    match expr {
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            for branch in conditions {
                adjust(&mut branch.result);
            }
            if let Some(value) = else_result {
                adjust(value);
            }
        }
        Expr::Function(function) => {
            let Some(skip) = skip(function) else {
                return;
            };
            if let FunctionArguments::List(args) = &mut function.args {
                for argument in args.args.iter_mut().skip(skip) {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = argument {
                        adjust(value);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Normalize known fixed character set operands before other passes obscure casts.
pub fn lower_fixed_sets<T: VisitMut>(value: &mut T) {
    use std::ops::ControlFlow;
    fn names(body: &SetExpr) -> Option<Vec<String>> {
        match body {
            SetExpr::Select(select) => select
                .projection
                .iter()
                .map(|item| match item {
                    SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.clone()),
                    SelectItem::UnnamedExpr(Expr::Identifier(name)) => Some(name.value.clone()),
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(names)) => {
                        names.last().map(|n| n.value.clone())
                    }
                    SelectItem::UnnamedExpr(_) => Some(String::new()),
                    _ => None,
                })
                .collect(),
            SetExpr::Query(query) => names(&query.body),
            SetExpr::SetOperation { left, .. } => names(left),
            _ => None,
        }
    }
    fn normalize(expr: &mut SetExpr) -> Option<Vec<Descriptor>> {
        match expr {
            SetExpr::Query(query) => normalize(&mut query.body),
            SetExpr::SetOperation { left, right, .. } => {
                let a = normalize(left)?;
                let b = normalize(right)?;
                if a.len() != b.len() {
                    return None;
                }
                let widths = a
                    .iter()
                    .zip(&b)
                    .map(|(a, b)| match (a, b) {
                        (
                            Descriptor::Known(ResultType::Character {
                                family: Family::Nchar,
                                length: Length::Bounded(a),
                            }),
                            Descriptor::Known(ResultType::Character {
                                family: Family::Nchar,
                                length: Length::Bounded(b),
                            }),
                        ) if a != b => Some(((*a).max(*b), "__msduck_nchar_width")),
                        (
                            Descriptor::Known(ResultType::Character {
                                family: Family::Char,
                                length: Length::Bounded(a),
                            }),
                            Descriptor::Known(ResultType::Character {
                                family: Family::Char,
                                length: Length::Bounded(b),
                            }),
                        ) if a != b => Some(((*a).max(*b), "__msduck_char_width")),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if widths.iter().any(Option::is_some) {
                    let left_names = names(left)?;
                    let right_names = names(right)?;
                    crate::nchar_sets::wrap(left, &left_names, &widths);
                    crate::nchar_sets::wrap(right, &right_names, &widths);
                }
                Some(a.into_iter().zip(b).map(|(a, b)| common(a, b)).collect())
            }
            _ => body(expr),
        }
    }
    struct Lower;
    impl VisitorMut for Lower {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            normalize(&mut query.body);
            ControlFlow::Continue(())
        }
    }
    let _ = value.visit(&mut Lower);
}

#[cfg(test)]
mod conditional_declaration_tests {
    use super::*;
    #[test]
    fn space_capacities_match_reference_without_folding_decimal_values() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/space-capacity.json")).unwrap();
        for case in fixture["results"].as_array().unwrap() {
            let stmt = crate::batch::parse(case["query"].as_str().unwrap())
                .unwrap()
                .remove(0);
            let actual = projection(&stmt);
            let columns = case["reference"]["sets"][0]["columns"].as_array().unwrap();
            assert_eq!(actual.len(), columns.len());
            for (actual, reference) in actual.into_iter().zip(columns) {
                if reference["type"] == "VarChar" {
                    assert_eq!(
                        actual,
                        Some(ResultType::Character {
                            family: Family::Varchar,
                            length: Length::Bounded(reference["length"].as_u64().unwrap() as u16)
                        }),
                        "{}",
                        case["query"]
                    );
                }
            }
        }
    }
    #[test]
    fn choose_keeps_all_arm_widths_and_max_survives_literal_folding() {
        for (sql, family, length) in [
            (
                "SELECT CHOOSE(1,N'a',N'longer')",
                Family::Nvarchar,
                Length::Bounded(6),
            ),
            (
                "SELECT CHOOSE(0,N'a',N'longer')",
                Family::Nvarchar,
                Length::Bounded(6),
            ),
            (
                "SELECT CHOOSE(NULL,'a','longer')",
                Family::Varchar,
                Length::Bounded(6),
            ),
            (
                "SELECT CHOOSE(@i,N'a',N'longer')",
                Family::Nvarchar,
                Length::Bounded(6),
            ),
            (
                "SELECT COALESCE(CAST(NULL AS NVARCHAR(MAX)),N'abc')",
                Family::Nvarchar,
                Length::Max,
            ),
            (
                "SELECT COALESCE('abc',CAST(NULL AS VARCHAR(MAX)))",
                Family::Varchar,
                Length::Max,
            ),
        ] {
            let statement = crate::batch::parse(sql).unwrap().remove(0);
            let before = statement.clone();
            assert_eq!(
                projection(&statement),
                [Some(ResultType::character(family, length))],
                "{sql}"
            );
            assert_eq!(statement, before);
        }
    }
}
