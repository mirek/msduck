//! REPLICATE typing and lowering over explicit source declarations.
use crate::{expression_metadata::storage, parameter::Parameter};
use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::Type,
};
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

pub fn args(function: &Function) -> Result<Option<[&Expr; 2]>, SqlError> {
    if !function.name.to_string().eq_ignore_ascii_case("REPLICATE") {
        return Ok(None);
    }
    let invalid = || SqlError::syntax(174, 1, "The replicate function requires 2 argument(s).");
    let FunctionArguments::List(args) = &function.args else {
        return Err(invalid());
    };
    if function.over.is_some() {
        return Err(SqlError::syntax(
            4113,
            6,
            "The function 'REPLICATE' is not a valid windowing function, and cannot be used with the OVER clause.",
        ));
    }
    if args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(function.parameters, FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
    {
        return Err(SqlError::syntax(102, 1, "Unsupported REPLICATE modifiers."));
    }
    match args.args.as_slice() {
        [
            FunctionArg::Unnamed(FunctionArgExpr::Expr(source)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(count)),
        ] => Ok(Some([source, count])),
        _ => Err(invalid()),
    }
}
pub(crate) fn source_type(
    source: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<(CharacterType, bool, bool)> {
    let kind = if matches!(source, Expr::Value(v) if matches!(&v.value, Value::NationalStringLiteral(text) if text.is_empty()))
    {
        Type::Character(CharacterType::new(Family::Nvarchar, Length::Bounded(1)).ok()?)
    } else if matches!(source, Expr::Value(v) if matches!(&v.value, Value::Null) || matches!(&v.value, Value::SingleQuotedString(text) if text.is_empty()))
    {
        Type::Character(CharacterType::new(Family::Varchar, Length::Bounded(1)).ok()?)
    } else {
        crate::sql_type::declaration(&storage::kind(source, parameters, column)?).ok()?
    };
    let (width, binary, bit) = match kind {
        Type::Character(kind) => return Some((kind, false, false)),
        Type::Binary(kind) => {
            return Some((
                CharacterType::new(Family::Varchar, kind.length()).ok()?,
                true,
                false,
            ));
        }
        Type::Bit => (1, false, true),
        Type::TinyInt => (4, false, false),
        Type::SmallInt => (6, false, false),
        Type::Int => (12, false, false),
        Type::BigInt => (24, false, false),
        Type::Decimal(_) => (41, false, false),
        Type::Money | Type::SmallMoney => (40, false, false),
        Type::Real | Type::Float => (23, false, false),
        _ => return None,
    };
    Some((
        CharacterType::new(Family::Varchar, Length::Bounded(width)).ok()?,
        binary,
        bit,
    ))
}
pub(crate) fn constant_count(expr: &Expr) -> Option<i32> {
    if let Expr::Cast {
        expr,
        data_type: DataType::Int(_) | DataType::Integer(_),
        ..
    } = expr
    {
        return constant_count(crate::variant_cast::source(expr).unwrap_or(expr));
    }
    crate::expression_metadata::temporal::integer_constant(expr)
}
pub fn result_type(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<CharacterType> {
    let Expr::Function(function) = expr else {
        return None;
    };
    let [source, count] = args(function).ok()??;
    let (input, _, _) = source_type(source, parameters, column)?;
    Some(msduck_core::replicate::result_type(
        input,
        constant_count(count),
    ))
}
fn cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr),
        data_type,
        format: None,
    }
}
// DuckDB first converts an unquoted decimal literal through its own numeric
// literal type before REAL, which double-rounds some SQL Server boundaries.
// Its string-to-REAL conversion reads the exact source decimal. Apply that
// only to a directly cast plain decimal literal; expressions and floating
// exponent literals retain their normal execution path.
fn exact_real_literal(source: &Expr) -> Expr {
    let mut source = source.clone();
    if let Expr::Cast { expr, .. } = &mut source {
        let decimal = match expr.as_ref() {
            Expr::Value(v) => match &v.value {
                Value::Number(text, _) => Some(text.clone()),
                _ => None,
            },
            Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr,
            } => match expr.as_ref() {
                Expr::Value(v) => match &v.value {
                    Value::Number(text, _) => Some(format!("-{text}")),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        if let Some(decimal) = decimal
            && decimal
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.' || byte == b'-')
        {
            **expr = Expr::Value(Value::SingleQuotedString(decimal).into());
        }
    }
    source
}
pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some([source, count]) = args(function).map_err(|error| error.message)? else {
        return Ok(());
    };
    let Some((input, binary, bit)) = source_type(source, parameters, column) else {
        return Ok(());
    };
    let floating = storage::kind(source, parameters, column)
        .and_then(|kind| crate::sql_type::declaration(&kind).ok())
        .filter(|kind| matches!(kind, Type::Real | Type::Float));
    let result = msduck_core::replicate::result_type(input, constant_count(count));
    let unicode = matches!(input.family(), Family::Nchar | Family::Nvarchar);
    let family = if matches!(floating, Some(Type::Real)) {
        "real"
    } else if matches!(floating, Some(Type::Float)) {
        "float"
    } else if binary {
        "binary"
    } else if unicode {
        "nvarchar"
    } else {
        "varchar"
    };
    let name = format!(
        "__msduck_replicate_{family}{}",
        if input.length() == Length::Max {
            "_max"
        } else {
            ""
        }
    );
    let source = if let Some(kind) = floating {
        let source = if kind == Type::Real {
            exact_real_literal(source)
        } else {
            source.clone()
        };
        cast(source, crate::sql_type::ast(kind))
    } else if binary {
        source.clone()
    } else {
        let source = if bit {
            cast(source.clone(), DataType::Int(None))
        } else {
            source.clone()
        };
        cast(source, crate::sql_type::ast(Type::Character(input)))
    };
    let function =
        crate::expr::binary_function(&name, source, cast(count.clone(), DataType::Int(None)));
    *expr = cast(function, crate::sql_type::ast(Type::Character(result)));
    Ok(())
}
pub fn validate(statement: &Statement) -> anyhow::Result<()> {
    struct Check;
    impl Visitor for Check {
        type Break = SqlError;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if let Expr::Function(function) = expr
                && let Err(error) = args(function)
            {
                return ControlFlow::Break(error);
            }
            ControlFlow::Continue(())
        }
    }
    match Visit::visit(statement, &mut Check) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declarations_and_lowering_preserve_arguments_and_constant_counts() {
        for (sql, expected) in [
            ("REPLICATE('ab',3)", "VARCHAR(6)"),
            ("REPLICATE(N'雪',3)", "NVARCHAR(3)"),
            ("REPLICATE('x',1+2)", "VARCHAR(3)"),
            ("REPLICATE('x',CAST(3 AS INT))", "VARCHAR(3)"),
            ("REPLICATE('x',0)", "VARCHAR(2)"),
            ("REPLICATE(N'x',-1)", "NVARCHAR(1)"),
            ("REPLICATE(12,3)", "VARCHAR(36)"),
            ("REPLICATE(CAST(1 AS REAL),2)", "VARCHAR(46)"),
            ("REPLICATE(CAST(1 AS FLOAT),2)", "VARCHAR(46)"),
            ("REPLICATE(CAST(0.00009999995 AS REAL),2)", "VARCHAR(46)"),
            ("REPLICATE('x',2.9)", "VARCHAR(8000)"),
            ("REPLICATE(CAST('x' AS VARCHAR(MAX)),3)", "VARCHAR(MAX)"),
        ] {
            let mut statements = crate::batch::parse(&format!("SELECT {sql}")).unwrap();
            let Statement::Query(query) = &mut statements[0] else {
                unreachable!()
            };
            let SetExpr::Select(select) = query.body.as_mut() else {
                unreachable!()
            };
            let SelectItem::UnnamedExpr(expr) = &mut select.projection[0] else {
                unreachable!()
            };
            assert_eq!(
                crate::sql_type::ast(Type::Character(
                    result_type(expr, &HashMap::new(), &|_| None).unwrap()
                ))
                .to_string(),
                expected,
                "{sql}"
            );
            lower(expr, &HashMap::new(), &|_| None).unwrap();
            if sql.contains(" AS REAL)") {
                assert!(expr.to_string().contains("__msduck_replicate_real"));
            }
            if sql.contains("0.00009999995") {
                assert!(expr.to_string().contains("'0.00009999995'"));
            }
            if sql.contains(" AS FLOAT)") {
                assert!(expr.to_string().contains("__msduck_replicate_float"));
            }
            let once = expr.clone();
            lower(expr, &HashMap::new(), &|_| None).unwrap();
            assert_eq!(*expr, once);
        }
        let mut statements =
            crate::batch::parse("SELECT REPLICATE(CAST(RANDOM() AS REAL),CAST(RANDOM()*2 AS INT))")
                .unwrap();
        let Statement::Query(query) = &mut statements[0] else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        let SelectItem::UnnamedExpr(expr) = &mut select.projection[0] else {
            unreachable!()
        };
        lower(expr, &HashMap::new(), &|_| None).unwrap();
        assert_eq!(expr.to_string().matches("RANDOM()").count(), 2);
        let statements =
            crate::batch::parse("INSERT INTO t VALUES(1); IF 1=0 SELECT REPLICATE('x',1,2)")
                .unwrap();
        let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 174);
    }
}
