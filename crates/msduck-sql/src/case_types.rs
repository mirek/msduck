//! Known integer/character CASE result precedence before backend binding.
use crate::parameter::Parameter;
use sqlparser::ast::*;
use std::collections::HashMap;

pub use crate::expression_metadata::conditional::coalesce_args;

#[derive(Clone, Copy)]
enum Kind {
    Null,
    Character,
    Bit,
    Integer(u8),
}

fn declared(kind: &DataType) -> Option<Kind> {
    Some(match kind {
        DataType::Bit(_) | DataType::Boolean => Kind::Bit,
        DataType::TinyInt(_) | DataType::UTinyInt => Kind::Integer(0),
        DataType::SmallInt(_) => Kind::Integer(1),
        DataType::Int(_) | DataType::Integer(_) => Kind::Integer(2),
        DataType::BigInt(_) => Kind::Integer(3),
        DataType::Varchar(_) | DataType::Nvarchar(_) | DataType::Char(_) => Kind::Character,
        kind if crate::expression_metadata::character::nchar_cast_width(kind)
            .ok()
            .flatten()
            .is_some() =>
        {
            Kind::Character
        }
        _ => return None,
    })
}

fn common<'a>(
    values: impl Iterator<Item = &'a Expr>,
    parameters: &HashMap<String, Parameter>,
) -> Option<Kind> {
    let mut result = Kind::Null;
    for value in values {
        result = match (result, infer(value, parameters)?) {
            (Kind::Integer(a), Kind::Integer(b)) => Kind::Integer(a.max(b)),
            (integer @ Kind::Integer(_), _) | (_, integer @ Kind::Integer(_)) => integer,
            (Kind::Bit, _) | (_, Kind::Bit) => Kind::Bit,
            (Kind::Character, _) | (_, Kind::Character) => Kind::Character,
            _ => Kind::Null,
        };
    }
    Some(result)
}

fn infer(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Option<Kind> {
    if let Some(value) = crate::expression_metadata::storage::retained_argument(expr)
        && matches!(infer(value, parameters), Some(Kind::Character))
    {
        return Some(Kind::Character);
    }
    match expr {
        Expr::Identifier(id) if id.value.eq_ignore_ascii_case("@@DATEFIRST") => {
            Some(Kind::Integer(0))
        }
        Expr::Identifier(id) => declared(&parameters.get(&id.value.to_lowercase())?.ast_type()),
        Expr::Cast { data_type, .. } => declared(data_type),
        Expr::Nested(expr) => infer(expr, parameters),
        Expr::Floor {
            expr,
            field: CeilFloorKind::DateTimeField(DateTimeField::NoDateTime),
        } => declared(&numeric_type(expr, "FLOOR", parameters)?),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::BitwiseAnd | BinaryOperator::BitwiseOr | BinaryOperator::BitwiseXor,
            right,
        } => match (infer(left, parameters)?, infer(right, parameters)?) {
            (Kind::Integer(a), Kind::Integer(b)) => Some(Kind::Integer(a.max(b))),
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::BitwiseNot,
            expr,
        } => match infer(expr, parameters)? {
            Kind::Bit => Some(Kind::Bit),
            integer @ Kind::Integer(_) => Some(integer),
            _ => None,
        },
        Expr::UnaryOp { op, expr } if matches!(op, UnaryOperator::Plus | UnaryOperator::Minus) => {
            match infer(expr, parameters)? {
                Kind::Integer(0) if *op == UnaryOperator::Minus => Some(Kind::Integer(1)),
                integer @ Kind::Integer(_) => Some(integer),
                _ => None,
            }
        }
        Expr::BinaryOp { left, op, right }
            if matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
                    | BinaryOperator::Modulo
            ) =>
        {
            match common([left.as_ref(), right.as_ref()].into_iter(), parameters)? {
                integer @ Kind::Integer(2..=3) => Some(integer),
                Kind::Character if *op == BinaryOperator::Plus => Some(Kind::Character),
                _ => None,
            }
        }
        Expr::Value(value) => match &value.value {
            Value::Null => Some(Kind::Null),
            Value::SingleQuotedString(_) | Value::NationalStringLiteral(_) => Some(Kind::Character),
            Value::Number(n, _) if n.parse::<i32>().is_ok() => Some(Kind::Integer(2)),
            _ => None,
        },
        Expr::Case {
            conditions,
            else_result,
            ..
        } => common(
            conditions
                .iter()
                .map(|c| &c.result)
                .chain(else_result.iter().map(|e| e.as_ref())),
            parameters,
        ),
        Expr::Function(function) => {
            if let Some(kind) = crate::session_function::result_type(function) {
                return declared(&kind);
            }
            if let Some(value) = crate::percentile::discrete_value(function) {
                return infer(value, parameters);
            }
            if let Some(value) = crate::expression_metadata::temporal::window_argument(function) {
                return infer(value, parameters);
            }
            if crate::ranking::returns_bigint(function) {
                return Some(Kind::Integer(3));
            }
            match function.name.to_string().to_ascii_uppercase().as_str() {
                "GROUPING" => return Some(Kind::Integer(0)),
                "COUNT" | "GROUPING_ID" | "DATEDIFF" => return Some(Kind::Integer(2)),
                "COUNT_BIG" | "DATEDIFF_BIG" => return Some(Kind::Integer(3)),
                _ => {}
            }
            if let Some(rank) = crate::aggregate::integer_rank(function, parameters) {
                return Some(Kind::Integer(rank));
            }
            if crate::expression_metadata::datepart::datename_args(function)
                .ok()?
                .is_some()
            {
                return Some(Kind::Character);
            }
            if crate::expression_metadata::temporal::calendar_argument(function)
                .ok()?
                .is_some()
                || crate::expression_metadata::datepart::args(function)
                    .ok()?
                    .is_some()
            {
                return Some(Kind::Integer(2));
            }
            if crate::function_args::unary(function, "NCHAR")
                .ok()?
                .is_some()
            {
                return Some(Kind::Character);
            }
            if crate::function_args::unary(function, "CHAR")
                .ok()?
                .is_some()
            {
                return Some(Kind::Character);
            }
            if crate::function_args::unary(function, "SPACE")
                .ok()?
                .is_some()
            {
                return Some(Kind::Character);
            }
            if function.name.to_string().eq_ignore_ascii_case("ISJSON")
                || function
                    .name
                    .to_string()
                    .to_ascii_lowercase()
                    .starts_with("__msduck_isjson_")
            {
                return Some(Kind::Integer(2));
            }
            if matches!(
                function.name.to_string().to_ascii_lowercase().as_str(),
                "json_value" | "json_query" | "__msduck_json_value" | "__msduck_json_query"
            ) {
                return Some(Kind::Character);
            }
            if crate::function_args::unary(function, "UNICODE")
                .ok()?
                .is_some()
            {
                return Some(Kind::Integer(2));
            }
            if let Some((name, value)) = numeric_arg(function) {
                return declared(&numeric_type(value, name, parameters)?);
            }
            if let Some([first, _]) = crate::nullif::args(function).ok()? {
                return infer(first, parameters);
            }
            if let Some([first, replacement]) =
                crate::expression_metadata::conditional::isnull_args(function).ok()?
            {
                return match infer(first, parameters)? {
                    Kind::Null => match infer(replacement, parameters)? {
                        Kind::Null => Some(Kind::Integer(2)),
                        other => Some(other),
                    },
                    other => Some(other),
                };
            }
            if let Some(args) = coalesce_args(function).ok()? {
                return common(args.into_iter(), parameters);
            }
            if let Some(args) = crate::choose::args(function).ok()? {
                return common(args.into_iter().skip(1), parameters);
            }
            let [_, yes, no] = crate::predicate::iif_args(function).ok()??;
            common([yes, no].into_iter(), parameters)
        }
        _ => None,
    }
}

pub fn is_integral(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    matches!(infer(expr, parameters), Some(Kind::Integer(_)))
}

pub fn is_character(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    matches!(infer(expr, parameters), Some(Kind::Character))
}

pub fn lower(expr: &mut Expr, parameters: &HashMap<String, Parameter>) {
    if let Expr::Floor {
        expr: value,
        field: CeilFloorKind::DateTimeField(DateTimeField::NoDateTime),
    } = expr
    {
        *expr = crate::expr::unary_function("FLOOR", *value.clone());
    }
    if let Expr::Function(function) = expr
        && let Some((name, value)) = numeric_arg(function)
        && let Some(kind) = numeric_type(value, name, parameters)
    {
        let integer = matches!(declared(&kind), Some(Kind::Integer(_)));
        if matches!(name, "CEILING" | "FLOOR") && integer {
            // Integral rounding is an identity. Avoid DuckDB's floating-point
            // overload and preserve every bit of BIGINT input values.
            *expr = value.clone();
            cast(expr, &kind);
            return;
        }
        if let FunctionArguments::List(args) = &mut function.args
            && let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &mut args.args[0]
        {
            // Decimal rounding needs the fractional input intact. ABS instead
            // widens before evaluation (notably SMALLINT's minimum to INT).
            if name == "ABS" || !matches!(kind, DataType::Decimal(_)) {
                cast(value, &kind);
            }
        }
        function.name = ObjectName::from(vec![Ident::new(match name {
            "ABS" => "__msduck_abs",
            "CEILING" => "__msduck_ceiling",
            "SIGN" => "__msduck_sign",
            _ => "__msduck_floor",
        })]);
        // Keep the result descriptor even for folded typed NULLs.
        cast(expr, &kind);
        return;
    }
    if let Expr::Between {
        expr, low, high, ..
    } = expr
    {
        if let Some(Kind::Integer(rank)) = common(
            [expr.as_ref(), low.as_ref(), high.as_ref()].into_iter(),
            parameters,
        ) {
            let kind = integer_type(rank);
            cast(expr, &kind);
            cast(low, &kind);
            cast(high, &kind);
        }
        return;
    }
    if let Expr::UnaryOp {
        op: UnaryOperator::Minus,
        expr: value,
    } = expr
        && matches!(infer(value, parameters), Some(Kind::Integer(0)))
    {
        cast(value, &DataType::SmallInt(None));
        return;
    }
    if let Expr::InList { expr, list, .. } = expr {
        if let Some(Kind::Integer(rank)) = common(
            std::iter::once(expr.as_ref()).chain(list.iter()),
            parameters,
        ) {
            let kind = integer_type(rank);
            cast(expr, &kind);
            for value in list {
                cast(value, &kind);
            }
        }
        return;
    }
    if let Expr::Function(function) = expr
        && let Ok(Some(args)) = coalesce_args(function)
        && let Some(Kind::Integer(rank)) = common(args.into_iter(), parameters)
    {
        let kind = integer_type(rank);
        if let FunctionArguments::List(args) = &mut function.args {
            for arg in &mut args.args {
                if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = arg {
                    cast(value, &kind);
                }
            }
        }
        return;
    }
    let Expr::Case {
        operand,
        conditions,
        else_result,
        ..
    } = expr
    else {
        return;
    };
    if let Some(input) = operand
        && let Some(Kind::Integer(rank)) = common(
            std::iter::once(input.as_ref()).chain(conditions.iter().map(|c| &c.condition)),
            parameters,
        )
    {
        let kind = integer_type(rank);
        cast(input, &kind);
        for branch in conditions.iter_mut() {
            cast(&mut branch.condition, &kind);
        }
    }
    let Some(Kind::Integer(rank)) = common(
        conditions
            .iter()
            .map(|c| &c.result)
            .chain(else_result.iter().map(|e| e.as_ref())),
        parameters,
    ) else {
        return;
    };
    let kind = integer_type(rank);
    for result in conditions
        .iter_mut()
        .map(|c| &mut c.result)
        .chain(else_result.iter_mut().map(|e| e.as_mut()))
    {
        cast(result, &kind);
    }
}

pub fn integer_comparison(
    left: &mut Expr,
    right: &mut Expr,
    parameters: &HashMap<String, Parameter>,
) {
    if let Some(Kind::Integer(rank)) = common([&*left, &*right].into_iter(), parameters) {
        let kind = integer_type(rank);
        cast(left, &kind);
        cast(right, &kind);
    }
}

pub fn integer_rank(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Option<u8> {
    match infer(expr, parameters)? {
        Kind::Integer(rank) => Some(rank),
        _ => None,
    }
}

pub fn is_bit(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    matches!(infer(expr, parameters), Some(Kind::Bit))
}

fn integer_type(rank: u8) -> DataType {
    match rank {
        0 => DataType::TinyInt(None),
        1 => DataType::SmallInt(None),
        2 => DataType::Int(None),
        _ => DataType::BigInt(None),
    }
}

fn numeric_arg(function: &Function) -> Option<(&'static str, &Expr)> {
    let name = match function.name.to_string().to_uppercase().as_str() {
        "ABS" => "ABS",
        "CEILING" => "CEILING",
        "FLOOR" => "FLOOR",
        "SIGN" => "SIGN",
        _ => return None,
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || function.over.is_some()
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
    {
        return None;
    }
    let FunctionArguments::List(args) = &function.args else {
        return None;
    };
    if args.duplicate_treatment.is_some() || !args.clauses.is_empty() {
        return None;
    }
    match args.args.as_slice() {
        [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] => Some((name, value)),
        _ => None,
    }
}

fn numeric_type(
    value: &Expr,
    name: &str,
    parameters: &HashMap<String, Parameter>,
) -> Option<DataType> {
    let source = match value {
        Expr::Nested(value) => return numeric_type(value, name, parameters),
        Expr::UnaryOp {
            op: UnaryOperator::Plus | UnaryOperator::Minus,
            expr,
        } => return numeric_type(expr, name, parameters),
        Expr::Value(value) => match &value.value {
            Value::Number(number, _) => numeric_literal_type(number),
            _ => None,
        },
        Expr::Floor {
            expr,
            field: CeilFloorKind::DateTimeField(DateTimeField::NoDateTime),
        } => numeric_type(expr, "FLOOR", parameters),
        Expr::Function(function) => {
            numeric_arg(function).and_then(|(name, value)| numeric_type(value, name, parameters))
        }
        Expr::Cast { data_type, .. } => Some(data_type.clone()),
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .map(|p| p.ast_type()),
        _ => None,
    };
    if let Some(source) = source {
        match &source {
            DataType::Bit(_) | DataType::Boolean if name == "SIGN" => return None,
            DataType::Bit(_)
            | DataType::Boolean
            | DataType::Real
            | DataType::Float(_)
            | DataType::Double(_) => return Some(DataType::Double(ExactNumberInfo::None)),
            DataType::Decimal(info) | DataType::Numeric(info) => {
                let (precision, scale) = match info {
                    ExactNumberInfo::PrecisionAndScale(precision, scale) => (*precision, *scale),
                    ExactNumberInfo::Precision(precision) => (*precision, 0),
                    ExactNumberInfo::None => (18, 0),
                };
                let (precision, scale) = match name {
                    "ABS" => (38, scale),
                    "SIGN" => (precision, scale),
                    _ => (precision, 0),
                };
                return Some(DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
                    precision, scale,
                )));
            }
            DataType::Custom(name, _)
                if matches!(
                    name.to_string().to_lowercase().as_str(),
                    "money" | "smallmoney"
                ) =>
            {
                return Some(DataType::Custom(
                    ObjectName::from(vec![Ident::new("money")]),
                    vec![],
                ));
            }
            _ => {}
        }
    }
    match infer(value, parameters)? {
        Kind::Integer(rank) => Some(integer_type(rank.max(2))),
        _ => None,
    }
}

fn cast(expr: &mut Expr, kind: &DataType) {
    *expr = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr.clone()),
        data_type: kind.clone(),
        format: None,
    };
}

// Classify numeric tokens without passing exact decimals through floating point.
// Signs are separate AST unary operators. Literals beyond DECIMAL(38) remain
// outside this inference path until their diagnostics are implemented.
pub use crate::expression_metadata::storage::numeric_literal_type;

/// Run after visiting the literal so the generated CAST is not recursively
/// translated. Small integers stay bare, preserving ORDER BY ordinals.
pub fn lower_literal(expr: &mut Expr) {
    if let Expr::Value(value) = expr
        && let Value::Number(number, _) = &value.value
        && let Some(kind @ DataType::Decimal(_)) = numeric_literal_type(number)
    {
        cast(expr, &kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::ServerDialect;
    use msduck_core::{types::Type, value::Value as BoundValue};
    use sqlparser::parser::Parser;

    fn expression(sql: &str) -> Expr {
        let Statement::Query(query) = Parser::parse_sql(&ServerDialect, &format!("SELECT {sql}"))
            .unwrap()
            .remove(0)
        else {
            panic!("query")
        };
        let SetExpr::Select(select) = *query.body else {
            panic!("select")
        };
        let SelectItem::UnnamedExpr(value) = select.projection.into_iter().next().unwrap() else {
            panic!("expression")
        };
        value
    }

    #[test]
    fn explicit_parameters_and_unknown_operands_control_inference() {
        let mut parameters = HashMap::from([
            (
                "@small".into(),
                Parameter {
                    value: BoundValue::Null,
                    data_type: Type::SmallInt,
                },
            ),
            (
                "@big".into(),
                Parameter {
                    value: BoundValue::Null,
                    data_type: Type::BigInt,
                },
            ),
        ]);
        for (sql, expected) in [
            ("ISNULL(@small,@big)", Some(1)),
            ("COALESCE(@small,@big)", Some(3)),
            ("NULLIF(@small,@big)", Some(1)),
            ("SUM(@small)", Some(2)),
            ("AVG(@big)", Some(3)),
            ("MIN(@small)", Some(1)),
            ("CASE WHEN 1=1 THEN @small ELSE @big END", Some(3)),
            ("COALESCE(@small,unbound_column)", None),
            ("CASE WHEN 1=1 THEN @small ELSE unbound_column END", None),
        ] {
            assert_eq!(
                integer_rank(&expression(sql), &parameters),
                expected,
                "{sql}"
            );
        }
        parameters.get_mut("@small").unwrap().data_type = Type::BigInt;
        assert_eq!(
            integer_rank(&expression("ISNULL(@small,1)"), &parameters),
            Some(3)
        );
        assert!(is_character(
            &expression("TRIM(CAST('x' AS NCHAR(3)))"),
            &parameters
        ));
        assert!(is_character(
            &expression("DATENAME(year,CAST('2024-01-01' AS DATE))"),
            &parameters
        ));
        assert!(!is_character(
            &expression("DATENAME('year',1)"),
            &parameters
        ));
    }

    #[test]
    fn numeric_lowering_preserves_producer_count_and_exact_bigint_type() {
        for sql in [
            "FLOOR(CAST(nextval('calls') AS BIGINT))",
            "CEILING(CAST(nextval('calls') AS BIGINT))",
            "COALESCE(CAST(nextval('first') AS INT),CAST(nextval('second') AS BIGINT))",
        ] {
            let mut value = expression(sql);
            let before = value.to_string().matches("nextval(").count();
            lower(&mut value, &HashMap::new());
            assert_eq!(value.to_string().matches("nextval(").count(), before);
            assert_eq!(integer_rank(&value, &HashMap::new()), Some(3));
            assert!(!value.to_string().contains("DOUBLE"));
        }
        let mut unknown = expression("COALESCE(unbound_column,CAST(1 AS INT))");
        let before = unknown.clone();
        lower(&mut unknown, &HashMap::new());
        assert_eq!(unknown, before);
    }
}
