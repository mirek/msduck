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

#[derive(Clone)]
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
                    Some(expression(expr))
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

// Conditional operands need literal declarations even when the top-level
// literal result takes a separate adapter path. Reuse the shared storage rule.
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

// Literal-only conditionals may be folded by SQL Server before metadata is
// declared. Keep the existing conservative result until that folding is modeled.
fn common_operands<'a>(values: impl Iterator<Item = &'a Expr>) -> Descriptor {
    fn constant(expr: &Expr) -> bool {
        match expr {
            Expr::Value(_) => true,
            Expr::Nested(value) | Expr::Cast { expr: value, .. } => constant(value),
            _ => false,
        }
    }
    let values: Vec<_> = values.collect();
    let has_runtime_operand = values.iter().any(|value| !constant(value));
    // MAX survives SQL Server's literal folding and fixes the family/capacity.
    let has_max_operand = values.iter().any(|value| {
        matches!(
            expression(value),
            Descriptor::Known(ResultType::Character {
                length: Length::Max,
                ..
            })
        )
    });
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
            conditions,
            else_result,
            ..
        } => common_operands(
            conditions
                .iter()
                .map(|branch| &branch.result)
                .chain(else_result.iter().map(|value| value.as_ref())),
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
                return common_operands(values.into_iter());
            }
            if let Ok(Some([_, yes, no])) = crate::predicate::iif_args(function) {
                return common_operands([yes, no].into_iter());
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

/// Pad only result branches, preserving each condition/index and branch laziness.
pub fn lower_fixed_results(expr: &mut Expr) {
    let (width, function) = match expression(expr) {
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
    let pad = |value: &mut Expr| {
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
                pad(&mut branch.result);
            }
            if let Some(value) = else_result {
                pad(value);
            }
        }
        Expr::Function(function) => {
            let skip = if crate::case_types::coalesce_args(function)
                .ok()
                .flatten()
                .is_some()
            {
                0
            } else if crate::predicate::iif_args(function)
                .ok()
                .flatten()
                .is_some()
                || crate::choose::args(function).ok().flatten().is_some()
            {
                1
            } else {
                return;
            };
            if let FunctionArguments::List(args) = &mut function.args {
                for argument in args.args.iter_mut().skip(skip) {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = argument {
                        pad(value);
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
