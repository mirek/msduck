//! LEFT/RIGHT declarations and lowering over explicit logical inputs.
use crate::parameter::Parameter;
use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::Type,
};
use sqlparser::ast::*;
use std::collections::HashMap;

fn args(f: &Function) -> Result<Option<(&str, [&Expr; 2])>, SqlError> {
    let name = if f.name.to_string().eq_ignore_ascii_case("LEFT") {
        "left"
    } else if f.name.to_string().eq_ignore_ascii_case("RIGHT") {
        "right"
    } else {
        return Ok(None);
    };
    let error = || {
        SqlError::syntax(
            174,
            1,
            format!("The {name} function requires 2 argument(s)."),
        )
    };
    let FunctionArguments::List(list) = &f.args else {
        return Err(error());
    };
    if f.over.is_some() {
        return Err(SqlError::syntax(
            4113,
            6,
            format!(
                "The function '{}' is not a valid windowing function, and cannot be used with the OVER clause.",
                name.to_uppercase()
            ),
        ));
    }
    if list.duplicate_treatment.is_some()
        || !list.clauses.is_empty()
        || f.filter.is_some()
        || !f.within_group.is_empty()
        || f.null_treatment.is_some()
        || !matches!(f.parameters, FunctionArguments::None)
    {
        return Err(SqlError::syntax(
            102,
            1,
            "Unsupported LEFT/RIGHT modifiers.",
        ));
    }
    match list.args.as_slice() {
        [
            FunctionArg::Unnamed(FunctionArgExpr::Expr(s)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(n)),
        ] => Ok(Some((name, [s, n]))),
        _ => Err(error()),
    }
}

fn cast_family(name: &str) -> Option<Family> {
    match name {
        "__msduck_cast_carrier_nvarchar" => Some(Family::Nvarchar),
        "__msduck_cast_carrier_nchar" => Some(Family::Nchar),
        "__msduck_cast_carrier_varchar" => Some(Family::Varchar),
        "__msduck_cast_carrier_char" => Some(Family::Char),
        _ => None,
    }
}
fn carrier(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> bool {
    match expr {
        Expr::Nested(e) => carrier(e, parameters, column),
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| matches!(p.value, msduck_core::value::Value::Unicode(_))),
        Expr::Cast {
            expr, data_type, ..
        } => {
            matches!(crate::sql_type::declaration(data_type),Ok(Type::Character(t)) if matches!(t.family(),Family::Nchar|Family::Nvarchar))
                && carrier(expr, parameters, column)
        }
        Expr::Function(_) => result_type(expr, parameters, column)
            .is_some_and(|t| matches!(t.family(), Family::Nchar | Family::Nvarchar)),
        _ => false,
    }
}

pub fn result_type(
    expr: &Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Option<CharacterType> {
    let Expr::Function(f) = expr else {
        return None;
    };
    if let Some(family) = cast_family(&f.name.to_string()) {
        let FunctionArguments::List(list) = &f.args else {
            return None;
        };
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(width)) = list.args.get(1)? else {
            return None;
        };
        let width = crate::replicate::constant_count(width)?;
        return CharacterType::new(
            family,
            if width == -1 {
                Length::Max
            } else {
                Length::Bounded(width.try_into().ok()?)
            },
        )
        .ok();
    }
    if matches!(
        f.name.to_string().as_str(),
        "__msduck_left_unicode_typed" | "__msduck_right_unicode_typed"
    ) {
        let FunctionArguments::List(list) = &f.args else {
            return None;
        };
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(width)) = list.args.get(2)? else {
            return None;
        };
        let width = crate::replicate::constant_count(width)?;
        return CharacterType::new(
            Family::Nvarchar,
            if width == -1 {
                Length::Max
            } else {
                Length::Bounded(width.try_into().ok()?)
            },
        )
        .ok();
    }
    let (_, [source, count]) = args(f).ok()??;
    let (input, _, _) = crate::replicate::source_type(source, parameters, column)?;
    Some(msduck_core::left_right::result_type(
        input,
        crate::replicate::constant_count(count).and_then(|n| n.try_into().ok()),
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
pub fn lower(
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
    column: &impl Fn(&Expr) -> Option<DataType>,
) -> Result<(), String> {
    if let Expr::Cast {
        expr: source,
        data_type,
        format: None,
        ..
    } = expr
        && carrier(source, parameters, column)
        && let Ok(Type::Character(mut target)) = crate::sql_type::declaration(data_type)
    {
        if matches!(
            data_type.to_string().to_ascii_uppercase().as_str(),
            "VARCHAR" | "NVARCHAR" | "CHAR" | "NCHAR"
        ) {
            target = CharacterType::new(target.family(), Length::Bounded(30))
                .map_err(|e| e.to_string())?;
        }
        let suffix = match target.family() {
            Family::Nvarchar => "nvarchar",
            Family::Nchar => "nchar",
            Family::Varchar => "varchar",
            Family::Char => "char",
        };
        let width = match target.length() {
            Length::Max => -1,
            Length::Bounded(n) => i32::from(n),
        };
        *expr = crate::expr::binary_function(
            &format!("__msduck_cast_carrier_{suffix}"),
            crate::expr::unary_function("__msduck_pack_unicode", *source.clone()),
            crate::expr::number(width),
        );
        return Ok(());
    }
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    let Some((name, [source, count])) = args(f).map_err(|e| e.message)? else {
        return Ok(());
    };
    let Some((input, binary, bit)) = crate::replicate::source_type(source, parameters, column)
    else {
        return Ok(());
    };
    let result = msduck_core::left_right::result_type(
        input,
        crate::replicate::constant_count(count).and_then(|n| n.try_into().ok()),
    );
    let count = cast(count.clone(), DataType::Int(None));
    if matches!(input.family(), Family::Nchar | Family::Nvarchar) {
        let source = crate::expr::unary_function("__msduck_pack_unicode", source.clone());
        let mut function =
            crate::expr::binary_function(&format!("__msduck_{name}_unicode_typed"), source, count);
        let width = match result.length() {
            Length::Max => -1,
            Length::Bounded(n) => i32::from(n),
        };
        if let Expr::Function(f) = &mut function
            && let FunctionArguments::List(list) = &mut f.args
        {
            list.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(
                crate::expr::number(width),
            )));
        }
        *expr = function;
    } else {
        let source = if binary {
            source.clone()
        } else {
            let source = if bit {
                cast(source.clone(), DataType::Int(None))
            } else {
                source.clone()
            };
            cast(source, crate::sql_type::ast(Type::Character(input)))
        };
        *expr = cast(
            crate::expr::binary_function(
                &format!(
                    "__msduck_{name}_{}",
                    if binary { "binary" } else { "varchar" }
                ),
                source,
                count,
            ),
            crate::sql_type::ast(Type::Character(result)),
        );
    }
    Ok(())
}
pub fn validate(statement: &Statement) -> anyhow::Result<()> {
    struct Check;
    impl Visitor for Check {
        type Break = SqlError;
        fn pre_visit_expr(&mut self, expr: &Expr) -> std::ops::ControlFlow<SqlError> {
            if let Expr::Function(f) = expr
                && let Err(error) = args(f)
            {
                return std::ops::ControlFlow::Break(error);
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    match Visit::visit(statement, &mut Check) {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declarations_and_lowering_retain_family_constants_and_nested_sources() {
        for (source, expected) in [
            ("LEFT('abcdef',3)", "VARCHAR(3)"),
            ("RIGHT(N'abcdef',3)", "NVARCHAR(3)"),
            ("LEFT(CAST('x' AS CHAR(10)),3)", "VARCHAR(3)"),
            ("RIGHT(N'x',0)", "NVARCHAR(1)"),
            ("LEFT(CAST(N'x' AS NVARCHAR(MAX)),2)", "NVARCHAR(MAX)"),
            ("RIGHT(LEFT(N'🦆xy',2),1)", "NVARCHAR(1)"),
            ("RIGHT('abcdef',2.9)", "VARCHAR(6)"),
            ("LEFT(12345,2)", "VARCHAR(2)"),
            (
                "LEFT(CASE WHEN 1=1 THEN CAST(NULL AS NVARCHAR(3)) ELSE N'🦆 ' END,3)",
                "NVARCHAR(3)",
            ),
            ("LEFT(0x008041,2)", "VARCHAR(2)"),
        ] {
            let mut statements = crate::batch::parse(&format!("SELECT {source}")).unwrap();
            let Statement::Query(query) = &mut statements[0] else {
                unreachable!()
            };
            let SetExpr::Select(select) = query.body.as_mut() else {
                unreachable!()
            };
            let SelectItem::UnnamedExpr(expr) = &mut select.projection[0] else {
                unreachable!()
            };
            let kind = result_type(expr, &HashMap::new(), &|_| None).unwrap();
            assert_eq!(
                crate::sql_type::ast(Type::Character(kind)).to_string(),
                expected
            );
            lower(expr, &HashMap::new(), &|_| None).unwrap();
            let lowered = expr.clone();
            lower(expr, &HashMap::new(), &|_| None).unwrap();
            assert_eq!(*expr, lowered);
            if expected.starts_with("NVARCHAR") {
                assert_eq!(result_type(expr, &HashMap::new(), &|_| None), Some(kind));
            }
        }
        let statement = crate::batch::parse("SELECT LEFT('x')").unwrap().remove(0);
        assert_eq!(
            validate(&statement)
                .unwrap_err()
                .downcast_ref::<SqlError>()
                .unwrap()
                .number,
            174
        );
    }
}
