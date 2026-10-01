//! Calls: user procedures, `EXEC (string)` and sp_executesql.
use super::super::{Exec, Partial, modules};
use super::run::{self, Failure, Finished};
use crate::engine::{Parameter, Session, StatementErrors};
use anyhow::{Result, bail};
use msduck_core::{diagnostic::SqlError, types::Type as SqlType};
use msduck_sql::dialect::ext::procedures::{Argument, Call, Definition, Target, call};
use sqlparser::ast::{DataType, Expr, Ident, Statement, Value};
use std::collections::HashMap;

/// A nested call aborted the batch; its error has been sent. Frames pass it
/// outward; the outermost call turns it into an error the engine ends the
/// batch with.
#[derive(Debug)]
pub(super) struct Aborted;
impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("batch aborted")
    }
}
impl std::error::Error for Aborted {}

pub(super) fn exec(
    session: &mut Session,
    statement: &Statement,
    variables: &mut HashMap<String, Parameter>,
) -> Option<Result<Exec>> {
    let call = call(statement)?;
    let parts = match call.target {
        Target::Dynamic(expr) => return Some(dynamic(session, &call, expr, variables)),
        Target::Name(name) => super::define::identifiers(name)?,
        Target::Variable(variable) => {
            let value = match variables.get(&variable.to_lowercase()) {
                Some(Parameter {
                    value: msduck_core::value::Value::Text(text),
                    ..
                }) => text.clone(),
                Some(_) => {
                    return Some(Err(SqlError::new(
                        2812,
                        62,
                        "Could not find stored procedure ''.",
                    )
                    .into()));
                }
                None => return None,
            };
            let dialect = msduck_sql::dialect::ServerDialect;
            match sqlparser::parser::Parser::new(&dialect)
                .try_with_sql(&value)
                .and_then(|mut parser| {
                    let name = parser.parse_object_name(false)?;
                    // The whole value must be a name.
                    parser.expect_token(&sqlparser::tokenizer::Token::EOF)?;
                    Ok(name)
                })
                .ok()
            {
                Some(name) => super::define::identifiers(&name)?,
                None => {
                    return Some(Err(not_found(&value).into()));
                }
            }
        }
    };
    let last = parts.last()?;
    let system_schema = parts.len() < 2 || {
        let schema = &parts[parts.len() - 2];
        schema.is_empty() || schema.eq_ignore_ascii_case("sys")
    };
    if last.eq_ignore_ascii_case("sp_executesql") && system_schema {
        return Some(execute_sql(session, &call, variables));
    }
    // System procedures (sp_, xp_) that are not user modules belong to
    // other features or the engine.
    let system = ["sp_", "xp_"]
        .iter()
        .any(|prefix| last.to_lowercase().starts_with(prefix));
    let (schema, name) = match super::define::split(session, &parts) {
        Ok(Some(found)) => found,
        Ok(None) => return None,
        Err(_) if system => return None,
        Err(error) => return Some(Err(error)),
    };
    let module = match modules::find(&session.db, schema.as_deref(), &name) {
        Ok(module) => module,
        Err(error) => return Some(Err(error)),
    };
    match module {
        Some(module) if module.type_code == modules::kind::PROCEDURE => {
            Some(procedure(session, &call, &module, variables))
        }
        // Other features run other module types and system procedures.
        Some(_) => None,
        None if system => None,
        None => Some(Err(not_found(&parts.join(".")).into())),
    }
}

fn not_found(name: &str) -> SqlError {
    SqlError::new(
        2812,
        62,
        format!("Could not find stored procedure '{name}'."),
    )
}

/// Values for a new frame and the caller variables that receive OUTPUT
/// parameters afterwards.
struct Bound {
    values: HashMap<String, Parameter>,
    outputs: Vec<(String, String)>,
}

fn type_name(data_type: &DataType) -> String {
    let text = data_type.to_string().to_lowercase();
    let base = text.split('(').next().unwrap_or(&text).trim().to_string();
    match base.as_str() {
        "decimal" => "numeric".into(),
        "integer" => "int".into(),
        _ => base,
    }
}

/// The SQL Server type name of an argument, for error 8114.
fn argument_type(expr: &Expr, variables: &HashMap<String, Parameter>) -> String {
    match expr {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(_) => "varchar".into(),
            Value::NationalStringLiteral(_) => "nvarchar".into(),
            Value::HexStringLiteral(_) => "varbinary".into(),
            Value::Number(number, _) => {
                if number.contains(['e', 'E']) {
                    "float".into()
                } else if number.contains('.') || number.parse::<i32>().is_err() {
                    "numeric".into()
                } else {
                    "int".into()
                }
            }
            _ => "int".into(),
        },
        Expr::UnaryOp { expr, .. } => argument_type(expr, variables),
        Expr::Identifier(ident) => variables
            .get(&ident.value.to_lowercase())
            .map(|parameter| type_name(&parameter.ast_type()))
            .unwrap_or_else(|| "int".into()),
        _ => "int".into(),
    }
}

/// Evaluate an argument for a parameter of `data_type`. A failed conversion
/// is SQL Server's 8114 for arguments.
fn convert(
    session: &Session,
    expr: &Expr,
    data_type: SqlType,
    variables: &HashMap<String, Parameter>,
) -> Result<Parameter> {
    // Arguments are constants and variables; an expression is a syntax error.
    let mut inner = expr;
    if let Expr::UnaryOp { expr, .. } = inner {
        inner = expr;
    }
    if let Expr::BinaryOp { op, .. } = inner {
        bail!(SqlError::syntax(
            102,
            1,
            format!("Incorrect syntax near '{op}'.")
        ));
    }
    let target = msduck_sql::sql_type::ast(data_type);
    let value = session
        .evaluate_scalar(expr.clone(), target.clone(), variables)
        .and_then(crate::backend_value::from_backend)
        .map_err(|_| {
            SqlError::new(
                8114,
                1,
                format!(
                    "Error converting data type {} to {}.",
                    argument_type(expr, variables),
                    type_name(&target)
                ),
            )
        })?;
    Ok(Parameter { value, data_type })
}

/// Bind a call's arguments to a procedure's parameters.
fn bind(
    session: &Session,
    call: &Call<'_>,
    definition: &Definition,
    name: &str,
    caller: &HashMap<String, Parameter>,
) -> Result<Bound> {
    let parameters = &definition.parameters;
    if call.arguments.len() > parameters.len() {
        bail!(SqlError::new(
            8144,
            2,
            format!("Procedure or function {name} has too many arguments specified.")
        ));
    }
    let mut assigned: Vec<Option<&Argument<'_>>> = vec![None; parameters.len()];
    for (index, argument) in call.arguments.iter().enumerate() {
        match argument.name {
            None => {
                if index >= parameters.len() {
                    bail!(SqlError::new(
                        8144,
                        2,
                        format!("Procedure or function {name} has too many arguments specified.")
                    ));
                }
                assigned[index] = Some(argument);
            }
            Some(argument_name) => {
                let Some(position) = parameters
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(argument_name))
                else {
                    bail!(SqlError::new(
                        8145,
                        1,
                        format!("{argument_name} is not a parameter for procedure {name}.")
                    ));
                };
                if assigned[position].is_some() {
                    bail!(SqlError::new(
                        8143,
                        1,
                        format!(
                            "Parameter '{}' was supplied multiple times.",
                            parameters[position].name
                        )
                    ));
                }
                assigned[position] = Some(argument);
            }
        }
    }
    let mut bound = Bound {
        values: HashMap::new(),
        outputs: Vec::new(),
    };
    let empty = HashMap::new();
    for (parameter, argument) in parameters.iter().zip(assigned) {
        let key = parameter.name.to_lowercase();
        let data_type = msduck_sql::batch::variable_type(parameter.data_type.clone())?;
        if argument.is_some_and(|argument| argument.output) && !parameter.output {
            bail!(SqlError::new(
                8162,
                2,
                format!(
                    "The formal parameter \"{}\" was not declared as an OUTPUT parameter, but the actual parameter passed in requested output.",
                    parameter.name
                )
            ));
        }
        let value = match argument.and_then(|argument| argument.value) {
            Some(value) => convert(session, value, data_type, caller)?,
            None => match &parameter.default {
                Some((default, _)) => convert(session, default, data_type, &empty)?,
                None => bail!(SqlError::new(
                    201,
                    4,
                    format!(
                        "Procedure or function '{name}' expects parameter '{}', which was not supplied.",
                        parameter.name
                    )
                )),
            },
        };
        if let Some(argument) = argument
            && argument.output
            && let Some(Expr::Identifier(variable)) = argument.value
        {
            bound
                .outputs
                .push((key.clone(), variable.value.to_lowercase()));
        }
        bound.values.insert(key, value);
    }
    Ok(bound)
}

/// Copy OUTPUT parameters and the status into the caller's variables.
fn write_back(
    session: &Session,
    call: &Call<'_>,
    outputs: &[(String, String)],
    frame: &HashMap<String, Parameter>,
    status: Option<i32>,
    caller: &mut HashMap<String, Parameter>,
) -> Result<()> {
    for (parameter, variable) in outputs {
        let Some(target) = caller.get(variable).map(|p| p.data_type) else {
            continue;
        };
        let target_type = msduck_sql::sql_type::ast(target);
        let value = session
            .evaluate_scalar(
                Expr::Identifier(Ident::new(parameter)),
                target_type.clone(),
                frame,
            )
            .and_then(crate::backend_value::from_backend)
            .map_err(|_| {
                let source = frame
                    .get(parameter)
                    .map(|p| type_name(&p.ast_type()))
                    .unwrap_or_else(|| "int".into());
                output_conversion(&source, &type_name(&target_type))
            })?;
        caller.insert(
            variable.clone(),
            Parameter {
                value,
                data_type: target,
            },
        );
    }
    if let (Some(status), Some(variable)) = (status, call.status) {
        let variable = variable.to_lowercase();
        if let Some(target) = caller.get(&variable).map(|p| p.data_type) {
            let target_type = msduck_sql::sql_type::ast(target);
            let value = session
                .evaluate_scalar(
                    msduck_sql::expr::number(status),
                    target_type.clone(),
                    &HashMap::new(),
                )
                .and_then(crate::backend_value::from_backend)
                .map_err(|_| output_conversion("int", &type_name(&target_type)))?;
            caller.insert(
                variable,
                Parameter {
                    value,
                    data_type: target,
                },
            );
        }
    }
    Ok(())
}

/// A returned value that does not fit the caller's variable (captured: 8114
/// state 2, which ends the batch).
fn output_conversion(source: &str, target: &str) -> anyhow::Error {
    SqlError::new(
        8114,
        2,
        format!("Error converting data type {source} to {target}."),
    )
    .into()
}

/// End the batch after a failed write-back, keeping the call's output; a
/// caller's CATCH handler receives the error instead.
fn abort_after(
    session: &mut Session,
    call: &Call<'_>,
    out: &[u8],
    error: anyhow::Error,
) -> anyhow::Error {
    if caught_by_caller(session, call) {
        return keep(out, error);
    }
    let mut tokens = out.to_vec();
    session.last_error = crate::engine::emit_error(&mut tokens, &error);
    Partial {
        tokens,
        error: if session.ext.procedures.frames.is_empty() {
            StatementErrors(vec![]).into()
        } else {
            Aborted.into()
        },
    }
    .into()
}

/// Whether this call's errors go to a CATCH handler of some caller.
fn caught_by_caller(session: &Session, call: &Call<'_>) -> bool {
    call.in_try
        || session
            .ext
            .procedures
            .frames
            .last()
            .is_some_and(|frame| frame.in_try)
}

/// Turn a frame's outcome into the hook result.
fn finish(session: &Session, out: Vec<u8>, result: Result<i32, Failure>) -> Result<Exec> {
    match result {
        Ok(status) => Ok(Exec {
            tokens: out,
            status,
        }),
        Err(Failure::Error(error)) => Err(Partial {
            tokens: out,
            error: super::diagnostic(&error).into(),
        }
        .into()),
        Err(Failure::Abort) => Err(Partial {
            tokens: out,
            // The engine ends the batch without sending another error.
            error: if session.ext.procedures.frames.is_empty() {
                StatementErrors(vec![]).into()
            } else {
                Aborted.into()
            },
        }
        .into()),
    }
}

/// Fail with 217 when `levels` more would exceed 32. SQL Server ends the
/// batch, unless a caller's CATCH handler receives the error (captured).
fn nesting(session: &mut Session, call: &Call<'_>, levels: usize) -> Result<()> {
    if session.ext.procedures.level() + levels > super::MAX_NESTING {
        let error = SqlError::new(
            217,
            1,
            "Maximum stored procedure, function, trigger, or view nesting level exceeded (limit 32).",
        );
        if caught_by_caller(session, call) {
            bail!(error);
        }
        let mut tokens = Vec::new();
        crate::tds::sql_error(&mut tokens, &error);
        session.last_error = error.number;
        let aborted: anyhow::Error = if session.ext.procedures.frames.is_empty() {
            StatementErrors(vec![]).into()
        } else {
            Aborted.into()
        };
        bail!(Partial {
            tokens,
            error: aborted
        });
    }
    Ok(())
}

/// The 266 check SQL Server makes after EXECUTE when `@@TRANCOUNT` changed.
fn transaction_count(session: &Session, before: u32) -> Option<SqlError> {
    (session.transactions != before).then(|| {
        SqlError::new(
            266,
            2,
            format!(
                "Transaction count after EXECUTE indicates a mismatching number of BEGIN and COMMIT statements. Previous count = {before}, current count = {}.",
                session.transactions
            ),
        )
    })
}

/// A failure after the frame produced output keeps that output.
fn keep(out: &[u8], error: anyhow::Error) -> anyhow::Error {
    Partial {
        tokens: out.to_vec(),
        error: super::diagnostic(&error).into(),
    }
    .into()
}

fn procedure(
    session: &mut Session,
    call: &Call<'_>,
    module: &modules::Module,
    caller: &mut HashMap<String, Parameter>,
) -> Result<Exec> {
    nesting(session, call, 1)?;
    let definition = msduck_sql::dialect::ext::procedures::definition(&module.definition)
        .ok_or_else(|| anyhow::anyhow!("stored procedure definition is not a procedure"))??;
    let note = |session: &mut Session, error: &SqlError| {
        session.ext.procedures.error_procedure = Some((error.clone(), module.name.clone()));
    };
    let bound = match bind(session, call, &definition, &module.name, caller) {
        Ok(bound) => bound,
        Err(error) => {
            note(session, &super::diagnostic(&error));
            return Err(error);
        }
    };
    let transactions = session.transactions;
    let in_try = caught_by_caller(session, call);
    let mut frame_variables = HashMap::new();
    let (out, result) = run::frame(
        session,
        Some(module.name.clone()),
        in_try,
        1,
        |session, out| {
            let (statements, variables) =
                match super::define::compile(&module.definition, &definition) {
                    Ok(compiled) => compiled,
                    Err(error) => {
                        run::note(session, &error);
                        return Err(Failure::Error(error));
                    }
                };
            frame_variables = variables;
            frame_variables.extend(bound.values.clone());
            run::run(session, &statements, &mut frame_variables, out)
        },
    );
    let result = result.map(|finished| finished.procedure_status());
    if let Ok(status) = result {
        write_back(
            session,
            call,
            &bound.outputs,
            &frame_variables,
            Some(status),
            caller,
        )
        .map_err(|error| abort_after(session, call, &out, error))?;
        if let Some(error) = transaction_count(session, transactions) {
            note(session, &error);
            return Err(keep(&out, error.into()));
        }
    }
    finish(session, out, result)
}

/// Parse dynamic SQL as SQL Server compiles it: syntax, then RETURN with a
/// value (178).
fn compile_dynamic(sql: &str) -> Result<Vec<Statement>> {
    let statements = crate::engine::parse_batch(sql).map_err(super::define::syntax_failure)?;
    if run::returns_value(&statements) {
        bail!(SqlError::syntax(
            178,
            1,
            "A RETURN statement with a return value cannot be used in this context."
        ));
    }
    Ok(statements)
}

/// `EXEC (string)`: a nameless frame with no variables of the caller.
fn dynamic(
    session: &mut Session,
    call: &Call<'_>,
    expr: &Expr,
    caller: &mut HashMap<String, Parameter>,
) -> Result<Exec> {
    nesting(session, call, 1)?;
    let text = session.evaluate_scalar(
        expr.clone(),
        DataType::Nvarchar(Some(sqlparser::ast::CharacterLength::Max)),
        caller,
    )?;
    let text = match crate::backend_value::from_backend(text)? {
        msduck_core::value::Value::Text(text) => text,
        msduck_core::value::Value::Null => String::new(),
        _ => bail!("unsupported dynamic SQL value"),
    };
    let in_try = caught_by_caller(session, call);
    let transactions = session.transactions;
    let (out, result) = run::frame(session, None, in_try, 1, |session, out| {
        if let Some(definition) = msduck_sql::dialect::ext::procedures::definition(&text) {
            return run::define(session, &text, definition, out);
        }
        // Dynamic SQL may also begin with a bare module name.
        let text = if super::define::bare_call(session, &text) {
            format!("EXEC {text}")
        } else {
            text
        };
        let statements = compile_dynamic(&text).map_err(Failure::Error)?;
        let mut variables =
            super::define::preflight(&statements, &HashMap::new()).map_err(Failure::Error)?;
        run::run(session, &statements, &mut variables, out)
    });
    let result = result.map(|finished| finished.procedure_status());
    if let Ok(status) = result {
        write_back(session, call, &[], &HashMap::new(), Some(status), caller)
            .map_err(|error| abort_after(session, call, &out, error))?;
        if let Some(error) = transaction_count(session, transactions) {
            return Err(keep(&out, error.into()));
        }
    }
    finish(session, out, result)
}

fn executesql_text(
    session: &Session,
    expr: Option<&Expr>,
    parameter: &str,
    caller: &HashMap<String, Parameter>,
) -> Result<Option<String>> {
    let wrong = || {
        SqlError::new(
            214,
            2,
            format!("Procedure expects parameter '{parameter}' of type 'ntext/nchar/nvarchar'."),
        )
    };
    let Some(expr) = expr else {
        return Ok(None);
    };
    let unicode = match expr {
        Expr::Value(value) => matches!(value.value, Value::NationalStringLiteral(_)),
        Expr::Identifier(ident) => caller.get(&ident.value.to_lowercase()).is_some_and(|p| {
            matches!(
                type_name(&p.ast_type()).as_str(),
                "nvarchar" | "nchar" | "ntext"
            )
        }),
        _ => false,
    };
    if !unicode {
        bail!(wrong());
    }
    let value = session.evaluate_scalar(
        expr.clone(),
        DataType::Nvarchar(Some(sqlparser::ast::CharacterLength::Max)),
        caller,
    )?;
    match crate::backend_value::from_backend(value)? {
        msduck_core::value::Value::Text(text) => Ok(Some(text)),
        msduck_core::value::Value::Null if parameter == "@params" => Ok(None),
        _ => bail!(wrong()),
    }
}

/// `EXEC sp_executesql @stmt [, @params [, values...]]` in a SQL batch.
fn execute_sql(
    session: &mut Session,
    call: &Call<'_>,
    caller: &mut HashMap<String, Parameter>,
) -> Result<Exec> {
    nesting(session, call, 2)?;
    let mut statement = None;
    let mut declarations = None;
    let mut values = Vec::new();
    for (index, argument) in call.arguments.iter().enumerate() {
        match (index, argument.name.map(str::to_lowercase).as_deref()) {
            (0, None) | (_, Some("@stmt")) => statement = argument.value,
            (1, None) | (_, Some("@params")) => declarations = argument.value,
            _ => values.push(argument),
        }
    }
    let Some(sql) = executesql_text(session, statement, "@statement", caller)? else {
        bail!(SqlError::new(
            214,
            2,
            "Procedure expects parameter '@statement' of type 'ntext/nchar/nvarchar'."
        ));
    };
    let declared_text = executesql_text(session, declarations, "@params", caller)?;
    let declared = match &declared_text {
        Some(text) => {
            msduck_sql::batch::declared_parameters(text).map_err(super::define::syntax_failure)?
        }
        None => Vec::new(),
    };
    let too_many = || {
        SqlError::new(
            8144,
            2,
            "Procedure or function  has too many arguments specified.",
        )
    };
    let mut assigned: Vec<Option<&Argument<'_>>> = vec![None; declared.len()];
    let mut positional = 0;
    // Captured: a missing declared parameter (8178) is reported before an
    // extra argument (8144).
    let mut extra = false;
    for argument in &values {
        let position = match argument.name {
            None => {
                positional += 1;
                Some(positional - 1)
            }
            Some(name) => declared
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(name)),
        };
        match position {
            Some(position) if position < declared.len() => assigned[position] = Some(argument),
            _ => extra = true,
        }
    }
    let mut bound = Bound {
        values: HashMap::new(),
        outputs: Vec::new(),
    };
    for (parameter, argument) in declared.iter().zip(&assigned) {
        let Some(argument) = argument.filter(|argument| argument.value.is_some()) else {
            bail!(SqlError::new(
                8178,
                1,
                format!(
                    "The parameterized query '({}){sql}' expects the parameter '{}', which was not supplied.",
                    declared_text.as_deref().unwrap_or_default(),
                    parameter.name
                )
            ));
        };
        if argument.output && !parameter.output {
            bail!(SqlError::new(
                8162,
                2,
                format!(
                    "The formal parameter \"{}\" was not declared as an OUTPUT parameter, but the actual parameter passed in requested output.",
                    parameter.name
                )
            ));
        }
        let value = argument.value.expect("filtered");
        if extra {
            continue;
        }
        bound.values.insert(
            parameter.name.clone(),
            convert(session, value, parameter.data_type, caller)?,
        );
        if argument.output
            && let Expr::Identifier(variable) = value
        {
            bound
                .outputs
                .push((parameter.name.clone(), variable.value.to_lowercase()));
        }
    }
    if extra {
        bail!(too_many());
    }
    let in_try = caught_by_caller(session, call);
    let transactions = session.transactions;
    let mut frame_variables = HashMap::new();
    let (out, result) = run::frame(session, None, in_try, 2, |session, out| {
        if let Some(definition) = msduck_sql::dialect::ext::procedures::definition(&sql) {
            if !declared.is_empty() {
                return Err(Failure::Error(super::define::nested_create().into()));
            }
            return run::define(session, &sql, definition, out);
        }
        let statements = compile_dynamic(&sql).map_err(Failure::Error)?;
        frame_variables =
            super::define::preflight(&statements, &bound.values).map_err(Failure::Error)?;
        run::run(session, &statements, &mut frame_variables, out)
    });
    let result = result.map(|finished: Finished| finished.error_status);
    let written = match &result {
        Ok(status) => write_back(
            session,
            call,
            &bound.outputs,
            &frame_variables,
            Some(*status),
            caller,
        ),
        // sp_executesql still reports a compilation error's number as its
        // status.
        Err(Failure::Error(error)) => {
            let number = super::diagnostic(error).number;
            write_back(session, call, &[], &HashMap::new(), Some(number), caller)
        }
        Err(Failure::Abort) => Ok(()),
    };
    written.map_err(|error| abort_after(session, call, &out, error))?;
    if result.is_ok()
        && let Some(error) = transaction_count(session, transactions)
    {
        return Err(keep(&out, error.into()));
    }
    finish(session, out, result)
}
