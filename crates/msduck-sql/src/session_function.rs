//! Session-function syntax and declarations over explicit inputs. No session I/O.
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

mod session_context;
pub use session_context::*;

/// Non-null session values: the INT counters and the SMALLINT @@SPID.
/// Runtime values are supplied by the shell.
pub fn counter_type(name: &str) -> Option<DataType> {
    if name.eq_ignore_ascii_case("@@SPID") {
        return Some(DataType::SmallInt(None));
    }
    ["@@ROWCOUNT", "@@TRANCOUNT", "@@ERROR"]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        .then_some(DataType::Int(None))
}

pub fn is_xact_state(function: &Function) -> bool {
    matches!(function.name.0.as_slice(), [ObjectNamePart::Identifier(id)] if id.value.eq_ignore_ascii_case("XACT_STATE"))
}
/// SESSIONPROPERTY and SESSION_CONTEXT: one argument, nullable sql_variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariantFunction {
    SessionProperty,
    SessionContext,
}
pub fn variant_function(function: &Function) -> Option<VariantFunction> {
    let [ObjectNamePart::Identifier(id)] = function.name.0.as_slice() else {
        return None;
    };
    if id.value.eq_ignore_ascii_case("SESSIONPROPERTY") {
        Some(VariantFunction::SessionProperty)
    } else if id.value.eq_ignore_ascii_case("SESSION_CONTEXT") {
        Some(VariantFunction::SessionContext)
    } else {
        None
    }
}
pub fn variant_type() -> DataType {
    DataType::Custom(ObjectName::from(vec![Ident::new("SQL_VARIANT")]), vec![])
}
pub fn is_original_login(function: &Function) -> bool {
    matches!(function.name.0.as_slice(), [ObjectNamePart::Identifier(id)] if id.value.eq_ignore_ascii_case("ORIGINAL_LOGIN"))
}
/// DB_NAME and DB_ID, which follow the session's current database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseFunction {
    Name,
    Id,
}
pub fn database_function(function: &Function) -> Option<DatabaseFunction> {
    let [ObjectNamePart::Identifier(id)] = function.name.0.as_slice() else {
        return None;
    };
    if id.value.eq_ignore_ascii_case("DB_NAME") {
        Some(DatabaseFunction::Name)
    } else if id.value.eq_ignore_ascii_case("DB_ID") {
        Some(DatabaseFunction::Id)
    } else {
        None
    }
}
/// Captured from SQL Server: DB_NAME is a nullable nvarchar(128) and DB_ID a
/// nullable smallint on the wire (documented as int), with or without an
/// argument.
fn database_type(function: DatabaseFunction) -> DataType {
    match function {
        DatabaseFunction::Name => DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 128,
            unit: None,
        })),
        DatabaseFunction::Id => DataType::SmallInt(None),
    }
}
/// Lower DB_NAME or DB_ID. Without an argument the result is the current
/// database, supplied by the shell. With one, the catalog's per-database
/// helper macros look it up; DB_NAME takes an int database ID.
pub fn database(
    function: DatabaseFunction,
    argument: Option<Expr>,
    current_name: &str,
    current_id: i32,
) -> Expr {
    let cast = |expr, data_type| Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr),
        data_type,
        format: None,
    };
    let value = match (function, argument) {
        (DatabaseFunction::Name, None) => {
            Expr::Value(Value::NationalStringLiteral(current_name.into()).into())
        }
        (DatabaseFunction::Id, None) => {
            Expr::Value(Value::Number(current_id.to_string(), false).into())
        }
        (DatabaseFunction::Name, Some(argument)) => {
            crate::expr::unary_function("__msduck_db_name", cast(argument, DataType::Int(None)))
        }
        (DatabaseFunction::Id, Some(argument)) => {
            crate::expr::unary_function("__msduck_db_id", argument)
        }
    };
    cast(value, database_type(function))
}
fn login_type() -> DataType {
    DataType::Nvarchar(Some(CharacterLength::IntegerLength {
        length: 4000,
        unit: None,
    }))
}
/// ERROR_* declarations remain nullable even inside an active CATCH. Context
/// supplies values, never a narrower declaration inferred from those values.
pub fn error_type(function: &Function) -> Option<DataType> {
    let [ObjectNamePart::Identifier(name)] = function.name.0.as_slice() else {
        return None;
    };
    match name.value.to_ascii_uppercase().as_str() {
        "ERROR_NUMBER" | "ERROR_STATE" | "ERROR_SEVERITY" | "ERROR_LINE" => {
            Some(DataType::Int(None))
        }
        "ERROR_MESSAGE" => Some(DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 4000,
            unit: None,
        }))),
        "ERROR_PROCEDURE" => Some(DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 128,
            unit: None,
        }))),
        _ => None,
    }
}

pub fn result_type(function: &Function) -> Option<DataType> {
    if is_xact_state(function) {
        Some(DataType::SmallInt(None))
    } else if let Some(database) = database_function(function) {
        Some(database_type(database))
    } else if is_original_login(function) {
        Some(login_type())
    } else if variant_function(function).is_some() {
        Some(variant_type())
    } else {
        error_type(function)
    }
}
/// Original authenticated identity, supplied by the connection adapter.
pub fn original_login(name: &str) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(
            Value::NationalStringLiteral(name.into()).into(),
        )),
        data_type: login_type(),
        format: None,
    }
}
/// Current engine states: no transaction or active. Doomed-state recovery is a
/// separate adapter responsibility and is not implemented by this scalar plan.
pub fn xact_state(active: bool) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(
            Value::Number(u8::from(active).to_string(), false).into(),
        )),
        data_type: DataType::SmallInt(None),
        format: None,
    }
}
fn check(function: &Function) -> Result<(), SqlError> {
    if result_type(function).is_none() {
        return Ok(());
    }
    let ObjectNamePart::Identifier(name) = &function.name.0[0] else {
        unreachable!()
    };
    let canonical = name.value.to_lowercase();
    let FunctionArguments::List(args) = &function.args else {
        return Err(SqlError::syntax(
            174,
            1,
            format!("The {canonical} function requires 0 argument(s)."),
        ));
    };
    if args.duplicate_treatment.is_some() {
        return Err(SqlError::syntax(
            195,
            10,
            format!("'{}' is not a recognized aggregate function.", name.value),
        ));
    }
    let wildcard = args.args.iter().any(|arg| {
        matches!(
            arg,
            FunctionArg::Unnamed(FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_))
        )
    });
    if function.over.is_some() {
        return Err(SqlError::syntax(
            4113,
            if wildcard { 1 } else { 6 },
            format!(
                "The function '{}' is not a valid windowing function, and cannot be used with the OVER clause.",
                name.value
            ),
        ));
    }
    if wildcard {
        return Err(SqlError::syntax(102, 1, "Incorrect syntax near '*'."));
    }
    if variant_function(function).is_some() {
        if args.args.len() != 1 {
            return Err(SqlError::syntax(
                174,
                1,
                format!("The {canonical} function requires 1 argument(s)."),
            ));
        }
    } else if database_function(function).is_some() {
        if args.args.len() > 1 {
            return Err(SqlError::syntax(
                189,
                1,
                format!("The {canonical} function requires 0 to 1 arguments."),
            ));
        }
    } else if !args.args.is_empty() {
        return Err(SqlError::syntax(
            174,
            1,
            format!("The {canonical} function requires 0 argument(s)."),
        ));
    }
    if !matches!(function.parameters, FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || !args.clauses.is_empty()
    {
        return Err(SqlError::syntax(
            102,
            1,
            format!("Unsupported {} modifiers.", name.value),
        ));
    }
    Ok(())
}
pub fn validate<T: Visit>(node: &T) -> Result<(), SqlError> {
    struct Check;
    impl Visitor for Check {
        type Break = SqlError;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if let Expr::Function(function) = expr
                && let Err(error) = check(function)
            {
                return ControlFlow::Break(error);
            }
            ControlFlow::Continue(())
        }
    }
    match node.visit(&mut Check) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    #[test]
    fn signatures_are_checked_in_unexecuted_branches_with_typed_diagnostics() {
        for (call, number, state) in [
            ("XACT_STATE(1)", 174, 1),
            ("XACT_STATE(*)", 102, 1),
            ("XACT_STATE() OVER()", 4113, 6),
            ("XACT_STATE(1) OVER()", 4113, 6),
            ("XACT_STATE(*) OVER()", 4113, 1),
            ("XACT_STATE(DISTINCT 1)", 195, 10),
            ("XACT_STATE(ALL 1)", 195, 10),
            ("XACT_STATE(DISTINCT 1) OVER()", 195, 10),
        ] {
            let statements =
                crate::batch::parse(&format!("INSERT INTO t VALUES(1); IF 1=0 SELECT {call}"))
                    .unwrap();
            let before = statements.clone();
            let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
            let error = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (error.number, error.state, error.severity),
                (number, state, 15),
                "{call}"
            );
            assert_eq!(statements, before);
        }
    }
    #[test]
    fn original_login_declares_reference_width_and_preflights_unreachable_calls() {
        for (call, number, state) in [
            ("ORIGINAL_LOGIN(1)", 174, 1),
            ("ORIGINAL_LOGIN(*)", 102, 1),
            ("ORIGINAL_LOGIN() OVER()", 4113, 6),
            ("ORIGINAL_LOGIN(*) OVER()", 4113, 1),
            ("ORIGINAL_LOGIN(DISTINCT 1)", 195, 10),
            ("ORIGINAL_LOGIN(ALL 1)", 195, 10),
        ] {
            let statements =
                crate::batch::parse(&format!("INSERT INTO t VALUES(1); IF 1=0 SELECT {call}"))
                    .unwrap();
            let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
            let diagnostic = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (diagnostic.number, diagnostic.state, diagnostic.severity),
                (number, state, 15)
            );
        }
        let name = "O'Connor; DROP TABLE t;--";
        let Expr::Cast {
            expr, data_type, ..
        } = original_login(name)
        else {
            panic!("missing declaration")
        };
        assert_eq!(data_type.to_string(), "NVARCHAR(4000)");
        assert!(
            matches!(*expr, Expr::Value(ValueWithSpan { value: Value::NationalStringLiteral(ref value), .. }) if value == name)
        );
    }
    #[test]
    fn database_functions_use_captured_types_and_argument_counts() {
        for (call, expected) in [
            ("DB_NAME()", "NVARCHAR(128)"),
            ("DB_NAME(1)", "NVARCHAR(128)"),
            ("DB_ID()", "SMALLINT"),
            ("DB_ID(N'x')", "SMALLINT"),
        ] {
            let statements = crate::batch::parse(&format!("SELECT {call}")).unwrap();
            crate::preflight::variables(&statements, &HashMap::new()).unwrap();
            let Statement::Query(query) = &statements[0] else {
                unreachable!()
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                unreachable!()
            };
            let SelectItem::UnnamedExpr(Expr::Function(function)) = &select.projection[0] else {
                unreachable!()
            };
            assert_eq!(
                result_type(function).unwrap().to_string(),
                expected,
                "{call}"
            );
        }
        for (call, name) in [("DB_NAME(1, 2)", "db_name"), ("DB_ID(1, 2)", "db_id")] {
            let statements =
                crate::batch::parse(&format!("INSERT INTO t VALUES(1); IF 1=0 SELECT {call}"))
                    .unwrap();
            let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
            let diagnostic = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (
                    diagnostic.number,
                    diagnostic.state,
                    diagnostic.severity,
                    diagnostic.message.as_str()
                ),
                (
                    189,
                    1,
                    15,
                    format!("The {name} function requires 0 to 1 arguments.").as_str()
                )
            );
        }
        assert_eq!(
            database(DatabaseFunction::Name, None, "O'Brien", 5).to_string(),
            "CAST(N'O''Brien' AS NVARCHAR(128))"
        );
        assert_eq!(
            database(DatabaseFunction::Id, None, "x", 5).to_string(),
            "CAST(5 AS SMALLINT)"
        );
    }
    #[test]
    fn error_functions_keep_nullable_declarations_and_validate_before_execution() {
        for (name, expected) in [
            ("ERROR_NUMBER", "INT"),
            ("ERROR_STATE", "INT"),
            ("ERROR_SEVERITY", "INT"),
            ("ERROR_LINE", "INT"),
            ("ERROR_MESSAGE", "NVARCHAR(4000)"),
            ("ERROR_PROCEDURE", "NVARCHAR(128)"),
        ] {
            let call = crate::expr::unary_function(name, crate::expr::number(0));
            let Expr::Function(mut function) = call else {
                unreachable!()
            };
            if let FunctionArguments::List(args) = &mut function.args {
                args.args.clear();
            }
            assert_eq!(result_type(&function).unwrap().to_string(), expected);
            assert_eq!(
                crate::result_properties::expression(&Expr::Function(function), &[], &[]),
                msduck_core::result::Properties::expression(true)
            );
        }
        for (call, number, state) in [
            ("ERROR_NUMBER(1)", 174, 1),
            ("ERROR_MESSAGE() OVER()", 4113, 6),
            ("ERROR_PROCEDURE(*)", 102, 1),
        ] {
            let statements =
                crate::batch::parse(&format!("INSERT INTO t VALUES(1); IF 1=0 SELECT {call}"))
                    .unwrap();
            let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
            let diagnostic = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (diagnostic.number, diagnostic.state, diagnostic.severity),
                (number, state, 15)
            );
        }
    }
    #[test]
    fn result_plan_uses_explicit_active_state_and_smallint_declaration() {
        assert_eq!(xact_state(false).to_string(), "CAST(0 AS SMALLINT)");
        assert_eq!(xact_state(true).to_string(), "CAST(1 AS SMALLINT)");
        for sql in [
            "SELECT XACT_STATE()",
            "SELECT xact_state()",
            "SELECT [XACT_STATE]()",
        ] {
            let statements = crate::batch::parse(sql).unwrap();
            crate::preflight::variables(&statements, &HashMap::new()).unwrap();
        }
    }
}
