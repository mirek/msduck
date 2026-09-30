//! Session-function syntax and declarations over explicit inputs. No session I/O.
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

mod session_context;
pub use session_context::*;

/// Session values named without parentheses: the non-null INT counters, the
/// SMALLINT @@SPID and the nullable nvarchar(128) SYSTEM_USER. Runtime values
/// are supplied by the shell.
pub fn counter_type(name: &str) -> Option<DataType> {
    if is_system_user(name) {
        return Some(login_name_type());
    }
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
/// SUSER_SNAME and SUSER_NAME: the login name of the current security
/// context, a nullable nvarchar(128). msduck has no impersonation, so this is
/// the authenticated login. SUSER_SNAME(sid) looks the SID up in
/// sys.server_principals.
pub fn is_login_name(function: &Function) -> bool {
    matches!(function.name.0.as_slice(), [ObjectNamePart::Identifier(id)]
        if id.quote_style.is_none()
            && (id.value.eq_ignore_ascii_case("SUSER_SNAME") || id.value.eq_ignore_ascii_case("SUSER_NAME")))
}
fn login_name_type() -> DataType {
    DataType::Nvarchar(Some(CharacterLength::IntegerLength {
        length: 128,
        unit: None,
    }))
}
/// SYSTEM_USER, the login name like SUSER_SNAME().
pub fn is_system_user(name: &str) -> bool {
    name.eq_ignore_ascii_case("SYSTEM_USER")
}
/// Lower SYSTEM_USER for the authenticated login `name`.
pub fn system_user(name: &str) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(
            Value::NationalStringLiteral(name.into()).into(),
        )),
        data_type: login_name_type(),
        format: None,
    }
}
/// Lower SUSER_SNAME or SUSER_NAME for the authenticated login `name`.
pub fn login_name(function: &Function, name: &str) -> Expr {
    let argument = match &function.args {
        FunctionArguments::List(args) => match args.args.as_slice() {
            [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] => Some(argument.clone()),
            _ => None,
        },
        _ => None,
    };
    let value = match argument {
        None => Expr::Value(Value::NationalStringLiteral(name.into()).into()),
        Some(sid) => {
            let mut statement = crate::batch::parse(
                "SELECT name FROM sys.server_principals WHERE sid = CAST(__msduck_sid AS VARBINARY(85))",
            )
            .expect("valid SID lookup")
            .remove(0);
            let _ = visit_expressions_mut(&mut statement, |expr| {
                if matches!(expr, Expr::Identifier(id) if id.value == "__msduck_sid") {
                    *expr = sid.clone();
                }
                ControlFlow::<()>::Continue(())
            });
            let Statement::Query(query) = statement else {
                unreachable!()
            };
            Expr::Subquery(query)
        }
    };
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(value),
        data_type: login_name_type(),
        format: None,
    }
}
pub fn is_original_login(function: &Function) -> bool {
    matches!(function.name.0.as_slice(), [ObjectNamePart::Identifier(id)] if id.value.eq_ignore_ascii_case("ORIGINAL_LOGIN"))
}
/// Current-time functions. CURRENT_TIMESTAMP is GETDATE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrentTime {
    SysDateTimeOffset,
    SysUtcDateTime,
    SysDateTime,
    GetUtcDate,
    GetDate,
}
pub fn current_time(function: &Function) -> Option<CurrentTime> {
    let [ObjectNamePart::Identifier(id)] = function.name.0.as_slice() else {
        return None;
    };
    if id.quote_style.is_some() {
        return None;
    }
    Some(match id.value.to_ascii_uppercase().as_str() {
        "SYSDATETIMEOFFSET" => CurrentTime::SysDateTimeOffset,
        "SYSUTCDATETIME" => CurrentTime::SysUtcDateTime,
        "SYSDATETIME" => CurrentTime::SysDateTime,
        "GETUTCDATE" => CurrentTime::GetUtcDate,
        "GETDATE" | "CURRENT_TIMESTAMP" => CurrentTime::GetDate,
        _ => return None,
    })
}
fn is_current_timestamp(function: &Function) -> bool {
    matches!(function.name.0.as_slice(), [ObjectNamePart::Identifier(id)] if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("CURRENT_TIMESTAMP"))
}
/// Captured declarations (reference/tedious-compat-gaps.json): not nullable
/// datetimeoffset(7), datetime2(7) and datetime.
fn current_time_type(kind: CurrentTime) -> DataType {
    let custom =
        |name: &str| DataType::Custom(ObjectName::from(vec![Ident::new(name)]), vec!["7".into()]);
    match kind {
        CurrentTime::SysDateTimeOffset => custom("DATETIMEOFFSET"),
        CurrentTime::SysUtcDateTime | CurrentTime::SysDateTime => custom("DATETIME2"),
        CurrentTime::GetUtcDate | CurrentTime::GetDate => DataType::Datetime(None),
    }
}
/// A statement's clock reading, supplied by the shell: 100-nanosecond ticks
/// since the Unix epoch (UTC) and the local UTC offset in minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub utc_ticks: i64,
    pub offset_minutes: i16,
}
const TICKS_PER_SECOND: i64 = 10_000_000;
/// `yyyy-mm-dd hh:mm:ss` for whole seconds since the Unix epoch, using the
/// proleptic Gregorian calendar (days-to-civil conversion).
fn civil(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let second = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        second / 3600,
        second / 60 % 60,
        second % 60
    )
}
/// Lower a current-time function to a typed literal of the statement's
/// clock. `datetime` values are rounded to SQL Server's 1/300 second.
pub fn current_time_value(kind: CurrentTime, clock: Clock) -> Expr {
    let offset = i64::from(clock.offset_minutes) * 60 * TICKS_PER_SECOND;
    let ticks = match kind {
        CurrentTime::SysUtcDateTime | CurrentTime::GetUtcDate => clock.utc_ticks,
        _ => clock.utc_ticks + offset,
    };
    let text = match kind {
        CurrentTime::GetUtcDate | CurrentTime::GetDate => {
            let units = (i128::from(ticks) * 300 + i128::from(TICKS_PER_SECOND) / 2)
                .div_euclid(i128::from(TICKS_PER_SECOND)) as i64;
            let milliseconds = (units.rem_euclid(300) * 10 + 1) / 3;
            format!("{}.{milliseconds:03}", civil(units.div_euclid(300)))
        }
        _ => {
            let fraction = ticks.rem_euclid(TICKS_PER_SECOND);
            let local = format!(
                "{}.{fraction:07}",
                civil(ticks.div_euclid(TICKS_PER_SECOND))
            );
            if kind == CurrentTime::SysDateTimeOffset {
                let minutes = clock.offset_minutes.unsigned_abs();
                let sign = if clock.offset_minutes < 0 { '-' } else { '+' };
                format!("{local} {sign}{:02}:{:02}", minutes / 60, minutes % 60)
            } else {
                local
            }
        }
    };
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(Value::SingleQuotedString(text).into())),
        data_type: current_time_type(kind),
        format: None,
    }
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
    if let Some(kind) = current_time(function) {
        Some(current_time_type(kind))
    } else if is_xact_state(function) {
        Some(DataType::SmallInt(None))
    } else if let Some(database) = database_function(function) {
        Some(database_type(database))
    } else if is_original_login(function) {
        Some(login_type())
    } else if is_login_name(function) {
        Some(login_name_type())
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
    if is_current_timestamp(function) {
        // CURRENT_TIMESTAMP takes no parentheses.
        return match function.args {
            FunctionArguments::None if function.over.is_none() => Ok(()),
            _ => Err(SqlError::syntax(102, 1, "Incorrect syntax near ')'.")),
        };
    }
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
    } else if database_function(function).is_some()
        || (is_login_name(function) && name.value.eq_ignore_ascii_case("SUSER_SNAME"))
    {
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
    fn current_time_functions_use_captured_types_and_clock() {
        for (call, expected) in [
            ("SYSDATETIMEOFFSET()", "DATETIMEOFFSET(7)"),
            ("SYSUTCDATETIME()", "DATETIME2(7)"),
            ("SYSDATETIME()", "DATETIME2(7)"),
            ("GETUTCDATE()", "DATETIME"),
            ("getdate()", "DATETIME"),
            ("CURRENT_TIMESTAMP", "DATETIME"),
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
            assert_eq!(
                crate::result_properties::expression(&Expr::Function(function.clone()), &[], &[]),
                msduck_core::result::Properties::expression(false),
                "{call}"
            );
        }
        for (call, number, state, message) in [
            (
                "GETUTCDATE(1)",
                174,
                1,
                "The getutcdate function requires 0 argument(s).",
            ),
            (
                "SYSDATETIMEOFFSET(1)",
                174,
                1,
                "The sysdatetimeoffset function requires 0 argument(s).",
            ),
            ("CURRENT_TIMESTAMP()", 102, 1, "Incorrect syntax near ')'."),
        ] {
            let statements =
                crate::batch::parse(&format!("INSERT INTO t VALUES(1); IF 1=0 SELECT {call}"))
                    .unwrap();
            let error = crate::preflight::variables(&statements, &HashMap::new()).unwrap_err();
            let diagnostic = error.downcast_ref::<SqlError>().unwrap();
            assert_eq!(
                (
                    diagnostic.number,
                    diagnostic.state,
                    diagnostic.message.as_str()
                ),
                (number, state, message),
                "{call}"
            );
        }
        // 2026-09-30 10:20:30.1234567 UTC, local offset -05:30.
        let clock = Clock {
            utc_ticks: 1_790_763_630 * 10_000_000 + 1_234_567,
            offset_minutes: -330,
        };
        for (kind, expected) in [
            (
                CurrentTime::SysUtcDateTime,
                "CAST('2026-09-30 10:20:30.1234567' AS DATETIME2(7))",
            ),
            (
                CurrentTime::SysDateTime,
                "CAST('2026-09-30 04:50:30.1234567' AS DATETIME2(7))",
            ),
            (
                CurrentTime::SysDateTimeOffset,
                "CAST('2026-09-30 04:50:30.1234567 -05:30' AS DATETIMEOFFSET(7))",
            ),
            (
                CurrentTime::GetUtcDate,
                "CAST('2026-09-30 10:20:30.123' AS DATETIME)",
            ),
            (
                CurrentTime::GetDate,
                "CAST('2026-09-30 04:50:30.123' AS DATETIME)",
            ),
        ] {
            assert_eq!(current_time_value(kind, clock).to_string(), expected);
        }
        // datetime rounds to 1/300 second, carrying into the next second.
        let at = |fraction: i64| Clock {
            utc_ticks: 1_790_763_630 * 10_000_000 + fraction,
            offset_minutes: 0,
        };
        for (fraction, expected) in [
            (0, "10:20:30.000"),
            (20_000, "10:20:30.003"),
            (50_000, "10:20:30.007"),
            (9_990_000, "10:20:31.000"),
        ] {
            assert_eq!(
                current_time_value(CurrentTime::GetUtcDate, at(fraction)).to_string(),
                format!("CAST('2026-09-30 {expected}' AS DATETIME)")
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
