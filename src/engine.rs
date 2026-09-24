//! Session-owned DuckDB execution with AST-based T-SQL translation.
mod joined_output;
use crate::tds::{self, Column, Type};
use anyhow::{Result, bail, ensure};
use duckdb::{
    Connection,
    arrow::datatypes::DataType as ArrowType,
    types::{TimeUnit, Value},
};
pub use joined_output::{BoundOutputQuery, BoundOutputUpdate, PreparedJoinedOutput};
use msduck_core::diagnostic::SqlError;
pub use msduck_sql::batch::parameter_declarations;
use msduck_sql::batch::{parse as parse_batch, variable_type};
use msduck_sql::expr::number;
pub(crate) use msduck_sql::expr::{binary_function, unary_function};
use msduck_sql::preflight::{
    try_catch_parts, validate_transaction_syntax, variables as batch_variables,
};
pub(crate) use msduck_sql::sql_type::integral_type;
use sqlparser::{ast::*, parser::Parser};
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);

pub use crate::parameter::Parameter;
use msduck_core::{types::Type as SqlType, value::Value as ParameterValue};

fn runtime_diagnostic(message: &str) -> Option<SqlError> {
    crate::json_extract::diagnostic(message)
        .or_else(|| crate::integer_conversion::diagnostic(message))
        .or_else(|| crate::binary_unicode::diagnostic(message))
        .or_else(|| crate::storage_diagnostic::diagnostic(message))
        .or_else(|| crate::query_error::integer_overflow(message))
        .or_else(|| {
            msduck_core::left_right::diagnostic(
                message
                    .strip_prefix("Invalid Input Error: ")
                    .unwrap_or(message),
            )
        })
        .or_else(|| msduck_sql::recursive_lower::diagnostic(message))
        .or_else(|| crate::money_range::diagnostic(message))
        .or_else(|| {
            msduck_core::diagnostic::numeric(
                message
                    .strip_prefix("Invalid Input Error: ")
                    .unwrap_or(message),
            )
        })
}

fn sql_error_from_message(message: &str) -> SqlError {
    runtime_diagnostic(message).unwrap_or_else(|| SqlError::new(error_number(message), 1, message))
}

pub(crate) fn emit_error(out: &mut Vec<u8>, error: &anyhow::Error) -> i32 {
    if let Some(error) = error.downcast_ref::<SqlError>() {
        tds::sql_error(out, error);
        error.number
    } else {
        let message = error.to_string();
        if let Some(error) = runtime_diagnostic(&message) {
            tds::sql_error(out, &error);
            error.number
        } else {
            let number = error_number(&message);
            tds::error(out, number, &message);
            number
        }
    }
}

// Control boundaries carry no row count and do not change session counters.
// SQL batches retain them under NOCOUNT; RPCs suppress non-result completions.
fn control_done(out: &mut Vec<u8>, rpc: bool, nocount: bool, command: u16) -> Option<usize> {
    if rpc && nocount {
        return None;
    }
    let offset = out.len();
    tds::done(out, if rpc { 0xff } else { 0xfd }, 1, command, 0);
    Some(offset)
}

fn binding_failure(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<crate::query_error::CompilationFailure>()
        .is_some()
    {
        return true;
    }
    let message = error.to_string();
    let number = sql_error_from_message(&message).number;
    error.downcast_ref::<SqlError>().is_none()
        && (matches!(number, 102 | 137 | 208 | 8117 | 40515)
            || message.contains("Binder Error")
            || message.contains("Parser Error")
            || message.contains("Catalog Error"))
}

// Result-set presence is recorded when metadata is produced, including an
// empty result. It is independent of row counts and encoded token bytes.
struct Execution {
    tokens: Vec<u8>,
    count: Option<u64>,
    command: u16,
    kind: msduck_core::completion::Kind,
}
impl Execution {
    fn statement(tokens: Vec<u8>, count: Option<u64>, command: u16) -> Self {
        Self {
            tokens,
            count,
            command,
            kind: msduck_core::completion::Kind::Statement,
        }
    }
    fn result_set(tokens: Vec<u8>, count: Option<u64>, command: u16) -> Self {
        Self {
            tokens,
            count,
            command,
            kind: msduck_core::completion::Kind::ResultSet,
        }
    }
}

// Command identities captured from SQL Server for successful DDL. Backend
// affected-row counts do not represent SQL Server DDL completion row counts.
fn ddl_completion_command(statement: &Statement) -> Option<u16> {
    Some(match statement {
        Statement::CreateTable(_) => 198,
        Statement::CreateView(_) | Statement::AlterView { .. } => 207,
        Statement::CreateIndex(_) => 200,
        Statement::AlterTable(_) => 216,
        Statement::Truncate(_) => 234,
        Statement::CreateSchema { .. } => 253,
        Statement::Drop { object_type, .. } => match object_type {
            ObjectType::Table => 199,
            ObjectType::View => 208,
            ObjectType::Index => 201,
            ObjectType::Schema => 253,
            _ => return None,
        },
        _ => return None,
    })
}

struct Work<'a> {
    statement: &'a Statement,
    loop_boundary: bool,
    catch_handler: Option<&'a [Statement]>,
    restore_error: Option<Option<SqlError>>,
    completion: Option<u16>,
}
impl<'a> Work<'a> {
    fn leaf(statement: &'a Statement) -> Self {
        Self {
            statement,
            loop_boundary: false,
            catch_handler: None,
            restore_error: None,
            completion: None,
        }
    }
}
pub struct Session {
    pub db: Connection,
    diagnostics: crate::statement_diagnostics::Registry,
    pub nocount: bool,
    pub transactions: u32,
    pub rowcount: u64,
    pub last_error: i32,
    pub transaction_descriptor: u64,
    pub original_login: String,
    transaction_name: String,
    xact_abort: bool,
    ansi_warnings: bool,
    transaction_doomed: bool,
    caught_error: Option<SqlError>,
}
impl Session {
    pub fn new(connection: crate::server::Connection) -> Result<Self> {
        let (db, diagnostics) = connection.into_parts();
        db.execute_batch("SET schema = 'dbo'; SET arrow_lossless_conversion = true; SET VARIABLE __msduck_datefirst = 7")?;
        db.execute_batch(
            "CREATE TEMP MACRO __msduck_time_round(value, quantum) AS
             CAST(substr(CAST(make_timestamp_ns(
                 (((epoch_ns(value) + quantum // 2) // quantum) * quantum)
                 % 86400000000000
             ) AS VARCHAR), 12) AS TIME_NS)",
        )?;
        db.execute_batch("CREATE TEMP MACRO __msduck_int_div(a,b) AS CASE WHEN b=0 THEN error('Divide by zero error encountered.') ELSE a // b END;
            CREATE TEMP MACRO __msduck_int_mod(a,b) AS CASE WHEN b=0 THEN error('Divide by zero error encountered.') ELSE a % b END")?;
        Ok(Self {
            db,
            diagnostics,
            nocount: false,
            transactions: 0,
            rowcount: 0,
            last_error: 0,
            transaction_descriptor: 0,
            original_login: "sa".into(),
            transaction_name: String::new(),
            xact_abort: false,
            ansi_warnings: true,
            transaction_doomed: false,
            caught_error: None,
        })
    }

    /// Open an explicit context for one execution. Preparing SQL must not open
    /// or mutate a current-session context in the shared native catalog.
    pub fn diagnostic_scope(&self) -> Result<crate::statement_diagnostics::Scope> {
        self.diagnostics.begin().map_err(anyhow::Error::msg)
    }
    pub fn batch(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Vec<u8> {
        self.batch_response(sql, parameters, rpc, None).0
    }
    /// Compile without stepping statements. Preparation must never execute DML.
    pub fn validate_prepared_sql(
        &self,
        sql: &str,
        declarations: &[(String, SqlType)],
    ) -> Result<()> {
        self.validate_prepared_statements(parse_batch(sql)?, declarations)
    }
    fn validate_prepared_statements(
        &self,
        statements: Vec<Statement>,
        declarations: &[(String, SqlType)],
    ) -> Result<()> {
        let parameters = declarations
            .iter()
            .map(|(name, kind)| {
                (
                    name.clone(),
                    Parameter {
                        value: ParameterValue::Null,
                        data_type: *kind,
                    },
                )
            })
            .collect();
        let parameters = batch_variables(&statements, &parameters)?;
        let mut pending = statements.into_iter().rev().collect::<Vec<_>>();
        while let Some(mut statement) = pending.pop() {
            crate::query_catalog::validate_cte_columns(&self.db, &statement)?;
            if let Some((body, handler)) = try_catch_parts(&statement) {
                pending.extend(handler.iter().rev().cloned());
                pending.extend(body.iter().rev().cloned());
                continue;
            }
            if crate::dialect::loop_control(&statement).is_some() {
                continue; // Placement was checked by batch_variables.
            }
            if msduck_sql::drop_index_syntax::request(&statement).is_some() {
                continue; // Bind table/index identities only when executed.
            }
            if let Some(call) = msduck_sql::raiserror::call(&statement) {
                for expression in [call.message, call.severity, call.state]
                    .into_iter()
                    .chain(call.substitutions)
                {
                    msduck_sql::raiserror::argument(expression, &parameters)?;
                }
                continue;
            }
            match &statement {
                Statement::If(condition) => {
                    self.validate_prepared_predicate(
                        condition
                            .if_block
                            .condition
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("missing IF condition"))?,
                        &parameters,
                    )?;
                    if let Some(block) = &condition.else_block {
                        pending.extend(block.statements().iter().rev().cloned());
                    }
                    pending.extend(condition.if_block.statements().iter().rev().cloned());
                    continue;
                }
                Statement::While(condition) => {
                    self.validate_prepared_predicate(
                        condition
                            .while_block
                            .condition
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("missing WHILE condition"))?,
                        &parameters,
                    )?;
                    pending.extend(condition.while_block.statements().iter().rev().cloned());
                    continue;
                }
                Statement::StartTransaction {
                    has_end_keyword: true,
                    statements,
                    ..
                } => {
                    pending.extend(statements.iter().rev().cloned());
                    continue;
                }
                Statement::Return(value) => {
                    if let Some(ReturnStatementValue::Expr(expression)) = &value.value {
                        self.validate_prepared_scalar(
                            expression.clone(),
                            DataType::Int(None),
                            &parameters,
                        )?;
                    }
                    continue;
                }
                Statement::Print(print) => {
                    self.validate_prepared_scalar(
                        *print.message.clone(),
                        print_target(&print.message, &parameters),
                        &parameters,
                    )?;
                    continue;
                }
                Statement::StartTransaction { .. }
                | Statement::Commit { .. }
                | Statement::Rollback { .. } => {
                    continue; // Syntax was checked; transaction state changes only on execution.
                }
                Statement::Throw(throw) => {
                    if let (Some(number), Some(message), Some(state)) =
                        (&throw.error_number, &throw.message, &throw.state)
                    {
                        self.validate_prepared_scalar(
                            *number.clone(),
                            DataType::Int(None),
                            &parameters,
                        )?;
                        self.validate_prepared_scalar(
                            *message.clone(),
                            DataType::Varchar(None),
                            &parameters,
                        )?;
                        self.validate_prepared_scalar(
                            *state.clone(),
                            DataType::Int(None),
                            &parameters,
                        )?;
                    }
                    continue;
                }
                _ => {}
            }
            if let Statement::Declare { stmts } = &statement {
                for declaration in stmts {
                    let kind = variable_type(
                        declaration
                            .data_type
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("missing variable type"))?,
                    )?;
                    let expression = match &declaration.assignment {
                        None => Expr::Value(sqlparser::ast::Value::Null.into()),
                        Some(DeclareAssignment::MsSqlAssignment(value)) => *value.clone(),
                        _ => bail!("unsupported variable initializer"),
                    };
                    self.validate_prepared_scalar(
                        expression,
                        crate::sql_type::ast(kind),
                        &parameters,
                    )?;
                }
                continue;
            }
            if matches!(&statement, Statement::Set(_))
                && !matches!(&statement, Statement::Set(Set::SingleAssignment { variable, .. }) if variable.to_string().starts_with('@'))
            {
                if let Some(value) =
                    crate::datepart::setting(&statement).map_err(anyhow::Error::msg)?
                {
                    self.validate_prepared_scalar(value, DataType::Int(None), &parameters)?;
                } else {
                    session_setting(&statement)?;
                }
                continue;
            }
            if let Statement::Set(Set::SingleAssignment {
                scope,
                hivevar,
                variable,
                values,
            }) = &statement
            {
                let name = variable.to_string().to_lowercase();
                ensure!(
                    name.starts_with('@') && scope.is_none() && !hivevar && values.len() == 1,
                    "unsupported prepared variable assignment"
                );
                let kind = parameters
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
                    .data_type;
                self.validate_prepared_scalar(
                    values[0].clone(),
                    crate::sql_type::ast(kind),
                    &parameters,
                )?;
                continue;
            }
            ensure!(
                matches!(
                    statement,
                    Statement::Query(_)
                        | Statement::Insert(_)
                        | Statement::Update { .. }
                        | Statement::Delete(_)
                ),
                "unsupported prepared statement kind"
            );
            // Lower assignments exactly as execution does, but only bind the
            // resulting query. Preparation must not evaluate or store values.
            if let Some((update, with)) = msduck_sql::output::joined_update(&statement) {
                self.plan_joined_execution(update, with.cloned(), &parameters)?;
                continue;
            }
            if let Statement::Query(query) = &mut statement {
                crate::query_catalog::bind_query_with_parameters(&self.db, query, &parameters)?;
            }
            self.lower_output(&mut statement, &parameters)?;
            self.bind_dml(&mut statement, &parameters)?;
            crate::query_catalog::lower_recursion(&self.db, &mut statement)?;
            let json = crate::for_json::Output::take(&self.db, &mut statement)?;
            select_assignments(&mut statement, &parameters)?;
            let into = crate::select_into::take(&mut statement)?;
            let money_columns = crate::insert::money_columns(&statement, &parameters);
            crate::update::expand_compound(&self.db, &mut statement)?;
            let money_assignments = crate::update::money_assignments(&statement, &parameters);
            crate::aggregate_columns::annotate(&self.db, &mut statement, &parameters)
                .map_err(anyhow::Error::msg)?;
            crate::query_catalog::bind_unicode_operations(&self.db, &mut statement, &parameters)?;
            crate::concat_lower::annotated_unicode_casts(&mut statement);
            crate::for_json::lower_nested(&self.db, &mut statement, &parameters)?;
            let mut translator = Translator {
                parameters: &parameters,
                values: Vec::new(),
                parameter_slots: HashMap::new(),
                transactions: self.transactions,
                transaction_doomed: self.transaction_doomed,
                original_login: &self.original_login,
                rowcount: self.rowcount,
                last_error: self.last_error,
                caught_error: self.caught_error.as_ref(),
            };
            if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut translator) {
                bail!(error);
            }
            crate::insert::lower(&self.db, &mut statement, &money_columns)?;
            crate::update::lower(&self.db, &mut statement, &money_assignments)?;
            if let Some(target) = into {
                crate::select_into::definition(
                    &self.db,
                    &target,
                    &statement.to_string(),
                    &translator.values,
                )?;
            }
            self.db.prepare(&statement.to_string())?;
            if let Some(json) = json {
                let names = crate::for_json::Output::names(
                    &self.db,
                    &statement.to_string(),
                    &translator.values,
                )?;
                json.plan(&names)?;
            }
        }
        Ok(())
    }
    fn bind_dml(
        &self,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let dml = matches!(statement, Statement::Update(_) | Statement::Delete(_))
            || matches!(statement, Statement::Query(query) if matches!(query.body.as_ref(), SetExpr::Update(_) | SetExpr::Delete(_)));
        if dml {
            crate::aggregate_columns::annotate(&self.db, statement, parameters)
                .map_err(anyhow::Error::msg)?;
            crate::update::canonicalize(statement)?;
            crate::delete::canonicalize(statement)?;
        }
        Ok(())
    }
    fn validate_prepared_scalar(
        &self,
        expression: Expr,
        data_type: DataType,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let expression = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expression),
            data_type,
            format: None,
        };
        self.validate_prepared_expression(expression, parameters)
            .map(|_| ())
    }
    fn validate_prepared_predicate(
        &self,
        expression: Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let kind = self.validate_prepared_expression(expression, parameters)?;
        ensure!(
            kind == duckdb::core::LogicalTypeId::Boolean,
            "An expression of non-boolean type specified in a context where a condition is expected"
        );
        Ok(())
    }
    fn validate_prepared_expression(
        &self,
        mut expression: Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<duckdb::core::LogicalTypeId> {
        crate::query_catalog::lower_recursion(&self.db, &mut expression)?;
        crate::aggregate_columns::annotate(&self.db, &mut expression, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_expression(&self.db, &mut expression, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut expression);
        crate::for_json::lower_nested(&self.db, &mut expression, parameters)?;
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut expression, &mut translator) {
            bail!(error);
        }
        // Bind only: evaluating an initializer here could invoke a volatile
        // function or raise an execution-time error during sp_prepare.
        let prepared = self.db.prepare(&format!("SELECT {expression}"))?;
        // Unlike Arrow schema access, this obtains bound logical metadata
        // directly from the prepared statement without executing it.
        Ok(prepared.column_logical_type(0).id())
    }
    pub fn batch_response(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc: bool,
        handle: Option<(&str, i32)>,
    ) -> (Vec<u8>, bool) {
        let saved_nocount = self.nocount;
        let saved_xact_abort = self.xact_abort;
        let result = self.batch_response_inner(sql, parameters, rpc, handle);
        if rpc {
            self.nocount = saved_nocount;
            self.xact_abort = saved_xact_abort;
        }
        result
    }

    fn batch_response_inner(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc: bool,
        handle: Option<(&str, i32)>,
    ) -> (Vec<u8>, bool) {
        self.caught_error = None;
        let mut out = Vec::new();
        let mut had_runtime_error = false;
        let statements = match parse_batch(sql) {
            Ok(s) => s,
            Err(e) => {
                if e.downcast_ref::<SqlError>().is_some() {
                    self.last_error = emit_error(&mut out, &e);
                    let command = if matches!(self.last_error, 156 | 159) {
                        if rpc {
                            out.push(0x79);
                            out.extend(self.last_error.to_le_bytes());
                            224
                        } else {
                            253
                        }
                    } else {
                        0
                    };
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, command, 0);
                    return (out, false);
                }
                let message = e.to_string();
                self.error(
                    &mut out,
                    crate::merge::error_number(&message)
                        .or_else(|| msduck_sql::query_options::error_number(&message))
                        .unwrap_or(102),
                    &message,
                );
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
        };
        let mut variables = match batch_variables(&statements, parameters) {
            Ok(variables) => variables,
            Err(error) => {
                self.last_error = emit_error(&mut out, &error);
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
        };
        let mut pending: Vec<Work<'_>> = statements.iter().rev().map(Work::leaf).collect();
        let mut steps = 0usize;
        let mut last_done = None;
        let mut return_status = 0i32;
        let mut executed_leaf = false;
        while let Some(work) = pending.pop() {
            steps += 1;
            if steps > 10_000 {
                self.error(
                    &mut out,
                    50000,
                    "batch execution exceeds current 10000-step limit",
                );
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
            if let Some(command) = work.completion {
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, command) {
                    last_done = Some(offset);
                }
                continue;
            }
            if let Some(previous) = work.restore_error {
                self.caught_error = previous;
                // END TRY/CATCH resets @@ROWCOUNT even when no exception was
                // raised. An informational aggregate warning is not an error.
                self.rowcount = 0;
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, 351) {
                    last_done = Some(offset);
                }
                continue;
            }
            let statement = work.statement;
            if let Some((body, handler)) = try_catch_parts(statement) {
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, 349) {
                    last_done = Some(offset);
                }
                pending.push(Work {
                    statement,
                    loop_boundary: false,
                    catch_handler: Some(handler),
                    restore_error: Some(self.caught_error.clone()),
                    completion: None,
                });
                pending.extend(body.iter().rev().map(Work::leaf));
                continue;
            }
            if let Some(is_continue) = crate::dialect::loop_control(statement) {
                if let Some(index) = pending.iter().rposition(|work| work.loop_boundary) {
                    self.unwind_work(&mut pending, index + usize::from(is_continue));
                    if let Some(offset) = control_done(&mut out, rpc, self.nocount, 202) {
                        last_done = Some(offset);
                    }
                    continue;
                }
                self.error(&mut out, 50000, "loop control outside WHILE");
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
            if let Statement::Return(return_statement) = statement {
                let value = match &return_statement.value {
                    None => Ok(Value::Int(self.last_error)),
                    Some(ReturnStatementValue::Expr(expression)) => {
                        self.evaluate_scalar(expression.clone(), DataType::Int(None), &variables)
                    }
                };
                match value {
                    Ok(Value::Int(value)) => return_status = value,
                    Ok(Value::Null) => {
                        tds::info(
                            &mut out,
                            282,
                            "A NULL return status is not allowed; returning 0 instead.",
                        );
                    }
                    Ok(_) => unreachable!("RETURN expression is cast to INT"),
                    Err(error) => {
                        if self.catch_error(&mut pending, &error) {
                            continue;
                        }
                        self.last_error = emit_error(&mut out, &error);
                        self.rollback_doomed(&mut out);
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                }
                self.rowcount = 1;
                self.last_error = 0;
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, 219) {
                    last_done = Some(offset);
                }
                break;
            }
            let branch: Result<Option<&[Statement]>> = match statement {
                Statement::While(while_statement) => {
                    let expression = while_statement
                        .while_block
                        .condition
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing WHILE condition"));
                    expression
                        .and_then(|expression| {
                            self.evaluate_expression(expression, &variables, true)
                        })
                        .map(|value| {
                            if matches!(value, Value::Boolean(true)) {
                                pending.push(Work {
                                    statement,
                                    loop_boundary: true,
                                    catch_handler: None,
                                    restore_error: None,
                                    completion: None,
                                });
                                Some(while_statement.while_block.statements().as_slice())
                            } else {
                                Some(&[][..])
                            }
                        })
                }
                Statement::If(condition) => {
                    let expression = condition
                        .if_block
                        .condition
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing IF condition"));
                    expression
                        .and_then(|expression| {
                            self.evaluate_expression(expression, &variables, true)
                        })
                        .map(|value| {
                            if matches!(value, Value::Boolean(true)) {
                                Some(condition.if_block.statements().as_slice())
                            } else {
                                Some(
                                    condition
                                        .else_block
                                        .as_ref()
                                        .map(|block| block.statements().as_slice())
                                        .unwrap_or(&[]),
                                )
                            }
                        })
                }
                Statement::StartTransaction {
                    has_end_keyword: true,
                    statements,
                    exception,
                    modifier,
                    ..
                } => {
                    if exception.is_some() || modifier.is_some() {
                        Err(anyhow::anyhow!("unsupported exception block"))
                    } else {
                        Ok(Some(statements.as_slice()))
                    }
                }
                _ => Ok(None),
            };
            match branch {
                Ok(Some(statements)) => {
                    // SQL Server completes each condition evaluation, even
                    // when its branch is not selected. BEGIN/END is only a
                    // grouping construct and contributes no completion token.
                    if matches!(statement, Statement::If(_) | Statement::While(_))
                        && !(rpc && self.nocount)
                    {
                        last_done = Some(out.len());
                        tds::done(
                            &mut out,
                            if rpc { 0xff } else { 0xfd },
                            u16::from(!statements.is_empty() || !pending.is_empty() || rpc),
                            0xc0,
                            0,
                        );
                    }
                    pending.extend(statements.iter().rev().map(Work::leaf));
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    if self.catch_error(&mut pending, &error) {
                        if matches!(statement, Statement::If(_) | Statement::While(_))
                            && let Some(offset) = control_done(&mut out, rpc, self.nocount, 0xc0)
                        {
                            last_done = Some(offset);
                        }
                        continue;
                    }
                    self.last_error = emit_error(&mut out, &error);
                    self.rollback_doomed(&mut out);
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                    return (out, false);
                }
            }
            let more = u16::from(!pending.is_empty() || rpc);
            if let Some(call) = msduck_sql::raiserror::call(statement) {
                let raised = match crate::raiserror::bind(&call, &variables) {
                    Ok(raised) => raised,
                    Err(error) => {
                        if self.catch_error(&mut pending, &error) {
                            continue;
                        }
                        self.last_error = emit_error(&mut out, &error);
                        self.rollback_doomed(&mut out);
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                };
                if out.len().saturating_add(4200) > tds::MAX_MESSAGE - 1024 {
                    self.error(
                        &mut out,
                        50000,
                        "batch result exceeds current 16 MiB response limit",
                    );
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                    return (out, false);
                }
                executed_leaf = true;
                self.rowcount = 0;
                let is_error = raised.delivery == msduck_core::raiserror::Delivery::Error;
                if is_error
                    && pending.iter().any(|work| work.catch_handler.is_some())
                    && self.catch_error(&mut pending, &raised.diagnostic.clone().into())
                {
                    if let Some(offset) = control_done(&mut out, rpc, self.nocount, 246) {
                        last_done = Some(offset);
                    }
                    continue;
                }
                self.last_error = raised.error_number;
                if is_error {
                    had_runtime_error = true;
                    return_status = raised.error_number;
                    tds::sql_error(&mut out, &raised.diagnostic);
                } else {
                    let units = raised
                        .diagnostic
                        .message_utf16
                        .clone()
                        .unwrap_or_else(|| raised.diagnostic.message.encode_utf16().collect());
                    tds::diagnostic_utf16(
                        &mut out,
                        tds::DiagnosticKind::Information,
                        raised.diagnostic.severity,
                        raised.diagnostic.state,
                        raised.diagnostic.number,
                        &units,
                    );
                    return_status = 0;
                }
                if !msduck_core::completion::visible(
                    rpc,
                    self.nocount,
                    if is_error {
                        msduck_core::completion::Kind::Error
                    } else {
                        msduck_core::completion::Kind::Statement
                    },
                ) {
                    continue;
                }
                last_done = Some(out.len());
                tds::done(
                    &mut out,
                    if rpc { 0xff } else { 0xfd },
                    more | if is_error { 2 } else { 0 },
                    246,
                    0,
                );
                continue;
            }
            match self.execute(statement.clone(), &mut variables) {
                Ok(Execution {
                    tokens,
                    count,
                    command,
                    kind,
                }) => {
                    executed_leaf = true;
                    if out.len().saturating_add(tokens.len()).saturating_add(13)
                        > tds::MAX_MESSAGE - 1024
                    {
                        self.error(
                            &mut out,
                            50000,
                            "batch result exceeds current 16 MiB response limit",
                        );
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                    out.extend(tokens);
                    self.last_error = 0;
                    if had_runtime_error {
                        return_status = 0;
                    }
                    // A declaration without an initializer changes neither the
                    // affected-row state nor the intermediate completion stream.
                    // The batch epilogue still supplies a final DONE if needed.
                    if matches!(statement, Statement::Declare { stmts }
                        if stmts.iter().all(|declaration| declaration.assignment.is_none()))
                    {
                        continue;
                    }
                    self.rowcount = count.unwrap_or(0);
                    let visible = count.filter(|_| !self.nocount);
                    if (rpc && ddl_completion_command(statement) == Some(253))
                        || !msduck_core::completion::visible(rpc, self.nocount, kind)
                    {
                        continue;
                    }
                    last_done = Some(out.len());
                    tds::done(
                        &mut out,
                        if rpc { 0xff } else { 0xfd },
                        more | if visible.is_some() { 16 } else { 0 },
                        command,
                        visible.unwrap_or(0),
                    );
                }
                Err(e) => {
                    // Boundary events are speculative until the first leaf
                    // binds. A compilation failure must precede those events.
                    let failed_binding = binding_failure(&e);
                    if !executed_leaf && failed_binding {
                        out.clear();
                    }
                    executed_leaf |= !failed_binding;
                    if let Some(failed) = e.downcast_ref::<crate::query_error::FailedQuery>() {
                        if out.len().saturating_add(failed.metadata.len()) > tds::MAX_MESSAGE - 1024
                        {
                            self.error(
                                &mut out,
                                50000,
                                "batch result exceeds current 16 MiB response limit",
                            );
                            tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                            return (out, false);
                        }
                        out.extend_from_slice(&failed.metadata);
                        if matches!(failed.command, 0xc1 | 0xc3..=0xc5) {
                            self.rowcount = 0;
                        }
                    }
                    if self.catch_error(&mut pending, &e) {
                        if let Some(failed) = e.downcast_ref::<crate::output_sink::Failed>() {
                            self.rowcount = 0;
                            if msduck_core::completion::visible(
                                rpc,
                                self.nocount,
                                msduck_core::completion::Kind::Statement,
                            ) {
                                let command = match failed.operation {
                                    msduck_sql::output::Operation::Insert => 0xc3,
                                    msduck_sql::output::Operation::Update => 0xc5,
                                    msduck_sql::output::Operation::Delete => 0xc4,
                                };
                                last_done = Some(out.len());
                                tds::done(
                                    &mut out,
                                    if rpc { 0xff } else { 0xfd },
                                    1 | if self.nocount { 0 } else { 16 },
                                    command,
                                    0,
                                );
                            }
                        }
                        if matches!(statement, Statement::Commit { .. })
                            && let Some(offset) = control_done(&mut out, rpc, self.nocount, 0)
                        {
                            last_done = Some(offset);
                        }
                        if matches!(statement, Statement::Throw(_))
                            && let Some(offset) = control_done(&mut out, rpc, self.nocount, 246)
                        {
                            last_done = Some(offset);
                        }
                        if let Some(failed) = e.downcast_ref::<crate::query_error::FailedQuery>() {
                            tds::done(
                                &mut out,
                                if rpc { 0xff } else { 0xfd },
                                1 | if self.nocount { 0 } else { 16 },
                                failed.command,
                                0,
                            );
                        }
                        continue;
                    }
                    self.last_error = emit_error(&mut out, &e);
                    if self.transaction_doomed {
                        self.rollback_doomed(&mut out);
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                    let failed_dml =
                        e.downcast_ref::<crate::query_error::FailedQuery>()
                            .map(|failed| failed.command)
                            .filter(|command| matches!(command, 0xc3..=0xc5))
                            .or_else(|| {
                                e.downcast_ref::<crate::output_sink::Failed>()
                                    .map(|failed| match failed.operation {
                                        msduck_sql::output::Operation::Insert => 0xc3,
                                        msduck_sql::output::Operation::Update => 0xc5,
                                        msduck_sql::output::Operation::Delete => 0xc4,
                                    })
                            });
                    if let Some(command) = failed_dml {
                        tds::diagnostic_utf16(
                            &mut out,
                            tds::DiagnosticKind::Information,
                            0,
                            0,
                            3621,
                            &"The statement has been terminated."
                                .encode_utf16()
                                .collect::<Vec<_>>(),
                        );
                        had_runtime_error = true;
                        return_status = self.last_error;
                        self.rowcount = 0;
                        last_done = Some(out.len());
                        tds::done(
                            &mut out,
                            if rpc { 0xff } else { 0xfd },
                            2 | more,
                            command,
                            0,
                        );
                        continue;
                    }
                    if let Some(failed) = e.downcast_ref::<crate::query_error::FailedQuery>()
                        && matches!(self.last_error, 8115 | 8134)
                    {
                        // Default SQL Server arithmetic errors end this query,
                        // not the batch. Keep the failure visible even if a later
                        // statement succeeds and resets @@ERROR.
                        had_runtime_error = true;
                        return_status = self.last_error;
                        self.rowcount = 0;
                        last_done = Some(out.len());
                        tds::done(
                            &mut out,
                            if rpc { 0xff } else { 0xfd },
                            2 | more,
                            failed.command,
                            0,
                        );
                        continue;
                    }
                    if rpc && msduck_sql::drop_index_syntax::request(statement).is_some() {
                        self.rowcount = 0;
                        if self.last_error == 3701 {
                            // A missing target ends this statement, while later
                            // RPC statements still run, even with NOCOUNT ON.
                            had_runtime_error = true;
                            return_status = self.last_error;
                            last_done = Some(out.len());
                            tds::done(&mut out, 0xff, 3, 201, 0);
                            continue;
                        }
                        if self.last_error == 3748 {
                            tds::done(&mut out, 0xfe, 2, 224, 0);
                            return (out, false);
                        }
                    }
                    let command =
                        if !rpc && msduck_sql::drop_index_syntax::request(statement).is_some() {
                            self.rowcount = 0;
                            if self.last_error == 3748 { 253 } else { 201 }
                        } else if !rpc
                            && matches!(statement, Statement::CreateIndex(_))
                            && self.last_error == 1913
                        {
                            self.rowcount = 0;
                            253
                        } else if !rpc
                            && e.downcast_ref::<crate::query_error::CompilationFailure>()
                                .is_some()
                        {
                            0xfd
                        } else {
                            0
                        };
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, command, 0);
                    return (out, false);
                }
            }
        }
        if self.transaction_doomed {
            if let Some(offset) = last_done {
                out[offset + 1] |= 1;
            }
            self.rollback_doomed(&mut out);
            self.error(&mut out, 3998, "Uncommittable transaction is detected at the end of the batch. The transaction is rolled back.");
            tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
            self.caught_error = None;
            return (out, false);
        }
        if rpc {
            if let Some((name, value)) = handle.filter(|_| !had_runtime_error) {
                tds::return_handle(&mut out, name, value);
            }
            out.push(0x79);
            out.extend(return_status.to_le_bytes());
            tds::done(&mut out, 0xfe, 0, 0xe0, 0);
        } else if let Some(offset) = last_done {
            // A trailing unselected IF may leave the previous leaf marked MORE.
            out[offset + 1] &= !1;
        } else {
            tds::done(&mut out, 0xfd, 0, 0, 0);
        }
        self.caught_error = None;
        (out, !had_runtime_error)
    }
    fn unwind_work(&mut self, pending: &mut Vec<Work<'_>>, length: usize) {
        while pending.len() > length {
            if let Some(previous) = pending.pop().and_then(|work| work.restore_error) {
                self.caught_error = previous;
            }
        }
    }
    fn catch_error<'a>(&mut self, pending: &mut Vec<Work<'a>>, error: &anyhow::Error) -> bool {
        let message = error.to_string();
        let caught = error
            .downcast_ref::<SqlError>()
            .cloned()
            .unwrap_or_else(|| sql_error_from_message(&message));
        // Same-level binding/compilation failures are not runtime catch targets.
        if binding_failure(error) {
            return false;
        }
        // Retain the transaction for CATCH reads and explicit ROLLBACK. The
        // pinned 17.0.4065.4 reference also dooms caught RAISERROR 11/16;
        // informational severity 10 never enters this path.
        if self.xact_abort && self.transactions > 0 && caught.severity >= 11 {
            self.transaction_doomed = true;
        }
        let Some(index) = pending
            .iter()
            .rposition(|work| work.catch_handler.is_some())
        else {
            return false;
        };
        self.unwind_work(pending, index + 1);
        let marker = pending.pop().expect("catch marker exists");
        let handler = marker.catch_handler.expect("catch handler exists");
        self.last_error = caught.number;
        self.caught_error = Some(caught);
        let statement = marker.statement;
        pending.push(Work {
            catch_handler: None,
            ..marker
        });
        pending.extend(handler.iter().rev().map(Work::leaf));
        pending.push(Work {
            completion: Some(350),
            ..Work::leaf(statement)
        });
        true
    }
    fn error(&mut self, out: &mut Vec<u8>, number: i32, message: &str) {
        self.last_error = number;
        if let Some(error) = crate::json_extract::diagnostic(message)
            && number == error.number
        {
            tds::sql_error(out, &error);
        } else {
            tds::error(out, number, message);
        }
    }
    fn validate_isolation(isolation: u8) -> Result<()> {
        // DuckDB currently supplies snapshot isolation. Other locking modes
        // require further emulation; do not silently accept SERIALIZABLE.
        ensure!(
            matches!(isolation, 0 | 2 | 5),
            "unsupported transaction isolation level {isolation}"
        );
        Ok(())
    }
    pub fn begin_transaction(&mut self, isolation: u8, name: &str) -> Result<Vec<u8>> {
        Self::validate_isolation(isolation)?;
        ensure!(self.transactions < u32::MAX, "transaction nesting overflow");
        let mut out = Vec::new();
        if self.transactions == 0 {
            self.db.execute_batch("BEGIN TRANSACTION")?;
            self.transaction_descriptor = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
            self.transaction_name = name.to_owned();
            tds::transaction_env(&mut out, 8, self.transaction_descriptor);
        }
        self.transactions += 1;
        Ok(out)
    }
    fn rollback_doomed(&mut self, out: &mut Vec<u8>) {
        if self.transaction_doomed {
            match self.rollback_transaction("") {
                Ok(tokens) => out.extend(tokens),
                Err(error) => {
                    emit_error(out, &error);
                }
            }
        }
    }
    fn require_committable(&self) -> Result<()> {
        if self.transaction_doomed {
            return Err(SqlError::new(3930, 1, "The current transaction cannot be committed and cannot support operations that write to the log file. Roll back the transaction.").into());
        }
        Ok(())
    }
    pub fn commit_transaction(&mut self) -> Result<Vec<u8>> {
        self.require_committable()?;
        ensure!(
            self.transactions > 0,
            "COMMIT has no corresponding BEGIN TRANSACTION"
        );
        let mut out = Vec::new();
        if self.transactions == 1 {
            self.db.execute_batch("COMMIT")?;
            tds::transaction_env(&mut out, 9, self.transaction_descriptor);
            self.transaction_descriptor = 0;
            self.transaction_name.clear();
        }
        self.transactions -= 1;
        Ok(out)
    }
    pub fn rollback_transaction(&mut self, name: &str) -> Result<Vec<u8>> {
        ensure!(
            self.transactions > 0,
            "ROLLBACK has no corresponding BEGIN TRANSACTION"
        );
        ensure!(
            name.is_empty() || name == self.transaction_name,
            "Cannot roll back {name}. No transaction or savepoint of that name was found."
        );
        self.db.execute_batch("ROLLBACK")?;
        let mut out = Vec::new();
        tds::transaction_env(&mut out, 10, self.transaction_descriptor);
        self.transactions = 0;
        self.transaction_doomed = false;
        self.transaction_descriptor = 0;
        self.transaction_name.clear();
        Ok(out)
    }
    pub fn transaction_request(&mut self, request: tds::TransactionRequest) -> Result<Vec<u8>> {
        use tds::TransactionRequest;
        // Validate the restart before changing the existing transaction.
        match &request {
            TransactionRequest::Commit {
                restart: Some(begin),
            }
            | TransactionRequest::Rollback {
                restart: Some(begin),
                ..
            } => Self::validate_isolation(begin.isolation)?,
            _ => {}
        }
        let (mut out, restart) = match request {
            TransactionRequest::Begin(begin) => {
                (self.begin_transaction(begin.isolation, &begin.name)?, None)
            }
            TransactionRequest::Commit { restart } => (self.commit_transaction()?, restart),
            TransactionRequest::Rollback { name, restart } => {
                (self.rollback_transaction(&name)?, restart)
            }
            TransactionRequest::Save { .. } => {
                bail!("unsupported savepoint: DuckDB has no native savepoint support")
            }
        };
        if let Some(begin) = restart {
            out.extend(self.begin_transaction(begin.isolation, &begin.name)?);
        }
        tds::done(&mut out, 0xfd, 0, 0, 0);
        Ok(out)
    }
    fn lower_output(
        &self,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<Option<msduck_sql::output::NativePlan>> {
        let plan = msduck_sql::output::lower_native(statement)?;
        if let Some(plan) = &plan {
            let mut bound = Statement::Query(plan.projection.clone());
            crate::aggregate_columns::annotate(&self.db, &mut bound, parameters)
                .map_err(anyhow::Error::msg)?;
            let Statement::Query(query) = bound else {
                unreachable!()
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                bail!("unsupported OUTPUT projection shape");
            };
            plan.bind_projection(statement, &select.projection)?;
            if plan.requires_materialization() || plan.paired {
                // Bind the original RETURNING contract with typed NULLs in
                // place of OUTPUT parameters. This retains native rejection
                // of aggregates/windows without evaluating any user values.
                let mut items = select.projection.clone();
                struct NullParameters<'a>(&'a HashMap<String, Parameter>);
                impl VisitorMut for NullParameters<'_> {
                    type Break = ();
                    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                        if let Expr::Identifier(id) = expr
                            && let Some(parameter) = self.0.get(&id.value.to_lowercase())
                        {
                            *expr = Expr::Cast {
                                kind: CastKind::Cast,
                                expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                                data_type: parameter.ast_type(),
                                format: None,
                            };
                        }
                        ControlFlow::Continue(())
                    }
                }
                let _ = VisitMut::visit(&mut items, &mut NullParameters(parameters));
                let mut candidate = statement.clone();
                plan.bind_projection(&mut candidate, &items)?;
                let declarations = parameters
                    .iter()
                    .map(|(name, parameter)| (name.clone(), parameter.data_type))
                    .collect::<Vec<_>>();
                self.validate_prepared_statements(vec![candidate], &declarations)?;
                plan.bind_projection(statement, &[SelectItem::Wildcard(Default::default())])?;
            }
            if let Some(sink) = &plan.sink {
                let fields = crate::query_catalog::projection_with_parameters(
                    &self.db,
                    &plan.projection,
                    parameters,
                )?
                .ok_or_else(|| anyhow::anyhow!("OUTPUT destination requires a known projection"))?;
                let sink = crate::output_sink::bind(&self.db, sink, &fields)?;
                self.validate_prepared_statements(vec![sink.statement], &sink.declarations)?;
            }
        }
        Ok(plan)
    }

    fn execute(
        &mut self,
        statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Execution> {
        if let Some(request) = msduck_sql::drop_index_syntax::request(&statement) {
            self.require_committable()?;
            return self.execute_drop_index(request);
        }
        if self.transaction_doomed {
            let command = match &statement {
                Statement::Insert(_) => Some(0xc3),
                Statement::Update(_) => Some(0xc5),
                Statement::Delete(_) => Some(0xc4),
                _ => None,
            };
            if let Some(command) = command {
                return Err(crate::query_error::attach_context(
                    self.require_committable().unwrap_err(),
                    vec![],
                    command,
                ));
            }
            if crate::object_catalog::is_ddl(&statement) {
                self.require_committable()?;
            }
        }
        crate::query_catalog::validate_cte_columns(&self.db, &statement)?;
        let catalog_ddl = crate::object_catalog::is_ddl(&statement);
        let own_transaction =
            (catalog_ddl || msduck_sql::output::has(&statement)) && self.transactions == 0;
        if own_transaction {
            self.db.execute_batch("BEGIN TRANSACTION")?;
        }
        let result = (|| {
            let view_columns = crate::query_catalog::view(&self.db, &statement)?;
            let mut result = if let Statement::CreateIndex(index) = &statement {
                let object_id: Option<i32> = self.db.query_row(
                    "SELECT __msduck_object_id(?,'U')",
                    [index.table_name.to_string()],
                    |r| r.get(0),
                )?;
                let object_id = object_id
                    .ok_or_else(|| anyhow::anyhow!("CREATE INDEX target table does not exist"))?;
                crate::index_catalog::create(
                    &self.db,
                    object_id,
                    index,
                    crate::index_catalog::Transaction::CallerOwned,
                )?;
                Execution::statement(vec![], None, 200)
            } else {
                self.execute_inner(
                    statement.clone(),
                    parameters,
                    self.transactions == 0 && !own_transaction,
                )?
            };
            if let Some(command) = ddl_completion_command(&statement) {
                result.count = None;
                result.command = command;
            }
            if catalog_ddl {
                crate::object_catalog::sync(&self.db)?;
                crate::query_catalog::sync(&self.db)?;
                crate::index_catalog::sync(&self.db)?;
                crate::object_catalog::touch(&self.db, &statement)?;
                crate::declared_columns::record(&self.db, &statement)?;
                crate::object_catalog::sync_lob(&self.db)?;
                if let Some((name, fields)) = view_columns {
                    crate::query_catalog::record(&self.db, &name, &fields)?;
                    crate::query_catalog::record_view_definition(&self.db, &statement)?;
                }
            }
            if own_transaction {
                self.db.execute_batch("COMMIT")?;
            }
            Ok(result)
        })();
        if result.is_err() && own_transaction {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        result
    }
    fn execute_drop_index(
        &mut self,
        request: msduck_sql::drop_index::Request,
    ) -> Result<Execution> {
        use msduck_sql::drop_index as bind;
        let managed = crate::index_catalog::acquire_complete(&self.db)?;
        let tables = self.db.prepare("SELECT o.object_id,s.name,o.name FROM sys.objects o JOIN sys.schemas s USING(schema_id) WHERE rtrim(o.type)='U'")?
            .query_map([], |r| Ok(bind::Table { id:r.get::<_,i32>(0)? as u64, schema:r.get(1)?, name:r.get(2)? }))?
            .collect::<duckdb::Result<Vec<_>>>()?;
        let indexes = managed
            .iter()
            .map(|i| bind::Index {
                table_id: i.object_id as u64,
                id: i.index_id as u64,
                name: i.name.clone(),
                clustered: false,
                constraint_backed: false,
                backend_name: ObjectName::from(vec![
                    Ident::with_quote('"', &i.backend_schema),
                    Ident::with_quote('"', &i.backend_name),
                ]),
            })
            .collect::<Vec<_>>();
        let plan = bind::bind(
            &request,
            &tables,
            &indexes,
            &["dbo"],
            str::eq_ignore_ascii_case,
        )
        .map_err(|error| match error {
            bind::Error::Unsupported(message) => anyhow::anyhow!(message),
            bind::Error::Sql(d) => SqlError::from_utf16(
                d.number,
                d.state,
                d.class,
                d.message.encode_utf16().collect(),
            )
            .into(),
        })?;
        for drop in plan.drops {
            let index = managed
                .iter()
                .find(|i| i.object_id as u64 == drop.table_id && i.index_id as u64 == drop.index_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("Bound index identity is absent from its catalog snapshot")
                })?;
            let owner = if self.transactions == 0 {
                crate::index_catalog::Transaction::Owned
            } else {
                crate::index_catalog::Transaction::CallerOwned
            };
            crate::index_catalog::drop_index(&self.db, index, owner)?;
        }
        if let Some(d) = plan.terminal {
            return Err(SqlError::from_utf16(
                d.number,
                d.state,
                d.class,
                d.message.encode_utf16().collect(),
            )
            .into());
        }
        Ok(Execution::statement(vec![], None, 201))
    }

    fn execute_inner(
        &mut self,
        statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
        autocommit: bool,
    ) -> Result<Execution> {
        let diagnostics = self
            .ansi_warnings
            .then(|| self.diagnostic_scope())
            .transpose()?;
        let mut result =
            self.execute_observed(statement, parameters, autocommit, diagnostics.as_ref())?;
        if self.ansi_warnings
            && diagnostics
                .as_ref()
                .is_some_and(|scope| scope.null_eliminated())
        {
            crate::aggregate_diagnostics::append_warning(&mut result.tokens);
        }
        Ok(result)
    }

    fn execute_observed(
        &mut self,
        mut statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
        autocommit: bool,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Execution> {
        validate_transaction_syntax(&statement)?;
        if let Statement::Print(print) = &statement {
            let unicode = print_is_unicode(&print.message, parameters);
            let value = self.evaluate_scalar(
                *print.message.clone(),
                print_target(&print.message, parameters),
                parameters,
            )?;
            let mut tokens = Vec::new();
            let units = match value {
                Value::Null => None,
                Value::Text(message) => Some(message.encode_utf16().take(8000).collect::<Vec<_>>()),
                value @ Value::Struct(_) => Some(crate::unicode_carrier::units(&value)?),
                _ => bail!("unsupported PRINT value representation"),
            };
            tds::diagnostic_utf16(
                &mut tokens,
                tds::DiagnosticKind::Information,
                0,
                1,
                0,
                msduck_core::print::message(units.as_deref(), unicode),
            );
            return Ok(Execution::statement(tokens, None, 247));
        }
        if let Statement::Throw(throw) = &statement {
            let (Some(number), Some(message), Some(state)) =
                (&throw.error_number, &throw.message, &throw.state)
            else {
                if let Some(error) = &self.caught_error {
                    return Err(error.clone().into());
                }
                bail!("unsupported bare THROW outside CATCH");
            };
            let number = self.evaluate_scalar(*number.clone(), DataType::Int(None), parameters)?;
            let message =
                self.evaluate_scalar(*message.clone(), DataType::Varchar(None), parameters)?;
            let state = self.evaluate_scalar(*state.clone(), DataType::Int(None), parameters)?;
            let (Value::Int(number), Value::Text(message), Value::Int(state)) =
                (number, message, state)
            else {
                bail!("THROW arguments cannot be NULL");
            };
            ensure!(number >= 50000, "THROW error number must be at least 50000");
            ensure!(
                (0..=255).contains(&state),
                "THROW state must be between 0 and 255"
            );
            ensure!(
                message.encode_utf16().count() <= 2048,
                "THROW message exceeds 2048 UTF-16 units"
            );
            return Err(SqlError {
                number,
                state: state as u8,
                severity: 16,
                message: message.replace("%%", "%"),
                message_utf16: None,
            }
            .into());
        }
        if let Statement::Declare { stmts } = &statement {
            for declaration in stmts {
                ensure!(
                    declaration.declare_type.is_none(),
                    "unsupported non-scalar declaration"
                );
                let kind = declaration
                    .data_type
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("missing variable type"))?;
                let kind = variable_type(kind)?;
                for name in &declaration.names {
                    let name = name.value.to_lowercase();
                    ensure!(
                        name.starts_with('@') && !name.starts_with("@@"),
                        "invalid local variable name"
                    );
                    let expression = match &declaration.assignment {
                        None => Expr::Value(sqlparser::ast::Value::Null.into()),
                        Some(DeclareAssignment::MsSqlAssignment(value)) => *value.clone(),
                        _ => bail!("unsupported variable initializer"),
                    };
                    let value = self.evaluate_scalar_observed(
                        expression,
                        crate::sql_type::ast(kind),
                        parameters,
                        diagnostics,
                    )?;
                    parameters.insert(
                        name,
                        Parameter {
                            value: crate::backend_value::from_backend(value)?,
                            data_type: kind,
                        },
                    );
                }
            }
            let count = stmts
                .iter()
                .any(|declaration| declaration.assignment.is_some())
                .then_some(1);
            return Ok(Execution::statement(vec![], count, 193));
        }
        if let Statement::Set(Set::SingleAssignment {
            scope,
            hivevar,
            variable,
            values,
        }) = &statement
        {
            let name = variable.to_string().to_lowercase();
            if name.starts_with('@') {
                ensure!(
                    scope.is_none() && !hivevar && values.len() == 1,
                    "unsupported variable assignment"
                );
                let kind = parameters
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
                    .data_type;
                let value = self
                    .evaluate_scalar_observed(
                        values[0].clone(),
                        crate::sql_type::ast(kind),
                        parameters,
                        diagnostics,
                    )
                    .map_err(|error| {
                        if error
                            .downcast_ref::<SqlError>()
                            .is_some_and(|error| matches!(error.number, 8115 | 8134))
                        {
                            self.rowcount = 0;
                            crate::query_error::attach_context(error, vec![], 0xc1)
                        } else {
                            error
                        }
                    })?;
                parameters.insert(
                    name,
                    Parameter {
                        value: crate::backend_value::from_backend(value)?,
                        data_type: kind,
                    },
                );
                return Ok(Execution::statement(vec![], Some(1), 193));
            }
        }
        if let Statement::Set(_) = &statement {
            if let Some(value) = crate::datepart::setting(&statement).map_err(anyhow::Error::msg)? {
                let value = self.evaluate_scalar(value, DataType::Int(None), parameters)?;
                let Value::Int(first) = value else {
                    bail!("DATEFIRST must be between 1 and 7")
                };
                ensure!(
                    (1..=7).contains(&first),
                    "DATEFIRST must be between 1 and 7"
                );
                self.db
                    .execute_batch(&format!("SET VARIABLE __msduck_datefirst = {first}"))?;
                return Ok(Execution::statement(vec![], None, 0));
            }
            if statement
                .to_string()
                .eq_ignore_ascii_case("SET LANGUAGE US_ENGLISH")
            {
                self.db
                    .execute_batch("SET VARIABLE __msduck_datefirst = 7")?;
            }
            let normalized = statement.to_string().to_uppercase().replace(" = ", " ");
            if matches!(
                normalized.as_str(),
                "SET ANSI_WARNINGS ON" | "SET ANSI_WARNINGS OFF"
            ) {
                self.ansi_warnings = normalized.ends_with(" ON");
            }
            if matches!(
                normalized.as_str(),
                "SET XACT_ABORT ON" | "SET XACT_ABORT OFF"
            ) {
                self.xact_abort = normalized.ends_with(" ON");
            }
            if let Some(nocount) = session_setting(&statement)? {
                self.nocount = nocount;
                return Ok(Execution::statement(
                    vec![],
                    None,
                    if nocount { 185 } else { 186 },
                ));
            }
            return Ok(Execution::statement(vec![], None, 0));
        }
        match &statement {
            Statement::StartTransaction { .. } => {
                return Ok(Execution::statement(
                    self.begin_transaction(0, "")?,
                    None,
                    0,
                ));
            }
            Statement::Commit { .. } => {
                return Ok(Execution::statement(self.commit_transaction()?, None, 0));
            }
            Statement::Rollback { savepoint, .. } => {
                let name = savepoint.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                return Ok(Execution::statement(
                    self.rollback_transaction(name)?,
                    None,
                    0,
                ));
            }
            _ => {}
        }
        if let Some((update, with)) = msduck_sql::output::joined_update(&statement) {
            let plan = self
                .plan_joined_execution(update, with.cloned(), parameters)
                .map_err(crate::query_error::compilation)?;
            return self.execute_joined_output(plan);
        }
        let alter_name = if let Some(view) = crate::views::alter_definition(&statement)? {
            let name = view.name.clone();
            statement = Statement::CreateView(view);
            Some(name)
        } else {
            None
        };
        let output = self.lower_output(&mut statement, parameters)?;
        let result_fields = if let Some(output) = &output {
            crate::query_catalog::checked_projection_with_parameters(
                &self.db,
                &output.projection,
                parameters,
            )
            .map_err(crate::query_error::compilation)?
            .unwrap_or_default()
        } else if let Statement::Query(query) = &mut statement {
            crate::query_catalog::bind_query_with_parameters(&self.db, query, parameters)
                .map_err(crate::query_error::compilation)?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        msduck_sql::projection::validate_collation_operations(&result_fields)
            .map_err(anyhow::Error::new)
            .map_err(crate::query_error::compilation)?;
        let output_sink = output
            .as_ref()
            .and_then(|plan| plan.sink.as_ref())
            .map(|sink| crate::output_sink::bind(&self.db, sink, &result_fields))
            .transpose()?;
        let materialized = if let Some(plan) = output
            .as_ref()
            .filter(|plan| plan.requires_materialization() && !plan.paired)
        {
            let name = crate::output_image::name();
            let mut materialized = plan.materialize(&mut statement, name.clone())?;
            let mut bound = Statement::Query(plan.projection.clone());
            crate::aggregate_columns::annotate(&self.db, &mut bound, parameters)
                .map_err(anyhow::Error::msg)?;
            let Statement::Query(bound) = bound else {
                unreachable!()
            };
            let (SetExpr::Select(bound), SetExpr::Select(projection)) =
                (bound.body.as_ref(), materialized.projection.body.as_mut())
            else {
                unreachable!()
            };
            projection.projection = bound.projection.clone();
            let mut translator = Translator {
                parameters,
                values: vec![],
                parameter_slots: HashMap::new(),
                transactions: self.transactions,
                transaction_doomed: self.transaction_doomed,
                original_login: &self.original_login,
                rowcount: self.rowcount,
                last_error: self.last_error,
                caught_error: self.caught_error.as_ref(),
            };
            if let ControlFlow::Break(error) =
                VisitMut::visit(&mut materialized.projection, &mut translator)
            {
                bail!(error);
            }
            Some((materialized, name, translator.values))
        } else {
            None
        };
        self.bind_dml(&mut statement, parameters)?;
        crate::query_catalog::lower_recursion(&self.db, &mut statement)?;
        let json = crate::for_json::Output::take(&self.db, &mut statement)?;
        let assignments = select_assignments(&mut statement, parameters)?;
        let into = crate::select_into::take(&mut statement)?;
        let into_columns = if into.is_some() {
            if let Statement::Query(query) = &statement {
                crate::query_catalog::projection(&self.db, query)?
            } else {
                None
            }
        } else {
            None
        };
        let metadata_statement = output
            .as_ref()
            .map(|output| Statement::Query(output.projection.clone()));
        let result_types = crate::result_types::bound_projection(
            &self.db,
            metadata_statement.as_ref().unwrap_or(&statement),
            parameters,
        )?;
        let is_query = output.is_some() || crate::update::is_query(&statement);
        let write_command = match &statement {
            Statement::Insert(_) => Some(0xc3),
            Statement::Update(_) => Some(0xc5),
            Statement::Delete(_) => Some(0xc4),
            Statement::Query(query) => match query.body.as_ref() {
                SetExpr::Update(_) => Some(0xc5),
                SetExpr::Delete(_) => Some(0xc4),
                _ => None,
            },
            _ => None,
        };
        ensure!(
            matches!(
                statement,
                Statement::Query(_)
                    | Statement::CreateTable(_)
                    | Statement::Truncate(_)
                    | Statement::CreateSchema { .. }
                    | Statement::AlterTable(_)
                    | Statement::CreateView(_)
                    | Statement::Insert(_)
                    | Statement::Update { .. }
                    | Statement::Delete(_)
                    | Statement::Drop { .. }
                    | Statement::CreateIndex(_)
            ),
            "unsupported T-SQL statement"
        );
        let identity_source = match &statement {
            Statement::CreateTable(_) | Statement::AlterTable(_) => Some(statement.clone()),
            _ => None,
        };
        let money_columns = crate::insert::money_columns(&statement, parameters);
        crate::update::expand_compound(&self.db, &mut statement)?;
        let money_assignments = crate::update::money_assignments(&statement, parameters);
        crate::query_catalog::bind_binary_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::statement(&mut statement, parameters).map_err(anyhow::Error::msg)?;
        crate::aggregate_columns::annotate(&self.db, &mut statement, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut statement);
        crate::for_json::lower_nested(&self.db, &mut statement, parameters)?;
        let checked_projection = if output.is_none()
            && json.is_none()
            && assignments.is_empty()
            && into.is_none()
            && let Statement::Query(query) = &statement
            && let Some(plan) = crate::checked_projection::plan(&self.db, query, parameters)?
            && let Some(metadata) = crate::checked_projection::metadata(&plan, &result_fields)?
        {
            for expression in &plan.scalar_checks {
                self.evaluate_expression(expression.clone(), parameters, false)
                    .map_err(|error| {
                        crate::query_error::attach_context(error, metadata.clone(), 0xc1)
                    })?;
            }
            statement = Statement::Query(plan.query);
            Some((plan.width, metadata))
        } else {
            None
        };
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut translator) {
            bail!(error);
        }
        crate::insert::lower(&self.db, &mut statement, &money_columns)?;
        crate::update::lower(&self.db, &mut statement, &money_assignments)?;
        if let Some(diagnostics) = diagnostics {
            let ticket = Expr::Value(
                sqlparser::ast::Value::Placeholder(format!("${}", translator.values.len() + 1))
                    .into(),
            );
            if crate::aggregate_diagnostics::instrument(&mut statement, ticket) > 0 {
                translator
                    .values
                    .push(Value::Blob(diagnostics.ticket().to_vec()));
            }
        }
        // Identity parameters are integer constants, not numeric result expressions.
        match (identity_source.as_ref(), &mut statement) {
            (Some(Statement::CreateTable(original)), Statement::CreateTable(table)) => {
                for (source, target) in original.columns.iter().zip(&mut table.columns) {
                    crate::identity::restore_options(source, target);
                }
            }
            (Some(Statement::AlterTable(original)), Statement::AlterTable(table)) => {
                for (source, target) in original.operations.iter().zip(&mut table.operations) {
                    if let (
                        AlterTableOperation::AddColumn {
                            column_def: source, ..
                        },
                        AlterTableOperation::AddColumn {
                            column_def: target, ..
                        },
                    ) = (source, target)
                    {
                        crate::identity::restore_options(source, target);
                    }
                }
            }
            _ => {}
        }
        if crate::schema_catalog::execute(&self.db, &statement, autocommit)? {
            return Ok(Execution::statement(vec![], None, 0));
        }
        if crate::identity::create(&self.db, &statement, autocommit)? {
            return Ok(Execution::statement(vec![], None, 0));
        }
        if let Statement::Truncate(truncate) = &statement {
            crate::truncate::execute(&self.db, truncate, autocommit)?;
            return Ok(Execution::statement(vec![], None, 0));
        }
        if let Statement::AlterTable(table) = &statement {
            ensure!(
                translator.values.is_empty(),
                "unsupported bound values in ALTER TABLE"
            );
            let Some(Statement::AlterTable(original)) = &identity_source else {
                unreachable!()
            };
            crate::table_alter::execute_declared(&self.db, table, original, autocommit)?;
            return Ok(Execution::statement(vec![], None, 0));
        }
        if crate::identity::drop_table(&self.db, &statement, autocommit)? {
            return Ok(Execution::statement(vec![], None, 0));
        }
        let paired = if let Some(output) = output.as_ref().filter(|output| output.paired) {
            let (update, with) = match &statement {
                Statement::Update(update) => (update, None),
                Statement::Query(query) => {
                    let SetExpr::Update(inner) = query.body.as_ref() else {
                        bail!("expected paired UPDATE")
                    };
                    let Statement::Update(update) = inner else {
                        bail!("expected paired UPDATE")
                    };
                    (update, query.with.clone())
                }
                _ => bail!("expected paired UPDATE"),
            };
            let columns = crate::output_update::columns(&self.db, update)?;
            let name = crate::output_image::name();
            let alias = name.0.last().unwrap().as_ident().unwrap().clone();
            let mut plan = msduck_sql::output_update::plan(update, &columns, name.clone(), alias)?;
            plan.capture.with = with;
            let mut bound = Statement::Query(output.projection.clone());
            crate::aggregate_columns::annotate(&self.db, &mut bound, parameters)
                .map_err(anyhow::Error::msg)?;
            let Statement::Query(bound) = bound else {
                unreachable!()
            };
            let SetExpr::Select(bound) = bound.body.as_ref() else {
                unreachable!()
            };
            let mut projection = plan.projection(&bound.projection)?;
            let mut projection_translator = Translator {
                parameters,
                values: vec![],
                parameter_slots: HashMap::new(),
                transactions: self.transactions,
                transaction_doomed: self.transaction_doomed,
                original_login: &self.original_login,
                rowcount: self.rowcount,
                last_error: self.last_error,
                caught_error: self.caught_error.as_ref(),
            };
            if let ControlFlow::Break(error) =
                VisitMut::visit(&mut projection, &mut projection_translator)
            {
                bail!(error);
            }
            Some((plan, name, projection, projection_translator.values))
        } else {
            None
        };
        let rendered = paired.as_ref().map_or_else(
            || statement.to_string(),
            |(plan, _, _, _)| plan.write.to_string(),
        );
        if let Some(target) = into {
            let count = crate::select_into::execute(
                &self.db,
                &target,
                &rendered,
                &translator.values,
                autocommit,
                into_columns.as_deref(),
            )?;
            return Ok(Execution::statement(vec![], Some(count), 0xc1));
        }
        if let Some(name) = alter_name {
            crate::views::alter_existing(&self.db, &name, &rendered, autocommit)?;
            return Ok(Execution::statement(vec![], None, 0));
        }
        let image = if let Some((plan, name, _, _)) = &paired {
            Some(crate::output_image::Image::create_bound(
                &self.db,
                name.clone(),
                &plan.capture,
                &translator.values,
            )?)
        } else {
            materialized
                .as_ref()
                .map(|(plan, name, _)| {
                    crate::output_image::Image::create(&self.db, name.clone(), &plan.image)
                })
                .transpose()?
        };
        let mut projected = if let Some((_, _, projection, _)) = &paired {
            Some(self.db.prepare(&projection.to_string())?)
        } else {
            materialized
                .as_ref()
                .map(|(plan, _, _)| self.db.prepare(&plan.projection.to_string()))
                .transpose()?
        };
        if self.transactions > 0 && !is_query && output.is_none() {
            let checked = match write_command {
                Some(0xc3) => crate::storage_diagnostic::checked_insert(
                    &self.db,
                    &statement,
                    &translator.values,
                ),
                Some(0xc5) => crate::storage_diagnostic::checked_update(
                    &self.db,
                    &statement,
                    &translator.values,
                ),
                _ => Ok(None),
            };
            let count = checked.map_err(|error| {
                if error
                    .downcast_ref::<SqlError>()
                    .is_some_and(|e| e.number == 2628)
                {
                    crate::query_error::attach_context(
                        error,
                        vec![],
                        write_command.expect("checked DML"),
                    )
                } else {
                    error
                }
            })?;
            if let Some(count) = count {
                return Ok(Execution::statement(
                    vec![],
                    Some(count),
                    write_command.expect("checked DML"),
                ));
            }
        }
        let mut prepared = self.db.prepare(&rendered).map_err(|error| {
            if let Some(diagnostic) = crate::query_error::integer_overflow(&error.to_string()) {
                if is_query
                    && output.is_none()
                    && json.is_none()
                    && assignments.is_empty()
                    && let Some(metadata) =
                        crate::query_error::describe_fields(&result_fields, &result_types)
                {
                    return crate::query_error::attach_context(diagnostic.into(), metadata, 0xc1);
                }
                return anyhow::Error::new(diagnostic);
            }
            anyhow::Error::new(error)
        })?;
        if !is_query {
            let count = prepared
                .execute(duckdb::params_from_iter(translator.values.iter()))
                .map_err(|error| {
                    // A rejected character write terminates this statement. Keep
                    // its operation identity for 3621 and subsequent batch work;
                    // preparation/binding errors never enter this execution path.
                    if let Some(command) = write_command
                        && matches!(error_number(&error.to_string()), 8152 | 2628)
                    {
                        crate::query_error::attach(error, vec![], command)
                    } else {
                        anyhow::Error::new(error)
                    }
                })? as u64;
            return Ok(Execution::statement(
                vec![],
                Some(count),
                write_command.unwrap_or(0xc3),
            ));
        }
        let names = if json.is_some() {
            crate::for_json::Output::names(&self.db, &rendered, &translator.values)?
        } else {
            Vec::new()
        };
        let json_plan = json.as_ref().map(|json| json.plan(&names)).transpose()?;
        let error_metadata = if let Some((_, metadata)) = &checked_projection {
            Some(metadata.clone())
        } else if json.is_none() && assignments.is_empty() && output_sink.is_none() {
            crate::query_error::describe(
                projected.as_ref().unwrap_or(&prepared),
                &result_fields,
                &result_types,
            )
        } else {
            None
        };
        let output_command = output.as_ref().map_or(0xc1, |plan| match plan.operation {
            msduck_sql::output::Operation::Insert => 0xc3,
            msduck_sql::output::Operation::Update => 0xc5,
            msduck_sql::output::Operation::Delete => 0xc4,
        });
        let batches = if let Some((plan, _, _, values)) = &paired {
            let describe = |error| match &error_metadata {
                Some(metadata) => {
                    crate::query_error::attach(error, metadata.clone(), output_command)
                }
                None if output_sink.is_some() => crate::output_sink::failed(
                    anyhow::Error::new(error),
                    msduck_sql::output::Operation::Update,
                ),
                None => anyhow::Error::new(error),
            };
            let mut capture = self.db.prepare(&plan.capture.to_string())?;
            image.as_ref().unwrap().append(
                capture
                    .query_arrow(duckdb::params_from_iter(translator.values.iter()))
                    .map_err(describe)?,
            )?;
            prepared.execute([]).map_err(describe)?;
            projected
                .as_mut()
                .unwrap()
                .query_arrow(duckdb::params_from_iter(values.iter()))
                .map_err(describe)?
        } else {
            let batches = prepared
                .query_arrow(duckdb::params_from_iter(translator.values.iter()))
                .map_err(|error| match &error_metadata {
                    Some(metadata) => {
                        crate::query_error::attach(error, metadata.clone(), output_command)
                    }
                    None if output_sink.is_some() => crate::output_sink::failed(
                        anyhow::Error::new(error),
                        output.as_ref().unwrap().operation,
                    ),
                    None => anyhow::Error::new(error),
                })?;
            if let Some((_, _, values)) = &materialized {
                image
                    .as_ref()
                    .expect("materialized image")
                    .append(batches)?;
                projected
                    .as_mut()
                    .expect("materialized projection")
                    .query_arrow(duckdb::params_from_iter(values.iter()))
                    .map_err(|error| match &error_metadata {
                        Some(metadata) => {
                            crate::query_error::attach(error, metadata.clone(), output_command)
                        }
                        None if output_sink.is_some() => crate::output_sink::failed(
                            anyhow::Error::new(error),
                            output.as_ref().unwrap().operation,
                        ),
                        None => anyhow::Error::new(error),
                    })?
            } else {
                batches
            }
        };
        if let Some(sink) = output_sink {
            let rows = Self::collect_output_rows(batches, sink.declarations.len())?;
            drop(prepared);
            drop(projected);
            crate::output_image::close(image)?;
            return self.store_output_rows(sink, rows, output.as_ref().unwrap().operation);
        }

        if let (Some(json), Some(plan)) = (json, json_plan) {
            let mut writer = json.writer(&plan)?;
            let json_types = batches
                .get_schema()
                .fields()
                .iter()
                .enumerate()
                .map(|(i, field)| {
                    result_types
                        .get(i)
                        .cloned()
                        .flatten()
                        .or_else(|| wire_type(field.data_type()).ok())
                })
                .collect::<Vec<_>>();
            for batch in batches {
                for row in 0..batch.num_rows() {
                    let values = (0..batch.num_columns())
                        .map(|i| {
                            arrow_value(batch.column(i).as_ref(), row, batch.schema().field(i))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    json.row(&mut writer, &names, &values, &json_types)?;
                }
            }
            return Ok(Execution::result_set(
                crate::for_json::Output::encode(writer.finish())?,
                Some(1),
                0xc1,
            ));
        }
        if !assignments.is_empty() {
            let mut count = 0u64;
            let mut last = None;
            for batch in batches {
                count += batch.num_rows() as u64;
                if batch.num_rows() > 0 {
                    let row = batch.num_rows() - 1;
                    last = Some(
                        (0..assignments.len())
                            .map(|i| {
                                binding_value(arrow_value(
                                    batch.column(i).as_ref(),
                                    row,
                                    batch.schema().field(i),
                                )?)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
            }
            // No rows leave existing bindings untouched. A scalar subquery
            // returning no rows instead produces one row containing NULL.
            if let Some(values) = last {
                for ((name, data_type), value) in assignments.into_iter().zip(values) {
                    parameters.insert(
                        name,
                        Parameter {
                            value: crate::backend_value::from_backend(value)?,
                            data_type,
                        },
                    );
                }
            }
            return Ok(Execution::statement(vec![], Some(count), 0xc1));
        }
        let schema = batches.get_schema();
        let (out, count) = Self::encode_batches_inner(
            batches,
            &schema,
            &result_fields,
            &result_types,
            checked_projection.map(|(width, _)| width),
        )?;
        crate::output_image::close(image)?;
        Ok(Execution::result_set(
            out,
            Some(count),
            output
                .as_ref()
                .map_or(0xc1, |output| match output.operation {
                    msduck_sql::output::Operation::Insert => 0xc3,
                    msduck_sql::output::Operation::Update => 0xc5,
                    msduck_sql::output::Operation::Delete => 0xc4,
                }),
        ))
    }

    fn collect_output_rows(
        batches: impl Iterator<Item = duckdb::arrow::record_batch::RecordBatch>,
        width: usize,
    ) -> Result<Vec<Vec<ParameterValue>>> {
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        for batch in batches {
            ensure!(
                batch.num_columns() == width,
                "OUTPUT projection width changed during execution"
            );
            for row in 0..batch.num_rows() {
                let values = (0..batch.num_columns())
                    .map(|i| {
                        crate::backend_value::from_backend(binding_value(arrow_value(
                            batch.column(i).as_ref(),
                            row,
                            batch.schema().field(i),
                        )?)?)
                    })
                    .collect::<Result<Vec<_>>>()?;
                for value in &values {
                    bytes = bytes
                        .saturating_add(std::mem::size_of::<ParameterValue>())
                        .saturating_add(match value {
                            ParameterValue::Text(value) => value.len(),
                            ParameterValue::Unicode(value) => value.len().saturating_mul(2),
                            ParameterValue::Blob(value) => value.len(),
                            _ => 0,
                        });
                }
                ensure!(
                    bytes <= 64 * 1024 * 1024,
                    "OUTPUT materialization exceeds the configured 64 MiB limit"
                );
                rows.push(values);
            }
        }
        Ok(rows)
    }

    fn store_output_rows(
        &mut self,
        sink: crate::output_sink::Bound,
        rows: Vec<Vec<ParameterValue>>,
        operation: msduck_sql::output::Operation,
    ) -> Result<Execution> {
        let count = rows.len() as u64;
        for row in rows {
            let mut bindings = sink
                .declarations
                .iter()
                .zip(row)
                .map(|((name, kind), value)| {
                    (
                        name.clone(),
                        Parameter {
                            data_type: *kind,
                            value,
                        },
                    )
                })
                .collect();
            self.execute_inner(sink.statement.clone(), &mut bindings, false)
                .map_err(|error| crate::output_sink::failed(error, operation))?;
        }
        Ok(Execution::statement(
            vec![],
            Some(count),
            match operation {
                msduck_sql::output::Operation::Insert => 0xc3,
                msduck_sql::output::Operation::Update => 0xc5,
                msduck_sql::output::Operation::Delete => 0xc4,
            },
        ))
    }

    fn encode_batches(
        batches: impl Iterator<Item = duckdb::arrow::record_batch::RecordBatch>,
        schema: &duckdb::arrow::datatypes::Schema,
        result_fields: &[crate::query_catalog::Field],
        result_types: &[Option<Type>],
    ) -> Result<(Vec<u8>, u64)> {
        Self::encode_batches_inner(batches, schema, result_fields, result_types, None)
    }

    fn encode_batches_inner(
        batches: impl Iterator<Item = duckdb::arrow::record_batch::RecordBatch>,
        schema: &duckdb::arrow::datatypes::Schema,
        result_fields: &[crate::query_catalog::Field],
        result_types: &[Option<Type>],
        checked_width: Option<usize>,
    ) -> Result<(Vec<u8>, u64)> {
        let public_schema = checked_width
            .map(|width| schema.project(&(0..width).collect::<Vec<_>>()))
            .transpose()?;
        let schema = public_schema.as_ref().unwrap_or(schema);
        let metadata = crate::result_metadata::Aligned::new(
            schema.fields().len(),
            result_fields,
            result_types,
        );
        let columns = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let kind = if result_fields.len() == schema.fields().len()
                    && matches!(
                        result_fields[index]
                            .info
                            .as_ref()
                            .and_then(|info| info.system_type_id),
                        Some(58 | 61)
                    )
                    && matches!(field.data_type(), ArrowType::Timestamp(_, None))
                {
                    Type::LegacyDateTime(
                        if result_fields[index]
                            .info
                            .as_ref()
                            .and_then(|info| info.system_type_id)
                            == Some(58)
                        {
                            4
                        } else {
                            8
                        },
                    )
                } else if let Some(kind) = metadata.declared(index)
                    && (matches!(field.data_type(), ArrowType::Utf8 | ArrowType::LargeUtf8)
                        || crate::unicode_carrier::is_arrow(field.data_type())
                            && matches!(kind, Type::Text | Type::Nvarchar(_) | Type::Nchar(_))
                        || matches!(
                            (kind, field.data_type()),
                            (Type::Time(_), ArrowType::Time64(_))
                                | (Type::Money(_), ArrowType::Decimal128(_, 4))
                                | (
                                    Type::Varbinary(_) | Type::FixedBinary(_),
                                    ArrowType::Binary | ArrowType::LargeBinary
                                )
                        ))
                {
                    kind.clone()
                } else if is_bool_field(field) {
                    Type::Bit
                } else if is_uuid_field(field) {
                    Type::Guid
                } else {
                    wire_type(field.data_type())?
                };
                Ok(metadata.column(index, field.name().clone(), kind))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut out = vec![];
        tds::metadata(&mut out, &columns)?;
        let mut count = 0;
        for batch in batches {
            for row in 0..batch.num_rows() {
                if let Some(width) = checked_width
                    && let Some(error) = crate::checked_projection::diagnostic(&batch, row, width)?
                {
                    return Err(crate::query_error::attach_context(error.into(), out, 0xc1));
                }
                out.push(0xd1);
                for (i, column) in columns.iter().enumerate() {
                    encode_column(
                        &mut out,
                        column,
                        &arrow_value(batch.column(i).as_ref(), row, batch.schema().field(i))?,
                    )?;
                }
                ensure!(
                    out.len() <= tds::MAX_MESSAGE,
                    "result exceeds current 16 MiB response limit"
                );
                count += 1;
            }
        }
        Ok((out, count))
    }

    fn evaluate_scalar(
        &self,
        expression: Expr,
        data_type: DataType,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<Value> {
        self.evaluate_scalar_observed(expression, data_type, parameters, None)
    }

    fn evaluate_scalar_observed(
        &self,
        expression: Expr,
        data_type: DataType,
        parameters: &HashMap<String, Parameter>,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Value> {
        let expression = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expression),
            data_type,
            format: None,
        };
        self.evaluate_expression_observed(expression, parameters, false, diagnostics)
    }

    fn evaluate_expression(
        &self,
        expression: Expr,
        parameters: &HashMap<String, Parameter>,
        predicate: bool,
    ) -> Result<Value> {
        self.evaluate_expression_observed(expression, parameters, predicate, None)
    }

    fn evaluate_expression_observed(
        &self,
        mut expression: Expr,
        parameters: &HashMap<String, Parameter>,
        predicate: bool,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Value> {
        crate::query_catalog::lower_recursion(&self.db, &mut expression)?;
        crate::aggregate_columns::annotate(&self.db, &mut expression, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_expression(&self.db, &mut expression, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut expression);
        crate::for_json::lower_nested(&self.db, &mut expression, parameters)?;
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
        };
        use msduck_sql::checked_expression::{Kind, plan};
        let mut declarations = parameters
            .iter()
            .filter_map(|(name, parameter)| {
                let kind = match parameter.ast_type() {
                    DataType::Int(_) | DataType::Integer(_) => Kind::Int,
                    DataType::BigInt(_) => Kind::BigInt,
                    _ => return None,
                };
                Some((name.clone(), kind))
            })
            .collect::<HashMap<_, _>>();
        for name in ["@@trancount", "@@rowcount", "@@error"] {
            declarations.insert(name.into(), Kind::Int);
        }
        let (sql, checked) = if let Some(mut checked) = plan(&expression, &declarations) {
            if let ControlFlow::Break(error) = VisitMut::visit(&mut checked.query, &mut translator)
            {
                bail!(error);
            }
            if let Some(diagnostics) = diagnostics {
                crate::aggregate_diagnostics::bind_expressions(
                    &mut checked.query,
                    diagnostics,
                    &mut translator.values,
                );
            }
            (checked.query.to_string(), true)
        } else {
            if let ControlFlow::Break(error) = VisitMut::visit(&mut expression, &mut translator) {
                bail!(error);
            }
            if let Some(diagnostics) = diagnostics {
                crate::aggregate_diagnostics::bind_expressions(
                    &mut expression,
                    diagnostics,
                    &mut translator.values,
                );
            }
            (format!("SELECT {expression}"), false)
        };
        let mut statement = self.db.prepare(&sql)?;
        let mut batches =
            statement.query_arrow(duckdb::params_from_iter(translator.values.iter()))?;
        if predicate {
            ensure!(
                is_bool_field(batches.get_schema().field(0)),
                "An expression of non-boolean type specified in a context where a condition is expected"
            );
        }
        let batch = batches
            .next()
            .ok_or_else(|| anyhow::anyhow!("scalar assignment returned no value"))?;
        ensure!(
            batch.num_rows() == 1 && batch.num_columns() == if checked { 5 } else { 1 },
            "scalar assignment must return one value"
        );
        if checked {
            let field =
                |index| arrow_value(batch.column(index).as_ref(), 0, batch.schema().field(index));
            match field(1)? {
                Value::Null => {}
                Value::Int(number) => {
                    let (Value::UTinyInt(state), Value::UTinyInt(severity), Value::Text(message)) =
                        (field(2)?, field(3)?, field(4)?)
                    else {
                        bail!("invalid checked scalar diagnostic");
                    };
                    let mut error = SqlError::new(number, state, message);
                    error.severity = severity;
                    return Err(error.into());
                }
                _ => bail!("invalid checked scalar error number"),
            }
        }
        let value = arrow_value(batch.column(0).as_ref(), 0, batch.schema().field(0))?;
        binding_value(value)
    }
}
// Shared validation for NOCOUNT and fixed settings. DATEFIRST has a separate
// typed runtime path so preparation cannot mutate the connection.
fn session_setting(statement: &Statement) -> Result<Option<bool>> {
    let normalized = statement.to_string().to_uppercase().replace(" = ", " ");
    match normalized.as_str() {
        "SET NOCOUNT ON" => return Ok(Some(true)),
        "SET NOCOUNT OFF" => return Ok(Some(false)),
        _ => {}
    }
    ensure!(
        [
            "SET ANSI_NULLS ON",
            "SET ANSI_NULL_DFLT_ON ON",
            "SET ANSI_PADDING ON",
            "SET ANSI_WARNINGS ON",
            "SET ANSI_WARNINGS OFF",
            "SET ARITHABORT ON",
            "SET QUOTED_IDENTIFIER ON",
            "SET CONCAT_NULL_YIELDS_NULL ON",
            "SET NUMERIC_ROUNDABORT OFF",
            "SET IMPLICIT_TRANSACTIONS OFF",
            "SET CURSOR_CLOSE_ON_COMMIT OFF",
            "SET XACT_ABORT OFF",
            "SET XACT_ABORT ON",
            "SET TEXTSIZE 2147483647",
            "SET DATEFORMAT MDY",
            "SET LANGUAGE US_ENGLISH",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED"
        ]
        .contains(&normalized.as_str()),
        "unsupported session setting: {normalized}"
    );
    Ok(None)
}

// Preserve temporal precision when an evaluated value is rebound later.
fn binding_value(value: Value) -> Result<Value> {
    if let Value::Time64(unit, value) = value {
        let value = ticks(unit, value)?;
        let seconds = value / 10_000_000;
        return Ok(Value::Text(format!(
            "{:02}:{:02}:{:02}.{:07}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            value % 10_000_000
        )));
    }
    Ok(value)
}

fn select_assignments(
    statement: &mut Statement,
    parameters: &HashMap<String, Parameter>,
) -> Result<Vec<(String, SqlType)>> {
    let Statement::Query(query) = statement else {
        return Ok(vec![]);
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        return Ok(vec![]);
    };
    let is_assignment = |item: &SelectItem| {
        matches!(item,
        SelectItem::ExprWithAlias { alias, .. } if alias.quote_style.is_none() && alias.value.starts_with('@'))
    };
    if !select.projection.iter().any(is_assignment) {
        return Ok(vec![]);
    }
    ensure!(
        select.projection.iter().all(is_assignment),
        "A SELECT statement that assigns a value to a variable must not be combined with data-retrieval operations"
    );
    ensure!(
        select.into.is_none(),
        "unsupported SELECT assignment with INTO"
    );
    let mut assignments = Vec::new();
    for item in &mut select.projection {
        let SelectItem::ExprWithAlias { expr, alias } = item else {
            unreachable!()
        };
        let name = alias.value.to_lowercase();
        let data_type = parameters
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
            .data_type;
        assignments.push((name, data_type));
        *item = SelectItem::UnnamedExpr(Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expr.clone()),
            data_type: crate::sql_type::ast(data_type),
            format: None,
        });
    }
    Ok(assignments)
}

// Preserve the known character family before expression translation erases it.
// Unknown function/column result types conservatively use the Unicode limit.
fn print_is_unicode(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    if let Some(kind) = msduck_sql::expression_metadata::storage::kind(expr, parameters, &|_| None)
        && let Ok(SqlType::Character(kind)) = crate::sql_type::declaration(&kind)
    {
        return matches!(
            kind.family(),
            msduck_core::character::Family::Nchar | msduck_core::character::Family::Nvarchar
        );
    }
    match expr {
        Expr::Value(value) => {
            matches!(value.value, sqlparser::ast::Value::NationalStringLiteral(_))
        }
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_none_or(|p| matches!(p.ast_type(), DataType::Nvarchar(_))),
        Expr::Nested(inner) => print_is_unicode(inner, parameters),
        Expr::Cast { data_type, .. } => matches!(data_type, DataType::Nvarchar(_)),
        Expr::BinaryOp { left, right, .. } => {
            print_is_unicode(left, parameters) || print_is_unicode(right, parameters)
        }
        _ => true,
    }
}
fn print_target(expr: &Expr, parameters: &HashMap<String, Parameter>) -> DataType {
    if print_is_unicode(expr, parameters) {
        DataType::Nvarchar(Some(CharacterLength::Max))
    } else {
        DataType::Varchar(None)
    }
}
pub fn error_number(message: &str) -> i32 {
    if message == msduck_sql::unary_operator::BIT_MINUS {
        return 8117;
    }
    if let Some(error) = runtime_diagnostic(message) {
        return error.number;
    }
    if message.starts_with("Types don't match between the anchor and the recursive part in column ")
        && message.ends_with("\".")
    {
        return 240;
    }
    if let Some(number) = msduck_sql::query_options::error_number(message) {
        return number;
    }
    if let Some(number) = msduck_sql::cte_columns::error_number(message) {
        return number;
    }
    if let Some(number) = msduck_sql::for_json::error_number(message) {
        return number;
    }
    if message == crate::string_escape::ARITY {
        return 174;
    }
    if message == crate::string_escape::NULL_FORMAT {
        return 8116;
    }
    if message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message)
        == crate::character_storage::TRUNCATED
    {
        return 8152;
    }
    if message.starts_with("Sequence Error: nextval: reached ")
        && message.contains("__msduck_identity_")
    {
        return 8115;
    }

    if message == crate::identity::EXPLICIT {
        return 544;
    }
    if message == crate::identity::UPDATE {
        return 8102;
    }
    if message == "Arithmetic overflow error converting IDENTITY seed." {
        return 8115;
    }

    if message.contains("is not supported by date function dateadd for data type")
        || message.contains("is not supported by date function datepart for data type")
        || message.contains("is not supported by date function datename for data type")
    {
        return 9810;
    }
    if message.contains(crate::datetime2_cast::CONVERSION) {
        return 241;
    }
    if message.starts_with("The reference to column ")
        && message.contains("argument to the NTILE function")
    {
        return 4195;
    }
    if message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message)
        == msduck_sql::generate_series::ZERO_STEP
    {
        return 4199;
    }
    if message.contains(crate::ntile::POSITIVE) {
        return 4116;
    }
    if message.contains(crate::ntile::TYPE) {
        return 4110;
    }
    if message.contains(crate::value_window::NEGATIVE_OFFSET) {
        return 8730;
    }
    if message == crate::window_placement::ERROR {
        return 4108;
    }
    if message == "Window element in OVER clause can not also be specified in WINDOW clause." {
        return 4123;
    }
    if let Some(number) = crate::window_frame::error_number(message) {
        return number;
    }
    if let Some(number) = crate::ranking::error_number(message) {
        return number;
    }
    if let Some(number) = crate::grouping::error_number(message) {
        return number;
    }
    if let Some(number) = crate::aggregate::error_number(message) {
        return number;
    }
    if message.starts_with("Operand data type bit is invalid for ")
        || message.starts_with("Operand data type sql_variant is invalid for ")
    {
        return 8117;
    }
    if let Some(number) = crate::merge::error_number(message) {
        return number;
    }
    if message
        == "Invalid Input Error: The offset specified in a OFFSET clause may not be negative."
    {
        return 10742;
    }
    if message
        == "Invalid Input Error: The number of rows provided for a FETCH clause must be greater then zero."
    {
        return 10744;
    }
    if message == "TOP cannot be combined with OFFSET/FETCH in the same query" {
        return 10741;
    }
    if message == "Invalid Input Error: A TOP or FETCH clause contains an invalid value." {
        return 1014;
    }
    if message == "SELECT INTO expressions require column names" {
        return 1038;
    }
    if message == "SELECT INTO column names must be unique" {
        return 2705;
    }
    if message.starts_with("The type of the first argument to NULLIF") {
        return 4151;
    }
    if message.starts_with("At least one of the arguments to COALESCE") {
        return 4127;
    }
    if message.starts_with("At least one of the result expressions in a CASE specification") {
        return 8133;
    }
    if message == "Case expressions may only be nested to level 10." {
        return 125;
    }
    if message.starts_with("Out of Range Error: Overflow in ")
        || message.starts_with("Out of Range Error: Overflow on abs(")
    {
        return 8115;
    }
    if message.starts_with("Constraint Error: Violates foreign key constraint")
        || message.starts_with("Constraint Error: CHECK constraint failed on table ")
    {
        return 547;
    }
    if message.starts_with(
        "Invalid Input Error: Arithmetic overflow error converting expression to data type ",
    ) {
        return 8115;
    }
    if message.contains(
        "Cannot construct data type date, some of the arguments have values which are not valid.",
    ) || message.contains(crate::timefromparts::INVALID)
        || message.contains(crate::datetime2fromparts::INVALID)
        || message.contains(crate::datetimeoffsetfromparts::INVALID)
    {
        return 289;
    }
    if message.contains(crate::datetime2fromparts::INVALID_SCALE)
        || message.contains(crate::timefromparts::INVALID_SCALE)
        || message.contains(crate::datetimeoffsetfromparts::INVALID_SCALE)
    {
        return 10760;
    }
    if message.contains(crate::datediff::OVERFLOW) {
        return 535;
    }
    if message.contains(crate::switchoffset::INVALID)
        || message.contains(crate::switchoffset::ATTACH_INVALID)
    {
        return 9812;
    }
    if message.contains(crate::switchoffset::OVERFLOW)
        || message.contains(crate::switchoffset::ATTACH_OVERFLOW)
    {
        return 9813;
    }
    if message.contains(crate::eomonth::OVERFLOW)
        || message.contains(crate::datetime2_add::OVERFLOW)
        || message.contains(crate::datetime2_add::OFFSET_OVERFLOW)
    {
        return 517;
    }
    if message.starts_with("Cannot truncate table") && message.contains("FOREIGN KEY") {
        return 4712;
    }

    if message.contains("Divide by zero error encountered") {
        return 8134;
    }
    if message.contains("non-boolean type") {
        return 4145;
    }
    if message.contains("must not be combined with data-retrieval") {
        return 141;
    }
    if message == crate::grouping_syntax::CONSTANT_ERROR {
        return 164;
    }
    if message == crate::grouping_syntax::ERROR {
        return 144;
    }
    if message.starts_with("The multi-part identifier ")
        && message.ends_with(" could not be bound.")
    {
        return 4104;
    }
    if message.starts_with("Invalid column name ") {
        return 207;
    }
    if message.contains("Must declare the scalar variable") {
        return 137;
    }
    if message.contains("has already been declared") {
        return 134;
    }
    if message.contains("More than one row returned by a subquery") {
        return 512;
    }
    if message.contains("Could not find prepared statement") {
        return 8179;
    }
    if message.contains("COMMIT has no corresponding") {
        return 3902;
    }
    if message.contains("ROLLBACK has no corresponding") {
        return 3903;
    }
    if message.contains("No transaction or savepoint") {
        return 6401;
    }
    if message.contains("does not exist") {
        208
    } else if message.contains("Duplicate key") || message.contains("PRIMARY KEY or UNIQUE") {
        2627
    } else if message.contains("NOT NULL constraint") {
        515
    } else if message.contains("Conversion Error") {
        245
    } else if message.contains("unsupported") {
        40515
    } else {
        50000
    }
}
struct Translator<'a> {
    parameters: &'a HashMap<String, Parameter>,
    values: Vec<Value>,
    parameter_slots: HashMap<String, usize>,
    transactions: u32,
    transaction_doomed: bool,
    original_login: &'a str,
    rowcount: u64,
    last_error: i32,
    caught_error: Option<&'a SqlError>,
}
fn max_length_type(kind: &DataType) -> bool {
    matches!(
        kind,
        DataType::Varchar(Some(CharacterLength::Max))
            | DataType::Nvarchar(Some(CharacterLength::Max))
            | DataType::Varbinary(Some(BinaryLength::Max))
    )
}
fn max_length_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| max_length_type(&p.ast_type())),
        Expr::Cast { data_type, .. } => max_length_type(data_type),
        Expr::Nested(expr) => max_length_expr(expr, parameters),
        Expr::BinaryOp { left, right, .. } => {
            max_length_expr(left, parameters) || max_length_expr(right, parameters)
        }
        Expr::Value(value) => match &value.value {
            sqlparser::ast::Value::NationalStringLiteral(s) => s.encode_utf16().count() > 4000,
            sqlparser::ast::Value::SingleQuotedString(s) => s.len() > 8000,
            _ => false,
        },
        _ => false,
    }
}
pub(crate) fn money_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    msduck_sql::expression_metadata::currency::kind(expr, parameters, &|_| None).is_some()
}
pub(crate) fn integral_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| integral_type(&p.ast_type())),
        Expr::Value(value) => {
            matches!(&value.value, sqlparser::ast::Value::Number(n, _) if n.parse::<i32>().is_ok())
        }
        Expr::Nested(expr) | Expr::UnaryOp { expr, .. } => integral_expr(expr, parameters),
        Expr::Cast { data_type, .. } => integral_type(data_type),
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo,
            right,
        } => integral_expr(left, parameters) && integral_expr(right, parameters),
        _ => crate::case_types::is_integral(expr, parameters),
    }
}
fn string_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Cast { data_type, .. } => matches!(
            data_type,
            DataType::Varchar(_) | DataType::Nvarchar(_) | DataType::Char(_) | DataType::Text
        ),
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| matches!(p.ast_type(), DataType::Varchar(_) | DataType::Nvarchar(_))),
        Expr::Value(value) => matches!(
            value.value,
            sqlparser::ast::Value::SingleQuotedString(_)
                | sqlparser::ast::Value::NationalStringLiteral(_)
        ),
        Expr::Nested(expr) => string_expr(expr, parameters),
        _ => crate::case_types::is_character(expr, parameters),
    }
}
pub(crate) fn integer_input(argument: Expr, target: &DataType, is_try: bool) -> Expr {
    // A proven integer source cannot be a Unicode carrier or SQL_VARIANT.
    // Dispatching it through a CASE macro repeats nested conditional ASTs.
    // Keep the existing numeric range check, with one source occurrence.
    let integer = crate::case_types::integer_rank(&argument, &Default::default());
    let mut result = if let Some(rank) = integer {
        let kind = match rank {
            0 => "UTINYINT",
            1 => "SMALLINT",
            2 => "INTEGER",
            _ => "BIGINT",
        };
        binary_function(
            "__msduck_integer_text",
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(argument),
                data_type: DataType::Varchar(None),
                format: None,
            },
            Expr::Value(sqlparser::ast::Value::SingleQuotedString(kind.into()).into()),
        )
    } else {
        unary_function("__msduck_integer_input", argument)
    };
    if let Expr::Function(f) = &mut result
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args.extend([
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::Value::SingleQuotedString(target.to_string()).into(),
            ))),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::Value::Boolean(is_try).into(),
            ))),
        ]);
    }
    result
}
fn translate_type(kind: &mut DataType) -> ControlFlow<String> {
    if crate::variant_pack::is_variant(kind) {
        *kind = crate::variant_pack::storage_type();
        return ControlFlow::Continue(());
    }
    match crate::datetimeoffset_cast::scale(kind) {
        Ok(Some(scale)) => {
            *kind = crate::datetimeoffset_cast::storage_type(scale);
            return ControlFlow::Continue(());
        }
        Err(error) => return ControlFlow::Break(error),
        Ok(None) => {}
    }
    match crate::datetime2_cast::scale(kind) {
        Ok(Some(scale)) => {
            *kind = crate::datetime2_cast::storage_type(scale);
            return ControlFlow::Continue(());
        }
        Err(error) => return ControlFlow::Break(error),
        Ok(None) => {}
    }
    match kind {
        DataType::Float(info) => {
            *kind = match info {
                ExactNumberInfo::None | ExactNumberInfo::Precision(25..=53) => {
                    DataType::Double(ExactNumberInfo::None)
                }
                ExactNumberInfo::Precision(1..=24) => DataType::Real,
                _ => return ControlFlow::Break("FLOAT precision must be between 1 and 53".into()),
            };
        }
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("money") => {
            *kind = DataType::Decimal(ExactNumberInfo::PrecisionAndScale(19, 4));
        }
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("smallmoney") => {
            *kind = DataType::Decimal(ExactNumberInfo::PrecisionAndScale(10, 4));
        }
        DataType::Nvarchar(_) | DataType::Varchar(_) => *kind = DataType::Varchar(None),
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("uniqueidentifier") => {
            *kind = DataType::Uuid;
        }
        DataType::Bit(_) => *kind = DataType::Boolean,
        DataType::Datetime(_) => *kind = DataType::Timestamp(None, TimezoneInfo::None),
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("smalldatetime") => {
            *kind = DataType::Timestamp(None, TimezoneInfo::None);
        }
        DataType::Varbinary(_) | DataType::Binary(_) => *kind = DataType::Blob(None),
        DataType::TinyInt(_) => *kind = DataType::UTinyInt,
        DataType::Time(scale, TimezoneInfo::None) if scale.is_none_or(|s| s <= 7) => {
            *kind = DataType::Custom(ObjectName::from(vec![Ident::new("TIME_NS")]), vec![]);
        }
        _ => {}
    }
    ControlFlow::Continue(())
}
impl VisitorMut for Translator<'_> {
    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<String> {
        if let Err(error) = msduck_sql::generate_series::lower(factor, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::openjson::lower(factor) {
            return ControlFlow::Break(error);
        }
        ControlFlow::Continue(())
    }
    type Break = String;
    fn pre_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<String> {
        if ident.quote_style == Some('[') {
            ident.quote_style = Some('"');
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_statement(&mut self, stmt: &mut Statement) -> ControlFlow<String> {
        if let Statement::Insert(insert) = stmt {
            // INTO is optional in T-SQL but required by DuckDB.
            insert.into = true;
        }
        // A MERGE can be nested in a WITH query body, which otherwise passes
        // the top-level Query allowlist. Keep its execution boundary uniform.
        if matches!(stmt, Statement::Merge(_)) {
            return ControlFlow::Break("unsupported MERGE execution".into());
        }
        if let Statement::CreateView(view) = stmt
            && view.or_alter
        {
            view.or_alter = false;
            view.or_replace = true;
        }
        if let Statement::AlterTable(table) = stmt {
            for operation in &mut table.operations {
                if let AlterTableOperation::AlterColumn {
                    op: AlterColumnOperation::SetDataType { data_type, .. },
                    ..
                } = operation
                {
                    if let DataType::Time(Some(scale), _) = data_type
                        && *scale > 7
                    {
                        return ControlFlow::Break("invalid time scale".into());
                    }
                    if let Some(storage) = crate::character_storage::unicode_storage_type(data_type)
                    {
                        *data_type = storage;
                    } else {
                        translate_type(data_type)?;
                    }
                }
                if let AlterTableOperation::AddColumn { column_def, .. } = operation {
                    if let Err(error) = crate::declared_columns::lower_collation(column_def) {
                        return ControlFlow::Break(error);
                    }
                    msduck_sql::money_cast::column(column_def);
                    if let Err(error) = crate::character_storage::column(column_def) {
                        return ControlFlow::Break(error);
                    }
                    crate::variant_pack::column(column_def);
                    if let Err(error) = crate::datetimeoffset_cast::column(column_def) {
                        return ControlFlow::Break(error);
                    }
                    if let Err(error) = crate::datetime2_cast::column(column_def) {
                        return ControlFlow::Break(error);
                    }
                    if let DataType::Time(Some(scale), _) = column_def.data_type
                        && scale > 7
                    {
                        return ControlFlow::Break("invalid time scale".into());
                    }
                    translate_type(&mut column_def.data_type)?;
                }
            }
        }
        if let Statement::CreateTable(table) = stmt {
            for col in &mut table.columns {
                if let Err(error) = crate::declared_columns::lower_collation(col) {
                    return ControlFlow::Break(error);
                }
                msduck_sql::money_cast::column(col);
                if let Err(error) = crate::character_storage::column(col) {
                    return ControlFlow::Break(error);
                }
                crate::variant_pack::column(col);
                if let Err(error) = crate::datetimeoffset_cast::column(col) {
                    return ControlFlow::Break(error);
                }
                if let Err(error) = crate::datetime2_cast::column(col) {
                    return ControlFlow::Break(error);
                }
                if let DataType::Time(scale, _) = &col.data_type
                    && scale.is_some_and(|s| s > 7)
                {
                    return ControlFlow::Break("invalid time scale".into());
                }
                translate_type(&mut col.data_type)?;
            }
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_order_by_expr(&mut self, order: &mut OrderByExpr) -> ControlFlow<String> {
        if order.options.nulls_first.is_none() {
            order.options.nulls_first =
                Some(!matches!(order.options.sort, Some(OrderBySort::Desc)));
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<String> {
        if let Err(error) = crate::named_windows::select(select) {
            return ControlFlow::Break(error);
        }
        for item in &select.projection {
            if let SelectItem::ExprWithAlias { alias, .. } = item
                && alias.quote_style.is_none()
                && alias.value.starts_with('@')
            {
                return ControlFlow::Break("unsupported SELECT variable assignment".into());
            }
            if let SelectItem::UnnamedExpr(Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                ..
            }) = item
                && matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@') && !id.value.starts_with("@@"))
            {
                return ControlFlow::Break("unsupported SELECT variable assignment".into());
            }
        }
        for table in &mut select.from {
            crate::apply::lower(table);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
        if let Err(error) = crate::named_windows::query(query) {
            return ControlFlow::Break(error);
        }
        crate::top::wrap_set_branches(&mut query.body);
        if let Err(error) = crate::top::paging(query) {
            return ControlFlow::Break(error);
        }
        if let SetExpr::Select(select) = query.body.as_mut()
            && let Some(top) = select.top.take()
        {
            if top.percent || top.with_ties {
                return ControlFlow::Break("unsupported TOP PERCENT/WITH TIES".into());
            }
            if query.limit_clause.is_some() || query.fetch.is_some() {
                return ControlFlow::Break(
                    "TOP cannot be combined with OFFSET/FETCH in the same query".into(),
                );
            }
            let limit = match top.quantity {
                Some(TopQuantity::Constant(n)) => number(n),
                Some(TopQuantity::Expr(e)) => e,
                None => return ControlFlow::Break("TOP requires a count".into()),
            };
            query.limit_clause = Some(LimitClause::LimitOffset {
                // DuckDB folds NULL scalar inputs without invoking native code.
                // A negative sentinel makes NULL fail the same count validator.
                limit: Some(unary_function(
                    "__msduck_top_count",
                    binary_function("coalesce", limit, number(-1)),
                )),
                offset: None,
                limit_by: vec![],
            });
        }
        ControlFlow::Continue(())
    }
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        crate::grouping::lower(expr);
        crate::variant_pack::canonicalize(expr);
        crate::case_types::lower_literal(expr);
        crate::datetime2_results::lower(expr);
        crate::variant_results::lower(expr);
        crate::datetimeoffset_compare::lower(expr);
        crate::datetime2_compare::lower(expr);
        crate::variant_compare::lower(expr);
        crate::datetime2_date::lower(expr);
        // Parameter replacement can introduce a cast after the pre-visit pass.
        if let Err(error) = crate::datetimeoffset_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetime2_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Expr::Convert {
            expr: value,
            data_type: Some(kind @ DataType::Time(_, TimezoneInfo::None)),
            is_try,
            styles,
            charset: None,
            ..
        } = expr
            && styles.is_empty()
        {
            *expr = Expr::Cast {
                kind: if *is_try {
                    CastKind::TryCast
                } else {
                    CastKind::Cast
                },
                expr: value.clone(),
                data_type: kind.clone(),
                format: None,
            };
        }
        if let Expr::Cast {
            data_type: DataType::Time(scale, TimezoneInfo::None),
            ..
        } = expr
        {
            let scale = scale.unwrap_or(7);
            if scale > 7 {
                return ControlFlow::Break("invalid time scale".into());
            }
            if let Expr::Cast {
                expr: argument,
                data_type,
                kind,
                ..
            } = expr
            {
                if matches!(argument.as_ref(), Expr::Value(v) if matches!(v.value, sqlparser::ast::Value::Placeholder(_)))
                {
                    // Keep concrete typing on directly bound TIME parameters.
                    translate_type(data_type)?;
                } else {
                    *expr = unary_function(
                        if matches!(kind, CastKind::TryCast | CastKind::SafeCast) {
                            "__msduck_try_time"
                        } else {
                            "__msduck_cast_time"
                        },
                        *argument.clone(),
                    );
                }
            }
            // Parse a fixed function skeleton, then attach the already-visited
            // expression as an AST node: values remain bound exactly once.
            let parsed = Parser::new(&sqlparser::dialect::GenericDialect {})
                .try_with_sql("__msduck_time_round(NULL, NULL)")
                .and_then(|mut parser| parser.parse_expr());
            let Ok(Expr::Function(mut function)) = parsed else {
                return ControlFlow::Break("failed to construct time conversion".into());
            };
            if let FunctionArguments::List(args) = &mut function.args {
                args.args = vec![
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr.clone())),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(number(
                        10u64.pow(9 - scale as u32),
                    ))),
                ];
            }
            *expr = Expr::Function(function);
        }
        if let Expr::Function(function) = expr {
            if function.name.to_string().eq_ignore_ascii_case("COUNT") {
                *expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(expr.clone()),
                    data_type: DataType::Int(None),
                    format: None,
                };
            } else if function.name.to_string().eq_ignore_ascii_case("COUNT_BIG") {
                function.name = ObjectName::from(vec![Ident::new("count")]);
            }
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        msduck_sql::expr::lower_unary_plus(expr);
        if let Err(error) = crate::concat_lower::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::unicode_case::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datalength::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::string_escape::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::json_extract::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::isjson::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetimeoffsetfromparts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::timefromparts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetime2fromparts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = msduck_sql::nullif::lower_currency(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        msduck_sql::money_arithmetic::lower(expr, self.parameters, &|_| None);
        msduck_sql::decimal_division::lower(expr, self.parameters, &|_| None);
        if let Err(error) = msduck_sql::left_right::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = msduck_sql::replicate::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        msduck_sql::money_compare::lower(expr, self.parameters, &|_| None);
        msduck_sql::money_results::lower(expr, self.parameters, &|_| None);
        if let Err(error) = crate::isnull::lower(expr) {
            return ControlFlow::Break(error);
        }
        crate::result_types::lower_fixed_results(expr);
        if let Err(error) = crate::type_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        crate::variant_pack::lower(expr);
        if let Err(error) = crate::variant::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::column_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::object_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::schema_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::identity_metadata::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = msduck_sql::money_format::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        if matches!(expr, Expr::Cast { expr: source, .. }
            if msduck_sql::projection::character_extrema::bound_result(source))
        {
            crate::concat_lower::annotated_unicode_casts(expr);
        }
        if let Err(error) = crate::varchar::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::nvarchar::lower(expr) {
            return ControlFlow::Break(error);
        }
        // ISNULL(NULL, replacement) can expose a DATETIME2 cast at this node.
        if let Err(error) = crate::datetimeoffset_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetime2_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::aggregate::mark(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if msduck_sql::projection::character_extrema::lower(expr) {
            crate::concat_lower::annotated_unicode_casts(expr);
            let Expr::Function(function) = expr else {
                unreachable!()
            };
            let ansi = function.name.to_string().ends_with("_ansi");
            let FunctionArguments::List(args) = &mut function.args else {
                unreachable!()
            };
            let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &mut args.args[0] else {
                unreachable!()
            };
            fn operand(value: Expr) -> Expr {
                match value {
                    Expr::Nested(value) | Expr::Collate { expr: value, .. } => operand(*value),
                    value => msduck_sql::expr::unary_function("__msduck_carrier_input", value),
                }
            }
            *value = operand(value.clone());
            if ansi {
                *expr = msduck_sql::expr::binary_function(
                    "__msduck_cast_carrier_varchar",
                    expr.clone(),
                    msduck_sql::expr::number(-1),
                );
            }
        }
        if let Err(error) = crate::percentile::lower(expr) {
            return ControlFlow::Break(error);
        }
        crate::ntile::lower(expr);
        if let Err(error) = crate::value_window::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::switchoffset::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datediff::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::dateadd::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::eomonth::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datepart::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::calendar_parts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::ncharacter::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::character::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::space::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::unicode::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::nullif::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::choose::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Expr::Function(function) = expr {
            match crate::predicate::iif_args(function) {
                Ok(Some([test, yes, no])) => {
                    *expr = Expr::Case {
                        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                        operand: None,
                        conditions: vec![CaseWhen {
                            condition: test.clone(),
                            result: yes.clone(),
                        }],
                        else_result: Some(Box::new(no.clone())),
                    };
                }
                Err(error) => return ControlFlow::Break(error),
                Ok(None) => {}
            }
        }
        crate::case_types::lower(expr, self.parameters);
        msduck_sql::money_cast::lower(expr);
        if let Expr::UnaryOp {
            op: UnaryOperator::BitwiseNot,
            expr: value,
        } = expr
        {
            *expr = unary_function("__msduck_bitnot", *value.clone());
        }
        if let Expr::Convert {
            is_try,
            expr: argument,
            data_type: Some(kind),
            charset: None,
            styles,
            ..
        } = expr
            && (integral_type(kind)
                || matches!(kind, DataType::Bit(_) | DataType::Datetime(_))
                || matches!(kind, DataType::Custom(name,_) if name.to_string().eq_ignore_ascii_case("smalldatetime")))
            && styles.is_empty()
        {
            *expr = Expr::Cast {
                kind: if *is_try {
                    CastKind::TryCast
                } else {
                    CastKind::Cast
                },
                expr: argument.clone(),
                data_type: kind.clone(),
                format: None,
            };
        }
        if let Expr::BinaryOp { left, op, right } = expr {
            if matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
                    | BinaryOperator::Modulo
            ) {
                crate::case_types::integer_comparison(left, right, self.parameters);
            }
            if matches!(
                op,
                BinaryOperator::Eq
                    | BinaryOperator::NotEq
                    | BinaryOperator::Lt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::GtEq
            ) {
                crate::case_types::integer_comparison(left, right, self.parameters);
            } else if *op == BinaryOperator::Plus
                && string_expr(left, self.parameters)
                && string_expr(right, self.parameters)
            {
                *op = BinaryOperator::StringConcat;
            } else if matches!(
                op,
                BinaryOperator::BitwiseAnd | BinaryOperator::BitwiseOr | BinaryOperator::BitwiseXor
            ) {
                let name = match op {
                    BinaryOperator::BitwiseAnd => "__msduck_bitand",
                    BinaryOperator::BitwiseOr => "__msduck_bitor",
                    _ => "__msduck_bitxor",
                };
                *expr = binary_function(name, *left.clone(), *right.clone());
            } else if matches!(op, BinaryOperator::Divide | BinaryOperator::Modulo)
                && integral_expr(left, self.parameters)
                && integral_expr(right, self.parameters)
            {
                let name = if *op == BinaryOperator::Divide {
                    "__msduck_int_div"
                } else {
                    "__msduck_int_mod"
                };
                *expr = binary_function(name, *left.clone(), *right.clone());
            }
        }
        match expr {
            Expr::Identifier(id) if id.value.starts_with("@@") => {
                *expr = match id.value.to_uppercase().as_str() {
                    "@@DATEFIRST" => Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(unary_function(
                            "getvariable",
                            Expr::Value(
                                sqlparser::ast::Value::SingleQuotedString(
                                    "__msduck_datefirst".into(),
                                )
                                .into(),
                            ),
                        )),
                        data_type: DataType::UTinyInt,
                        format: None,
                    },
                    "@@TRANCOUNT" => number(self.transactions),
                    "@@ROWCOUNT" => number(self.rowcount),
                    "@@ERROR" => number(self.last_error),
                    "@@VERSION" => Expr::Value(
                        sqlparser::ast::Value::SingleQuotedString(
                            "Microsoft SQL Server compatible msduck (DuckDB)".into(),
                        )
                        .into(),
                    ),
                    _ => return ControlFlow::Break(format!("unsupported global {}", id.value)),
                };
            }
            Expr::Identifier(id) if id.value.starts_with('@') => {
                let name = id.value.to_lowercase();
                let Some(value) = self.parameters.get(&name) else {
                    return ControlFlow::Break(format!("Must declare the scalar variable {name}"));
                };
                let slot = if let Some(slot) = self.parameter_slots.get(&name) {
                    *slot
                } else {
                    let bound = match crate::backend_value::to_backend(&value.value) {
                        Ok(bound) => bound,
                        Err(error) => return ControlFlow::Break(error.to_string()),
                    };
                    self.values.push(bound);
                    let slot = self.values.len();
                    self.parameter_slots.insert(name, slot);
                    slot
                };
                if matches!(value.value, ParameterValue::Unicode(_)) {
                    *expr = unary_function(
                        "__msduck_unicode_from_le",
                        Expr::Value(sqlparser::ast::Value::Placeholder(format!("${slot}")).into()),
                    );
                    return ControlFlow::Continue(());
                }
                *expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::Value(
                        sqlparser::ast::Value::Placeholder(format!("${slot}")).into(),
                    )),
                    data_type: {
                        let mut kind = value.ast_type();
                        if !matches!(
                            value.data_type,
                            SqlType::Time(_) | SqlType::DateTime2(_) | SqlType::DateTimeOffset(_)
                        ) {
                            translate_type(&mut kind)?;
                        }
                        kind
                    },
                    format: None,
                };
            }
            Expr::Value(v) => {
                if let sqlparser::ast::Value::HexStringLiteral(s) = &v.value {
                    *expr = unary_function(
                        "from_hex",
                        Expr::Value(sqlparser::ast::Value::SingleQuotedString(s.clone()).into()),
                    );
                    return ControlFlow::Continue(());
                }
                if let sqlparser::ast::Value::NationalStringLiteral(s) = &v.value {
                    v.value = sqlparser::ast::Value::SingleQuotedString(s.clone());
                }
            }
            Expr::Trim {
                expr: source,
                trim_where,
                trim_what,
                trim_characters,
            } => {
                if trim_characters.is_some() {
                    return ControlFlow::Break("unsupported TRIM argument syntax".into());
                }
                if let Some(characters) = trim_what {
                    if max_length_expr(characters, self.parameters) {
                        return ControlFlow::Break("TRIM characters cannot have a MAX type".into());
                    }
                } else {
                    *trim_what = Some(Box::new(Expr::Value(
                        sqlparser::ast::Value::SingleQuotedString(" ".into()).into(),
                    )));
                }
                let name = match trim_where {
                    Some(TrimWhereField::Leading) => "__msduck_ltrim",
                    Some(TrimWhereField::Trailing) => "__msduck_rtrim",
                    _ => "__msduck_trim",
                };
                *expr = msduck_sql::expr::binary_function(
                    name,
                    crate::concat_lower::trim_input(*source.clone(), self.parameters),
                    *trim_what.clone().unwrap(),
                );
            }
            Expr::Cast {
                expr: argument,
                data_type,
                kind,
                ..
            } if !matches!(data_type, DataType::Time(..)) => {
                let explicit = crate::variant_cast::take(argument);
                if explicit && matches!(data_type, DataType::Bit(_)) {
                    **argument = unary_function(
                        if matches!(kind, CastKind::TryCast | CastKind::SafeCast) {
                            "__msduck_explicit_try_bit"
                        } else {
                            "__msduck_explicit_bit"
                        },
                        *argument.clone(),
                    );
                }
                if integral_type(data_type) && !money_expr(argument, self.parameters) {
                    **argument = integer_input(
                        *argument.clone(),
                        data_type,
                        matches!(kind, CastKind::TryCast | CastKind::SafeCast),
                    );
                    if explicit
                        && let Expr::Function(f) = argument.as_mut()
                        && f.name.to_string() == "__msduck_integer_input"
                    {
                        f.name =
                            ObjectName::from(vec![Ident::new("__msduck_explicit_integer_input")]);
                    }
                }
                translate_type(data_type)?
            }
            Expr::Function(f) => {
                let name = f.name.to_string().to_uppercase();
                if name == "DATEFROMPARTS" {
                    if !matches!(&f.args, FunctionArguments::List(args)
                        if args.args.len() == 3 && args.duplicate_treatment.is_none()
                            && args.clauses.is_empty()
                            && args.args.iter().all(|arg| matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))))
                        || !matches!(f.parameters, FunctionArguments::None)
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                    {
                        return ControlFlow::Break(
                            "DATEFROMPARTS requires three scalar arguments".into(),
                        );
                    }
                    f.name = ObjectName::from(vec![Ident::new("__msduck_datefromparts")]);
                    if let FunctionArguments::List(args) = &mut f.args {
                        for argument in &mut args.args {
                            if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = argument {
                                *value = Expr::Cast {
                                    kind: CastKind::Cast,
                                    expr: Box::new(value.clone()),
                                    data_type: DataType::Int(None),
                                    format: None,
                                };
                            }
                        }
                    }
                    return ControlFlow::Continue(());
                }
                if matches!(name.as_str(), "LTRIM" | "RTRIM") {
                    let FunctionArguments::List(args) = &mut f.args else {
                        return ControlFlow::Break(
                            "trim function requires scalar arguments".into(),
                        );
                    };
                    if !(1..=2).contains(&args.args.len())
                        || args.duplicate_treatment.is_some()
                        || !args.clauses.is_empty()
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                        || !matches!(f.parameters, FunctionArguments::None)
                        || !args.args.iter().all(|arg| {
                            matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))
                        })
                    {
                        return ControlFlow::Break(
                            "trim function requires one or two scalar arguments".into(),
                        );
                    }
                    if args.args.len() == 1 {
                        args.args
                            .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                                sqlparser::ast::Value::SingleQuotedString(" ".into()).into(),
                            ))));
                    } else if let FunctionArg::Unnamed(FunctionArgExpr::Expr(characters)) =
                        &args.args[1]
                        && max_length_expr(characters, self.parameters)
                    {
                        return ControlFlow::Break("trim characters cannot have a MAX type".into());
                    }
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(source)) = &mut args.args[0] {
                        *source = crate::concat_lower::trim_input(source.clone(), self.parameters);
                    }
                    f.name = ObjectName::from(vec![Ident::new(if name == "LTRIM" {
                        "__msduck_ltrim"
                    } else {
                        "__msduck_rtrim"
                    })]);
                    return ControlFlow::Continue(());
                }
                if name == "LEN" {
                    let FunctionArguments::List(args) = &f.args else {
                        return ControlFlow::Break("LEN requires one argument".into());
                    };
                    if args.args.len() != 1
                        || args.duplicate_treatment.is_some()
                        || !args.clauses.is_empty()
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                        || !matches!(f.parameters, FunctionArguments::None)
                    {
                        return ControlFlow::Break("LEN requires one scalar argument".into());
                    }
                    let FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) = &args.args[0] else {
                        return ControlFlow::Break("LEN requires one scalar argument".into());
                    };
                    let large = max_length_expr(arg, self.parameters);
                    f.name = ObjectName::from(vec![Ident::new("__msduck_len")]);
                    *expr = Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(Expr::Function(f.clone())),
                        data_type: if large {
                            DataType::BigInt(None)
                        } else {
                            DataType::Int(None)
                        },
                        format: None,
                    };
                    return ControlFlow::Continue(());
                }
                if name == "NEWID" {
                    if !matches!(&f.args, FunctionArguments::List(args)
                        if args.args.is_empty() && args.duplicate_treatment.is_none() && args.clauses.is_empty())
                        || !matches!(f.parameters, FunctionArguments::None)
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                    {
                        return ControlFlow::Break(
                            "NEWID requires no arguments or aggregate clauses".into(),
                        );
                    }
                    // Keep this as a volatile database expression, including
                    // column defaults; never generate a constant at translation.
                    f.name = ObjectName::from(vec![Ident::new("uuid")]);
                    return ControlFlow::Continue(());
                }

                if matches!(
                    name.as_str(),
                    "ERROR_NUMBER"
                        | "ERROR_STATE"
                        | "ERROR_MESSAGE"
                        | "ERROR_SEVERITY"
                        | "ERROR_LINE"
                        | "ERROR_PROCEDURE"
                ) {
                    if !matches!(&f.args, FunctionArguments::List(args) if args.args.is_empty()) {
                        return ControlFlow::Break("error functions take no arguments".into());
                    }
                    let data_type =
                        msduck_sql::session_function::error_type(f).expect("known error function");
                    if name == "ERROR_MESSAGE"
                        && let Some(units) =
                            self.caught_error.and_then(|e| e.message_utf16.as_ref())
                    {
                        self.values.push(duckdb::types::Value::Blob(
                            units.iter().flat_map(|u| u.to_le_bytes()).collect(),
                        ));
                        *expr = binary_function(
                            "__msduck_cast_carrier_nvarchar",
                            unary_function(
                                "__msduck_unicode_from_le",
                                Expr::Value(
                                    sqlparser::ast::Value::Placeholder(format!(
                                        "${}",
                                        self.values.len()
                                    ))
                                    .into(),
                                ),
                            ),
                            number(4000),
                        );
                        return ControlFlow::Continue(());
                    }
                    let value = match (self.caught_error, name.as_str()) {
                        (Some(error), "ERROR_NUMBER") => number(error.number),
                        (Some(error), "ERROR_STATE") => number(error.state),
                        (Some(error), "ERROR_MESSAGE") => Expr::Value(
                            sqlparser::ast::Value::NationalStringLiteral(error.message.clone())
                                .into(),
                        ),
                        (Some(error), "ERROR_SEVERITY") => number(error.severity),
                        (Some(_), "ERROR_LINE") => number(1),
                        _ => Expr::Value(sqlparser::ast::Value::Null.into()),
                    };
                    *expr = Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(value),
                        data_type,
                        format: None,
                    };
                    return ControlFlow::Continue(());
                }
                if msduck_sql::session_function::is_xact_state(f) {
                    *expr = msduck_sql::session_function::xact_state(self.transactions > 0);
                    if self.transaction_doomed
                        && let Expr::Cast { expr, .. } = expr
                    {
                        **expr = number(-1);
                    }
                } else if msduck_sql::session_function::is_original_login(f) {
                    *expr = msduck_sql::session_function::original_login(self.original_login);
                } else if f.name.to_string().eq_ignore_ascii_case("DB_NAME") {
                    *expr = Expr::Value(
                        sqlparser::ast::Value::SingleQuotedString("master".into()).into(),
                    );
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}
fn wire_type(kind: &ArrowType) -> Result<Type> {
    if crate::unicode_carrier::is_arrow(kind) {
        return Ok(Type::Text);
    }
    if crate::variant::is_arrow(kind) {
        return Ok(Type::Variant);
    }
    if let Some(scale) = crate::datetimeoffset_cast::arrow_scale(kind) {
        return Ok(Type::DateTimeOffset(scale));
    }
    if let Some(scale) = crate::datetime2_cast::arrow_scale(kind) {
        return Ok(Type::DateTime2(scale));
    }
    Ok(match kind {
        ArrowType::Int8 => Type::Int(2),
        ArrowType::UInt8 => Type::Int(1),
        ArrowType::Int16 => Type::Int(2),
        ArrowType::Int32 => Type::Int(4),
        ArrowType::Int64 => Type::Int(8),
        ArrowType::Boolean => Type::Bit,
        ArrowType::Float32 => Type::Float(4),
        ArrowType::Float64 => Type::Float(8),
        ArrowType::Utf8 | ArrowType::LargeUtf8 => Type::Text,
        ArrowType::Binary | ArrowType::LargeBinary => Type::Binary,
        ArrowType::Decimal128(p, s) if *s >= 0 => Type::Decimal(*p, *s as u8),
        ArrowType::Date32 => Type::Date,
        ArrowType::Time64(_) => Type::Time(7),
        ArrowType::Timestamp(_, None) => Type::DateTime,
        _ => bail!("unsupported result type {kind:?}"),
    })
}
// Read Arrow directly: duckdb-rs Row::get does not support TIME_NS and panics.
fn is_bool_field(field: &duckdb::arrow::datatypes::Field) -> bool {
    field.data_type() == &ArrowType::Boolean
        || (field.data_type() == &ArrowType::Int8
            && field
                .metadata()
                .get("ARROW:extension:name")
                .is_some_and(|name| name == "arrow.bool8"))
}
fn is_uuid_field(field: &duckdb::arrow::datatypes::Field) -> bool {
    field.data_type() == &ArrowType::FixedSizeBinary(16)
        && field
            .metadata()
            .get("ARROW:extension:name")
            .is_some_and(|name| name == "arrow.uuid")
}
fn arrow_value(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
    field: &duckdb::arrow::datatypes::Field,
) -> Result<Value> {
    use duckdb::arrow::{array::*, datatypes::TimeUnit as Unit};
    if array.is_null(row) {
        return Ok(Value::Null);
    }
    if crate::unicode_carrier::is_arrow(array.data_type()) {
        return crate::unicode_carrier::read(array, row);
    }
    if crate::variant::is_arrow(array.data_type()) {
        return crate::variant::read(array, row);
    }
    if let Some(scale) = crate::datetimeoffset_cast::arrow_scale(array.data_type()) {
        let values = array
            .as_any()
            .downcast_ref::<StructArray>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIMEOFFSET array"))?;
        let ticks = values
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIMEOFFSET ticks"))?;
        let offset = values
            .column(1)
            .as_any()
            .downcast_ref::<Int16Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIMEOFFSET offset"))?;
        ensure!(
            !ticks.is_null(row) && !offset.is_null(row),
            "invalid DATETIMEOFFSET value"
        );
        return Ok(Value::Text(
            crate::datetimeoffset::DateTimeOffset::from_utc(
                crate::datetime2::DateTime2::from_ticks(ticks.value(row))?,
                offset.value(row),
            )?
            .format_iso(scale)?,
        ));
    }
    if let Some(scale) = crate::datetime2_cast::arrow_scale(array.data_type()) {
        let values = array
            .as_any()
            .downcast_ref::<StructArray>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIME2 array"))?;
        let ticks = values
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIME2 ticks"))?;
        ensure!(!ticks.is_null(row), "invalid DATETIME2 ticks");
        return Ok(Value::Text(
            crate::datetime2::DateTime2::from_ticks(ticks.value(row))?.format_iso(scale)?,
        ));
    }
    macro_rules! get {
        ($ty:ty) => {
            array
                .as_any()
                .downcast_ref::<$ty>()
                .ok_or_else(|| anyhow::anyhow!("unexpected Arrow array representation"))?
                .value(row)
        };
    }
    Ok(match array.data_type() {
        ArrowType::FixedSizeBinary(16) if is_uuid_field(field) => {
            Value::Text(uuid::Uuid::from_slice(get!(FixedSizeBinaryArray))?.to_string())
        }
        ArrowType::Int8 if is_bool_field(field) => Value::Boolean(get!(Int8Array) != 0),
        ArrowType::Int8 => Value::TinyInt(get!(Int8Array)),
        ArrowType::UInt8 => Value::UTinyInt(get!(UInt8Array)),
        ArrowType::Int16 => Value::SmallInt(get!(Int16Array)),
        ArrowType::Int32 => Value::Int(get!(Int32Array)),
        ArrowType::Int64 => Value::BigInt(get!(Int64Array)),
        ArrowType::Boolean => Value::Boolean(get!(BooleanArray)),
        ArrowType::Float32 => Value::Float(get!(Float32Array)),
        ArrowType::Float64 => Value::Double(get!(Float64Array)),
        ArrowType::Utf8 => Value::Text(get!(StringArray).to_owned()),
        ArrowType::LargeUtf8 => Value::Text(get!(LargeStringArray).to_owned()),
        ArrowType::Binary => Value::Blob(get!(BinaryArray).to_owned()),
        ArrowType::LargeBinary => Value::Blob(get!(LargeBinaryArray).to_owned()),
        ArrowType::Decimal128(p, s) if *s >= 0 => Value::Decimal(duckdb::types::Decimal::new(
            *p,
            *s as u8,
            get!(Decimal128Array),
        )?),
        ArrowType::Date32 => Value::Date32(get!(Date32Array)),
        ArrowType::Time64(Unit::Microsecond) => {
            Value::Time64(TimeUnit::Microsecond, get!(Time64MicrosecondArray))
        }
        ArrowType::Time64(Unit::Nanosecond) => {
            Value::Time64(TimeUnit::Nanosecond, get!(Time64NanosecondArray))
        }
        ArrowType::Timestamp(Unit::Second, None) => {
            Value::Timestamp(TimeUnit::Second, get!(TimestampSecondArray))
        }
        ArrowType::Timestamp(Unit::Millisecond, None) => {
            Value::Timestamp(TimeUnit::Millisecond, get!(TimestampMillisecondArray))
        }
        ArrowType::Timestamp(Unit::Microsecond, None) => {
            Value::Timestamp(TimeUnit::Microsecond, get!(TimestampMicrosecondArray))
        }
        ArrowType::Timestamp(Unit::Nanosecond, None) => {
            Value::Timestamp(TimeUnit::Nanosecond, get!(TimestampNanosecondArray))
        }
        other => bail!("unsupported result type {other:?}"),
    })
}
fn ticks(unit: TimeUnit, value: i64) -> Result<i64> {
    match unit {
        TimeUnit::Second => value.checked_mul(10_000_000),
        TimeUnit::Millisecond => value.checked_mul(10_000),
        TimeUnit::Microsecond => value.checked_mul(10),
        TimeUnit::Nanosecond => Some(value / 100),
    }
    .ok_or_else(|| anyhow::anyhow!("date/time overflow"))
}
pub fn encode_column(out: &mut Vec<u8>, column: &Column, value: &Value) -> Result<()> {
    encode_value_mode(
        out,
        &column.kind,
        value,
        column.fixed_scalar_type().is_some(),
    )
}
pub fn encode_value(out: &mut Vec<u8>, kind: &Type, value: &Value) -> Result<()> {
    encode_value_mode(out, kind, value, false)
}
fn encode_value_mode(out: &mut Vec<u8>, kind: &Type, value: &Value, fixed: bool) -> Result<()> {
    ensure!(
        !fixed || !matches!(value, Value::Null),
        "NULL cannot be encoded as a fixed scalar"
    );
    if matches!(kind, Type::Variant) {
        return crate::variant::encode(out, value);
    }
    if matches!(kind, Type::Text | Type::Nvarchar(_) | Type::Nchar(_)) {
        match value {
            Value::Null => return tds::unicode_value(out, kind, None),
            Value::Struct(_) => return crate::unicode_carrier::encode(out, kind, value),
            Value::Text(value) => {
                let units: Vec<_> = value.encode_utf16().collect();
                return tds::unicode_value(out, kind, Some(&units));
            }
            _ => {}
        }
    }
    if matches!(value, Value::Null) {
        if matches!(kind, Type::Varchar(u16::MAX)) {
            out.extend(u64::MAX.to_le_bytes());
            return Ok(());
        }
        if matches!(
            kind,
            Type::Varchar(_)
                | Type::Char(_)
                | Type::Nvarchar(_)
                | Type::Nchar(_)
                | Type::Varbinary(_)
                | Type::FixedBinary(_)
        ) {
            out.extend(u16::MAX.to_le_bytes());
        } else if matches!(kind, Type::Text | Type::Binary) {
            out.extend(u64::MAX.to_le_bytes());
        } else {
            out.push(0);
        }
        return Ok(());
    }
    match (kind, value) {
        (Type::Varchar(width) | Type::Char(width), Value::Text(value)) => {
            let bytes = tds::encode_cp1252(value)?;
            if matches!(kind, Type::Varchar(u16::MAX)) {
                out.extend((bytes.len() as u64).to_le_bytes());
                if !bytes.is_empty() {
                    out.extend((bytes.len() as u32).to_le_bytes());
                    out.extend(&bytes);
                }
                out.extend(0u32.to_le_bytes());
                return Ok(());
            }
            ensure!(
                *width <= 8000 && bytes.len() <= usize::from(*width),
                "VARCHAR value exceeds declared width"
            );
            ensure!(
                !matches!(kind, Type::Char(_)) || bytes.len() == usize::from(*width),
                "CHAR value does not match declared width"
            );
            out.extend((bytes.len() as u16).to_le_bytes());
            out.extend(bytes);
        }
        (Type::Guid, Value::Text(value)) => {
            out.push(16);
            out.extend(uuid::Uuid::parse_str(value)?.to_bytes_le());
        }
        (Type::Int(n), v) => {
            let int = match v {
                Value::TinyInt(v) => *v as i64,
                Value::UTinyInt(v) => *v as i64,
                Value::SmallInt(v) => *v as i64,
                Value::Int(v) => *v as i64,
                Value::BigInt(v) => *v,
                _ => bail!("integer value/type mismatch"),
            };
            ensure!(matches!(n, 1 | 2 | 4 | 8), "invalid integer wire width");
            ensure!(
                match n {
                    1 => u8::try_from(int).is_ok(),
                    2 => i16::try_from(int).is_ok(),
                    4 => i32::try_from(int).is_ok(),
                    8 => true,
                    _ => unreachable!(),
                },
                "integer result exceeds declared width"
            );
            if !fixed {
                out.push(*n);
            }
            out.extend(&int.to_le_bytes()[..*n as usize]);
        }
        (Type::Bit, Value::Boolean(v)) => {
            if !fixed {
                out.push(1);
            }
            out.push(u8::from(*v));
        }
        (Type::Float(4), Value::Float(v)) => {
            if !fixed {
                out.push(4);
            }
            out.extend(v.to_le_bytes());
        }
        (Type::Float(8), Value::Double(v)) => {
            if !fixed {
                out.push(8);
            }
            out.extend(v.to_le_bytes());
        }
        (Type::Binary, Value::Blob(v)) => plp(out, v),
        (Type::Varbinary(width) | Type::FixedBinary(width), Value::Blob(v)) => {
            ensure!(
                v.len() <= usize::from(*width),
                "binary value exceeds declared width"
            );
            ensure!(
                !matches!(kind, Type::FixedBinary(_)) || v.len() == usize::from(*width),
                "BINARY value does not match declared width"
            );
            out.extend((v.len() as u16).to_le_bytes());
            out.extend(v);
        }
        (Type::Money(width), Value::Decimal(v)) => {
            ensure!(v.scale() == 4, "money scale mismatch");
            if fixed {
                tds::fixed_money(out, *width, v.value())?;
            } else {
                tds::money(out, *width, Some(v.value()))?;
            }
        }
        (Type::Decimal(precision, scale), Value::Decimal(v)) => {
            ensure!(
                (1..=38).contains(precision) && *scale <= *precision,
                "invalid decimal metadata"
            );
            ensure!(v.scale() == *scale, "decimal scale mismatch");
            let magnitude = v.value().unsigned_abs();
            ensure!(
                magnitude < 10u128.pow(*precision as u32),
                "decimal result exceeds declared precision"
            );
            let width = tds::decimal_value_length(magnitude);
            out.extend([width, u8::from(v.value() >= 0)]);
            out.extend(&magnitude.to_le_bytes()[..width as usize - 1]);
        }
        (Type::Date, Value::Date32(v)) => {
            let days = *v as i64 + 719162;
            ensure!(
                (0..=3652058).contains(&days),
                "date outside SQL Server range"
            );
            out.push(3);
            out.extend(&days.to_le_bytes()[..3]);
        }
        (Type::Time(scale), Value::Time64(unit, v)) => {
            let time = ticks(*unit, *v)?;
            ensure!((0..864000000000).contains(&time), "invalid time");
            ensure!(*scale <= 7, "invalid TIME scale");
            let quantum = 10i64.pow(u32::from(7 - *scale));
            let units = ((time + quantum / 2) / quantum) % (864000000000 / quantum);
            let width = match scale {
                0..=2 => 3,
                3..=4 => 4,
                _ => 5,
            };
            out.push(width as u8);
            out.extend(&units.to_le_bytes()[..width]);
        }
        (Type::DateTimeOffset(scale), Value::Text(value)) => {
            let encoded =
                crate::datetimeoffset::DateTimeOffset::parse_iso(value)?.encode(*scale)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
        }
        (Type::DateTime2(scale), Value::Text(value)) => {
            let encoded = crate::datetime2::DateTime2::parse_iso(value)?.encode(*scale)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
        }
        (Type::LegacyDateTime(width), Value::Timestamp(unit, v)) => {
            let nanos = i128::from(*v)
                * match unit {
                    TimeUnit::Second => 1_000_000_000,
                    TimeUnit::Millisecond => 1_000_000,
                    TimeUnit::Microsecond => 1_000,
                    TimeUnit::Nanosecond => 1,
                };
            tds::legacy_datetime(out, *width, nanos, fixed)?;
        }
        (Type::DateTime, Value::Timestamp(unit, v)) => {
            let nanos = i128::from(*v)
                * match unit {
                    TimeUnit::Second => 1_000_000_000,
                    TimeUnit::Millisecond => 1_000_000,
                    TimeUnit::Microsecond => 1_000,
                    TimeUnit::Nanosecond => 1,
                };
            let encoded = crate::datetime2::DateTime2::from_unix_nanos(nanos)?.encode(7)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
        }
        _ => bail!("unsupported result value for {kind:?}"),
    }
    Ok(())
}
fn plp(out: &mut Vec<u8>, data: &[u8]) {
    out.extend((data.len() as u64).to_le_bytes());
    if !data.is_empty() {
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
    }
    out.extend(0u32.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn alignment_fields(count: usize) -> Vec<crate::query_catalog::Field> {
        (0..count)
            .map(|_| crate::query_catalog::Field {
                name: "unrelated".into(),
                info: None,
                json_fragment: false,
                properties: msduck_core::result::Properties::expression(false),
                collation: Some(Ok(msduck_core::collation::Label::Explicit(
                    "Latin1_General_100_BIN2".into(),
                ))),
            })
            .collect()
    }

    fn alignment_result(
        db: &Connection,
        sql: &str,
        fields: &[crate::query_catalog::Field],
        declared: &[Option<Type>],
    ) -> Result<(Vec<u8>, u64)> {
        let mut statement = db.prepare(sql)?;
        let batches = statement.query_arrow([])?;
        let schema = batches.get_schema();
        Session::encode_batches(batches, &schema, fields, declared)
    }

    #[test]
    fn descriptor_alignment_success_rejects_partial_facts() {
        let db = Connection::open_in_memory().unwrap();
        for sql in [
            "SELECT CAST(NULL AS INTEGER) AS n, 'long value' AS s",
            "SELECT 'long value' AS s, CAST(NULL AS INTEGER) AS n",
        ] {
            let baseline = alignment_result(&db, sql, &[], &[]).unwrap();
            for count in [1, 3] {
                assert_eq!(
                    alignment_result(&db, sql, &alignment_fields(count), &[]).unwrap(),
                    baseline,
                    "mismatched field count {count}: {sql}"
                );
                assert_eq!(
                    alignment_result(&db, sql, &[], &vec![Some(Type::Nvarchar(1)); count]).unwrap(),
                    baseline,
                    "mismatched override count {count}: {sql}"
                );
            }
        }
    }

    #[test]
    fn descriptor_alignment_errors_reject_partial_facts() {
        let db = Connection::open_in_memory().unwrap();
        for sql in [
            "SELECT CAST(NULL AS INTEGER) AS n, 'long value' AS s",
            "SELECT 'long value' AS s, CAST(NULL AS INTEGER) AS n",
        ] {
            let statement = db.prepare(sql).unwrap();
            let baseline = crate::query_error::describe(&statement, &[], &[]).unwrap();
            for count in [1, 3] {
                assert_eq!(
                    crate::query_error::describe(&statement, &alignment_fields(count), &[])
                        .unwrap(),
                    baseline,
                    "mismatched field count {count}: {sql}"
                );
                assert_eq!(
                    crate::query_error::describe(
                        &statement,
                        &[],
                        &vec![Some(Type::Nvarchar(1)); count],
                    )
                    .unwrap(),
                    baseline,
                    "mismatched override count {count}: {sql}"
                );
            }
        }
    }

    #[test]
    fn descriptor_alignment_preserves_complete_facts_on_success_and_error() {
        let db = Connection::open_in_memory().unwrap();
        let sql = "SELECT CAST(NULL AS INTEGER) AS n, 'text' AS s";
        let mut fields = alignment_fields(2);
        for field in &mut fields {
            field.name = "duplicate".into();
            field.properties = msduck_core::result::Properties::expression(true);
        }
        let declared = [None, Some(Type::Nvarchar(8))];
        let expected_columns = vec![
            Column {
                name: "duplicate".into(),
                kind: Type::Int(4),
                properties: fields[0].properties,
                collation: tds::collation::Collation::for_name("Latin1_General_100_BIN2"),
            },
            Column {
                name: "duplicate".into(),
                kind: Type::Nvarchar(8),
                properties: fields[1].properties,
                collation: tds::collation::Collation::for_name("Latin1_General_100_BIN2"),
            },
        ];
        let mut metadata = Vec::new();
        tds::metadata(&mut metadata, &expected_columns).unwrap();
        let statement = db.prepare(sql).unwrap();
        assert_eq!(
            crate::query_error::describe(&statement, &fields, &declared),
            Some(metadata.clone())
        );
        let (out, count) = alignment_result(&db, sql, &fields, &declared).unwrap();
        assert_eq!(count, 1);
        assert!(out.starts_with(&metadata));
        let empty = format!("{sql} WHERE false");
        assert_eq!(
            alignment_result(&db, &empty, &fields, &declared).unwrap(),
            (metadata, 0)
        );
    }

    #[test]
    fn public_bin2_comparisons_match_live_rows_with_nullable_case_metadata() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../reference/bin2-comparisons.json")).unwrap();
        let ansi: serde_json::Value =
            serde_json::from_str(include_str!("../reference/bin2-ansi-comparisons.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut checked = 0;
        for case in reference["results"]
            .as_array()
            .unwrap()
            .iter()
            .chain(ansi["results"].as_array().unwrap())
            .filter(|case| case["reference"]["errors"].as_array().unwrap().is_empty())
        {
            let sql = case["query"].as_str().unwrap();
            session.validate_prepared_sql(sql, &[]).unwrap();
            let (out, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(success, "{}: {out:?}", case["name"]);
            let set = &case["reference"]["sets"][0];
            let columns = set["columns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|col| Column {
                    name: col["name"].as_str().unwrap().into(),
                    kind: Type::Int(4),
                    collation: None,
                    // SQL Server folds the constant CASE conditions to prove
                    // non-null results. Our declaration pass remains nullable;
                    // client differential coverage preserves that metadata gap.
                    properties: msduck_core::result::Properties::expression(true),
                })
                .collect::<Vec<_>>();
            let mut expected = Vec::new();
            tds::metadata(&mut expected, &columns).unwrap();
            expected.push(0xd1);
            for value in set["rows"][0].as_array().unwrap() {
                if let Some(value) = value.as_i64() {
                    expected.push(4);
                    expected.extend((value as i32).to_le_bytes());
                } else {
                    expected.push(0);
                }
            }
            assert!(
                out.starts_with(&expected),
                "{}: expected {expected:?}, got {out:?}",
                case["name"]
            );
            checked += 1;
        }
        assert_eq!(checked, 13);
    }

    #[test]
    fn sensitive_collation_errors_are_compilation_errors_in_execution_and_prepare() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for (expression, operation) in [
            (
                "REPLACE(N'a' COLLATE Latin1_General_100_CI_AS,N'b' COLLATE Latin1_General_100_CS_AS,N'z')",
                "replace",
            ),
            (
                "STUFF(N'a' COLLATE Latin1_General_100_CI_AS,1,1,N'b' COLLATE Latin1_General_100_CS_AS)",
                "stuff",
            ),
            (
                "NULLIF(N'a' COLLATE Latin1_General_100_CI_AS,N'b' COLLATE Latin1_General_100_CS_AS)",
                "equal to",
            ),
        ] {
            for sql in [
                format!("SELECT ({expression}) COLLATE Latin1_General_100_BIN2 AS s"),
                format!("SELECT LEN({expression}) AS n"),
                format!("SELECT 1 AS n WHERE ({expression}) IS NULL"),
                format!("SELECT 1 AS n ORDER BY {expression}"),
            ] {
                let error = session.validate_prepared_sql(&sql, &[]).unwrap_err();
                let error = error.downcast_ref::<SqlError>().unwrap();
                assert_eq!((error.number, error.state, error.severity), (468, 9, 16));
                assert!(
                    error
                        .message
                        .ends_with(&format!("in the {operation} operation."))
                );
                let (out, success) = session.batch_response(&sql, &Default::default(), false, None);
                assert!(!success);
                assert_eq!(out[0], 0xaa, "compilation must not emit result metadata");
                assert_eq!(i32::from_le_bytes(out[3..7].try_into().unwrap()), 468);
                assert_eq!(&out[7..9], &[9, 16]);
            }
        }
        assert!(
            session
                .batch_response("SELECT 1 AS recovered", &Default::default(), false, None)
                .1
        );
    }

    #[test]
    fn result_collations_reach_rows_empty_results_and_error_descriptors() {
        use duckdb::arrow::{array::StringArray, datatypes::Field, record_batch::RecordBatch};
        use msduck_core::collation::Label;
        let schema = std::sync::Arc::new(duckdb::arrow::datatypes::Schema::new(vec![
            Field::new("a", ArrowType::Utf8, true),
            Field::new("b", ArrowType::Utf8, true),
        ]));
        let labels = ["Latin1_General_100_CS_AS", "Latin1_General_100_BIN2"];
        let fields = labels
            .iter()
            .zip(["a", "b"])
            .map(|(label, name)| crate::query_catalog::Field {
                name: name.into(),
                info: None,
                json_fragment: false,
                properties: Default::default(),
                collation: Some(Ok(Label::Explicit((*label).into()))),
            })
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        tds::metadata(
            &mut expected,
            &fields
                .iter()
                .zip(labels)
                .map(|(field, name)| Column {
                    name: field.name.clone(),
                    kind: Type::Text,
                    properties: field.properties,
                    collation: tds::collation::Collation::for_name(name),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for values in [vec![], vec![Some("a"), None, Some("🦆")]] {
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    std::sync::Arc::new(StringArray::from(values.clone())),
                    std::sync::Arc::new(StringArray::from(values.clone())),
                ],
            )
            .unwrap();
            let (out, count) =
                Session::encode_batches(std::iter::once(batch), &schema, &fields, &[]).unwrap();
            assert_eq!(count, values.len() as u64);
            assert!(out.starts_with(&expected));
            if values.is_empty() {
                assert_eq!(out, expected);
            }
        }
        let db = duckdb::Connection::open_in_memory().unwrap();
        let prepared = db
            .prepare("SELECT CAST(NULL AS VARCHAR) AS a, CAST(NULL AS VARCHAR) AS b")
            .unwrap();
        assert_eq!(
            crate::query_error::describe(&prepared, &fields, &[]).unwrap(),
            expected
        );
        assert!(crate::query_catalog::wire_collation(&fields, 1, 0).is_none());
        let mut unresolved = fields;
        unresolved[0].collation = Some(Ok(Label::NoCollation {
            left: labels[0].into(),
            right: labels[1].into(),
        }));
        assert!(crate::query_catalog::wire_collation(&unresolved, 2, 0).is_none());
    }

    #[test]
    fn typed_diagnostics_survive_context_without_reclassification() {
        fn wire(error: anyhow::Error) -> (i32, u8, u8, String) {
            let mut bytes = Vec::new();
            let number = emit_error(&mut bytes, &error);
            assert_eq!(bytes[0], 0xaa);
            assert_eq!(i32::from_le_bytes(bytes[3..7].try_into().unwrap()), number);
            let len = u16::from_le_bytes(bytes[9..11].try_into().unwrap()) as usize;
            let units = bytes[11..11 + len * 2]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>();
            (
                number,
                bytes[7],
                bytes[8],
                String::from_utf16(&units).unwrap(),
            )
        }
        let message = msduck_core::json_path::SCALAR;
        let explicit =
            anyhow::Error::new(SqlError::new(51000, 7, message)).context("outer execution context");
        assert_eq!(wire(explicit), (51000, 7, 16, message.into()));
        let wrapped = format!("Invalid Input Error: {message}");
        assert!(msduck_core::json_path::diagnostic(&wrapped).is_none());
        assert_eq!(
            wire(anyhow::anyhow!(wrapped)),
            (13623, 2, 16, message.into())
        );
        let syntax = "Window element in OVER clause can not also be specified in WINDOW clause.";
        assert_eq!(wire(anyhow::anyhow!(syntax)), (4123, 1, 15, syntax.into()));
        assert!(crate::json_extract::diagnostic(&format!("user text: {message}")).is_none());
        let numeric = "Arithmetic overflow error converting expression to data type money.";
        let decimal = "Arithmetic overflow error converting expression to data type numeric.";
        let decimal_wrapped = format!("Invalid Input Error: {decimal}");
        assert_eq!(
            wire(anyhow::anyhow!(decimal_wrapped.clone())),
            (8115, 2, 16, decimal.into())
        );
        assert_eq!(
            sql_error_from_message(&decimal_wrapped),
            SqlError::new(8115, 2, decimal)
        );
        assert_eq!(
            wire(anyhow::Error::new(SqlError::new(51002, 8, &decimal_wrapped)).context("outer")),
            (51002, 8, 16, decimal_wrapped)
        );
        let wrapped = format!("Invalid Input Error: {numeric}");
        assert_eq!(
            wire(anyhow::anyhow!(wrapped.clone())),
            (8115, 1, 16, numeric.into())
        );
        assert_eq!(
            sql_error_from_message(&wrapped),
            SqlError::new(8115, 1, numeric)
        );
        assert_eq!(
            wire(anyhow::Error::new(SqlError::new(51001, 9, &wrapped)).context("outer")),
            (51001, 9, 16, wrapped)
        );
        assert!(
            runtime_diagnostic(&format!("Invalid Input Error: {numeric} trailing text")).is_none()
        );
    }
    #[test]
    fn translation_keeps_literal_contents_and_nested_top_scopes() {
        let mut statements =
            parse_batch("SELECT TOP (2) N'[x] TOP 100' AS [text], (SELECT TOP (1) 9) AS n")
                .unwrap();
        let parameters = HashMap::new();
        let mut translator = Translator {
            parameters: &parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: 0,
            transaction_doomed: false,
            original_login: "sa",
            rowcount: 0,
            last_error: 0,
            caught_error: None,
        };
        assert!(matches!(
            VisitMut::visit(&mut statements[0], &mut translator),
            ControlFlow::Continue(())
        ));
        let sql = statements[0].to_string();
        assert!(sql.contains("'[x] TOP 100'"));
        assert!(sql.contains("SELECT 9 LIMIT __msduck_top_count("));
        assert_eq!(sql.matches("LIMIT __msduck_top_count(").count(), 2);
    }
    #[test]
    fn time_tokens_use_declared_scale_width_and_midnight_rounding() {
        for scale in 0..=7 {
            let width = match scale {
                0..=2 => 3,
                3..=4 => 4,
                _ => 5,
            };
            for nanos in [0, 45_296_123_456_700, 86_399_999_999_900] {
                let mut out = vec![];
                encode_value(
                    &mut out,
                    &Type::Time(scale),
                    &Value::Time64(TimeUnit::Nanosecond, nanos),
                )
                .unwrap();
                assert_eq!(out.len(), width + 1);
                assert_eq!(out[0] as usize, width);
                let mut bytes = [0u8; 8];
                bytes[..width].copy_from_slice(&out[1..]);
                let quantum = 10i64.pow(u32::from(7 - scale));
                assert_eq!(
                    i64::from_le_bytes(bytes),
                    ((nanos / 100 + quantum / 2) / quantum) % (864000000000 / quantum)
                );
            }
            let mut out = vec![];
            encode_value(&mut out, &Type::Time(scale), &Value::Null).unwrap();
            assert_eq!(out, [0]);
        }
        assert!(
            encode_value(
                &mut vec![],
                &Type::Time(8),
                &Value::Time64(TimeUnit::Nanosecond, 0)
            )
            .is_err()
        );
    }

    #[test]
    fn decimal_and_date_tokens_match_wire_layout() {
        let mut out = vec![];
        encode_value(
            &mut out,
            &Type::Decimal(38, 2),
            &Value::Decimal(duckdb::types::Decimal::new(38, 2, -12345).unwrap()),
        )
        .unwrap();
        assert_eq!(out[0..4], [5, 0, 0x39, 0x30]);
        assert_eq!(out.len(), 6);
        out.clear();
        encode_value(&mut out, &Type::Date, &Value::Date32(-719162)).unwrap();
        assert_eq!(out, [3, 0, 0, 0]);
    }
}

#[cfg(test)]
mod ddl_completion_tests {
    use super::*;

    #[test]
    fn ddl_batch_and_rpc_tokens_match_captured_commands_and_rowcounts() {
        // Captured twice in fresh SQL Server databases, both batch and RPC.
        let cases = [
            ("CREATE SCHEMA ddl_schema", 253, None),
            ("CREATE TABLE dbo.ddl_plain(v INT)", 198, None),
            ("CREATE TABLE dbo.ddl_identity(id INT IDENTITY)", 198, None),
            ("INSERT INTO dbo.ddl_plain VALUES(1),(2)", 195, Some(2u64)),
            (
                "CREATE VIEW dbo.ddl_view AS SELECT v FROM dbo.ddl_plain",
                207,
                None,
            ),
            (
                "ALTER VIEW dbo.ddl_view AS SELECT v+1 AS v FROM dbo.ddl_plain",
                207,
                None,
            ),
            ("DROP VIEW dbo.ddl_view", 208, None),
            ("ALTER TABLE dbo.ddl_plain ADD extra INT", 216, None),
            ("ALTER TABLE dbo.ddl_plain DROP COLUMN extra", 216, None),
            ("TRUNCATE TABLE dbo.ddl_plain", 234, None),
            ("CREATE INDEX ddl_index ON dbo.ddl_plain(v)", 200, None),
            ("DROP INDEX ddl_index ON dbo.ddl_plain", 201, None),
            ("DROP TABLE dbo.ddl_plain", 199, None),
            ("DROP TABLE dbo.ddl_identity", 199, None),
            ("DROP SCHEMA ddl_schema", 253, None),
        ];
        for rpc in [false, true] {
            let server = crate::server::Server::open(":memory:").unwrap();
            let mut session = Session::new(server.connection().unwrap()).unwrap();
            for (sql, command, count) in cases {
                let (actual, ok) = session.batch_response(sql, &Default::default(), rpc, None);
                assert!(ok, "rpc={rpc}, {sql}: {actual:?}");
                let mut expected = Vec::new();
                if !rpc || command != 253 {
                    expected.extend([
                        if rpc { 0xff } else { 0xfd },
                        u8::from(rpc) | if count.is_some() { 16 } else { 0 },
                        0,
                    ]);
                    expected.extend((command as u16).to_le_bytes());
                    expected.extend(count.unwrap_or(0).to_le_bytes());
                }
                if rpc {
                    expected.extend([0x79, 0, 0, 0, 0, 0xfe, 0, 0, 0xe0, 0]);
                    expected.extend(0u64.to_le_bytes());
                }
                assert_eq!(actual, expected, "rpc={rpc}, {sql}");
                assert_eq!(session.rowcount, count.unwrap_or(0), "{sql}");
                assert_eq!(session.last_error, 0, "{sql}");
            }
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use crate::{
        server::Server,
        tds::{BeginTransaction, TransactionRequest},
    };
    #[test]
    fn restart_commits_or_rolls_back_then_allocates_a_new_descriptor() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE TABLE items (id INT)")
            .unwrap();
        session.begin_transaction(2, "first").unwrap();
        let first = session.transaction_descriptor;
        session
            .db
            .execute_batch("INSERT INTO items VALUES (1)")
            .unwrap();
        let response = session
            .transaction_request(TransactionRequest::Commit {
                restart: Some(BeginTransaction {
                    isolation: 5,
                    name: "second".into(),
                }),
            })
            .unwrap();
        assert_eq!(response[3], 9);
        assert_eq!(response[17], 8);
        let second = session.transaction_descriptor;
        assert_ne!(first, second);
        assert_eq!(session.transactions, 1);
        session
            .db
            .execute_batch("INSERT INTO items VALUES (2)")
            .unwrap();
        session
            .transaction_request(TransactionRequest::Rollback {
                name: "second".into(),
                restart: Some(BeginTransaction {
                    isolation: 0,
                    name: String::new(),
                }),
            })
            .unwrap();
        assert_ne!(second, session.transaction_descriptor);
        assert_eq!(
            session
                .db
                .query_row("SELECT COUNT(*) FROM items", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        session.rollback_transaction("").unwrap();
        assert_eq!(session.transaction_descriptor, 0);
    }
    #[test]
    fn invalid_restart_and_unknown_names_preserve_the_active_transaction() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session.begin_transaction(0, "outer").unwrap();
        let descriptor = session.transaction_descriptor;
        assert!(
            session
                .transaction_request(TransactionRequest::Commit {
                    restart: Some(BeginTransaction {
                        isolation: 4,
                        name: String::new()
                    })
                })
                .is_err()
        );
        assert!(session.rollback_transaction("missing").is_err());
        assert!(
            session
                .transaction_request(TransactionRequest::Save {
                    name: "point".into()
                })
                .is_err()
        );
        assert_eq!(session.transaction_descriptor, descriptor);
        assert_eq!(session.transactions, 1);
        session.rollback_transaction("outer").unwrap();
    }
    #[test]
    fn disconnect_rolls_back_uncommitted_writes() {
        let server = Server::open(":memory:").unwrap();
        server
            .connection()
            .unwrap()
            .execute_batch("CREATE TABLE dbo.items (id INT)")
            .unwrap();
        {
            let mut session = Session::new(server.connection().unwrap()).unwrap();
            session.begin_transaction(0, "").unwrap();
            session
                .db
                .execute_batch("INSERT INTO items VALUES (1)")
                .unwrap();
        }
        assert_eq!(
            server
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM dbo.items", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod decimal_encoding_tests {
    use super::*;
    #[test]
    fn magnitude_selects_wire_width_and_precision_rejects_overflow() {
        for precision in [9, 10, 19, 20, 28, 29, 38] {
            let mut out = Vec::new();
            let value = Value::Decimal(duckdb::types::Decimal::new(precision, 2, -12345).unwrap());
            encode_value(&mut out, &Type::Decimal(precision, 2), &value).unwrap();
            assert_eq!(out, [5, 0, 0x39, 0x30, 0, 0]);
        }
        for (magnitude, width) in [
            (0, 5),
            (u32::MAX as i128, 5),
            (1i128 << 32, 9),
            (u64::MAX as i128, 9),
            (1i128 << 64, 13),
            ((1i128 << 96) - 1, 13),
            (1i128 << 96, 17),
            (10i128.pow(38) - 1, 17),
        ] {
            for coefficient in [magnitude, -magnitude] {
                let mut out = Vec::new();
                let value =
                    Value::Decimal(duckdb::types::Decimal::new(38, 0, coefficient).unwrap());
                encode_value(&mut out, &Type::Decimal(38, 0), &value).unwrap();
                assert_eq!(out[0], width);
                assert_eq!(out[1], u8::from(coefficient >= 0));
                assert_eq!(out.len(), width as usize + 1);
                let mut bytes = [0u8; 16];
                bytes[..out.len() - 2].copy_from_slice(&out[2..]);
                assert_eq!(u128::from_le_bytes(bytes), magnitude as u128);
            }
        }
        let value = Value::Decimal(duckdb::types::Decimal::new(5, 0, 10000).unwrap());
        assert!(encode_value(&mut vec![], &Type::Decimal(4, 0), &value).is_err());
        assert!(encode_value(&mut vec![], &Type::Decimal(5, 1), &value).is_err());
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use crate::server::Server;

    #[test]
    fn repeated_results_obey_whole_batch_response_limit() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (response, success) = session.batch_response(
            "SELECT REPEAT('x', 4500000); SELECT REPEAT('y', 4500000)",
            &HashMap::new(),
            false,
            None,
        );
        assert!(!success);
        assert!(response.len() <= tds::MAX_MESSAGE);
        let message: Vec<u8> = "batch result exceeds"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(
            response
                .windows(message.len())
                .any(|window| window == message)
        );
        assert!(
            session
                .batch_response("SELECT 1", &HashMap::new(), false, None)
                .1
        );
    }

    #[test]
    fn runaway_loop_hits_execution_limit_and_session_recovers() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (response, success) =
            session.batch_response("WHILE 1=1 BEGIN END", &HashMap::new(), false, None);
        assert!(!success);
        let message: Vec<u8> = "10000-step limit"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(
            response
                .windows(message.len())
                .any(|window| window == message)
        );
        assert!(
            session
                .batch_response("SELECT 1", &HashMap::new(), false, None)
                .1
        );
        assert_eq!(session.transactions, 0);
    }
}

#[cfg(test)]
mod fixed_scalar_encoding_tests {
    use super::*;
    #[test]
    fn fixed_scalar_rows_have_no_prefix_and_reject_null_before_writing() {
        for (kind, value, expected) in [
            (Type::Int(1), Value::UTinyInt(255), vec![255]),
            (Type::Int(2), Value::SmallInt(i16::MIN), vec![0, 128]),
            (Type::Int(4), Value::Int(i32::MIN), vec![0, 0, 0, 128]),
            (
                Type::Int(8),
                Value::BigInt(i64::MIN),
                vec![0, 0, 0, 0, 0, 0, 0, 128],
            ),
            (Type::Bit, Value::Boolean(true), vec![1]),
            (Type::Float(4), Value::Float(1.25), vec![0, 0, 160, 63]),
            (
                Type::Float(8),
                Value::Double(-2.5),
                vec![0, 0, 0, 0, 0, 0, 4, 192],
            ),
        ] {
            let mut column = Column {
                collation: None,
                name: "x".into(),
                kind,
                properties: msduck_core::result::Properties::expression(false),
            };
            let mut out = vec![];
            encode_column(&mut out, &column, &value).unwrap();
            assert_eq!(out, expected);
            out.clear();
            out.push(0xaa);
            assert!(encode_column(&mut out, &column, &Value::Null).is_err());
            assert_eq!(out, [0xaa]);
            column.properties.null_extend();
            out.clear();
            encode_column(&mut out, &column, &value).unwrap();
            assert_eq!(out[0] as usize, expected.len());
            assert_eq!(&out[1..], expected);
            out.clear();
            encode_column(&mut out, &column, &Value::Null).unwrap();
            assert_eq!(out, [0]);
        }
        for (width, value) in [(1, -1), (1, 256), (2, 32768), (4, i64::MAX), (9, 0)] {
            let mut out = vec![0xaa];
            assert!(encode_value(&mut out, &Type::Int(width), &Value::BigInt(value)).is_err());
            assert_eq!(out, [0xaa]);
        }
    }
}

#[cfg(test)]
mod unary_plus_tests {
    use super::*;

    #[test]
    fn unary_plus_preserves_native_values_and_evaluates_volatile_input_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let parameters = HashMap::new();
        for sql in [
            "N'abc'",
            "'abc'",
            "CAST(1 AS BIT)",
            "CAST(NULL AS INT)",
            "CAST('2024-01-01' AS DATE)",
            "CAST('2024-01-01' AS DATETIME2(3))",
            "CAST(12.34 AS DECIMAL(5,2))",
            "CAST(12.34 AS MONEY)",
        ] {
            let expr = Parser::new(&crate::dialect::ServerDialect)
                .try_with_sql(sql)
                .unwrap()
                .parse_expr()
                .unwrap();
            let expected = session
                .evaluate_expression(expr.clone(), &parameters, false)
                .unwrap();
            let actual = session
                .evaluate_expression(
                    Expr::UnaryOp {
                        op: UnaryOperator::Plus,
                        expr: Box::new(expr),
                    },
                    &parameters,
                    false,
                )
                .unwrap();
            assert_eq!(actual, expected, "{sql}");
        }
        for source in ["(VALUES(1),(2))", "(VALUES(1),(CAST(NULL AS INT)))"] {
            let plain = session.batch(
                &format!("SELECT n AS p FROM {source} s(n) ORDER BY n"),
                &parameters,
                false,
            );
            let plus = session.batch(
                &format!("SELECT +n AS p FROM {source} s(n) ORDER BY n"),
                &parameters,
                false,
            );
            assert_eq!(plain, plus, "{source}");
        }
        session
            .db
            .execute_batch("CREATE SEQUENCE unary_plus_sequence")
            .unwrap();
        let expr = Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql("+nextval('unary_plus_sequence')")
            .unwrap()
            .parse_expr()
            .unwrap();
        assert_eq!(
            session
                .evaluate_expression(expr, &parameters, false)
                .unwrap(),
            Value::BigInt(1)
        );
        let current: i64 = session
            .db
            .query_row("SELECT currval('unary_plus_sequence')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(current, 1);
    }
}

#[cfg(test)]
mod unary_bit_tests {
    use super::*;

    #[test]
    fn bit_negation_preflight_prevents_writes_and_binding_preserves_diagnostic() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE TABLE unary_guard(n INTEGER)")
            .unwrap();
        let parameters = HashMap::new();
        let mut expected = Vec::new();
        tds::sql_error(
            &mut expected,
            &SqlError::new(8117, 1, msduck_sql::unary_operator::BIT_MINUS),
        );
        tds::done(&mut expected, 0xfd, 2, 0, 0);
        for sql in [
            "INSERT INTO unary_guard VALUES(1); SELECT -CAST(1 AS BIT)",
            "IF 1=0 SELECT -CAST(1 AS BIT)",
            "BEGIN TRY SELECT -CAST(1 AS BIT) END TRY BEGIN CATCH SELECT 99 END CATCH",
            "SELECT -b FROM (VALUES(CAST(1 AS BIT))) s(b)",
            "BEGIN TRY SELECT -b FROM (VALUES(CAST(1 AS BIT))) s(b) END TRY BEGIN CATCH SELECT 99 END CATCH",
        ] {
            let (tokens, success) = session.batch_response(sql, &parameters, false, None);
            assert!(!success, "{sql}");
            assert_eq!(tokens, expected, "{sql}");
        }
        let written: i64 = session
            .db
            .query_row("SELECT COUNT(*) FROM unary_guard", [], |row| row.get(0))
            .unwrap();
        assert_eq!(written, 0);
        let error = session
            .validate_prepared_sql("SELECT -@b", &[("@b".into(), SqlType::Bit)])
            .unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8117);
    }
}

#[cfg(test)]
mod output_materialization_tests {
    use super::*;
    #[test]
    fn paired_output_prepares_without_writes_and_keeps_old_new_images() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut parameters = HashMap::new();
        for statement in parse_batch("CREATE TABLE paired_source(id INT PRIMARY KEY,n INT); INSERT INTO paired_source VALUES(10,1),(20,2); CREATE TABLE paired_sink(old_id INT,new_id INT)").unwrap() {
            session.execute(statement, &mut parameters).unwrap();
        }
        let sql = "UPDATE paired_source SET id=id+100,n=n+10 OUTPUT deleted.id,inserted.id,deleted.n,inserted.n";
        session.validate_prepared_sql(sql, &[]).unwrap();
        let values = |session: &Session| {
            session
                .db
                .prepare("SELECT id,n FROM paired_source ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i32>(1)?)))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(values(&session), vec![(10, 1), (20, 2)]);
        let result = session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(result.count, Some(2));
        assert_eq!(values(&session), vec![(110, 11), (120, 12)]);
        let sql =
            "UPDATE paired_source SET id=id+100 OUTPUT deleted.id,inserted.id INTO paired_sink";
        session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT COUNT(*) FROM paired_sink WHERE new_id=old_id+100",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            2
        );
        let before_failure = values(&session);
        session
            .execute(
                parse_batch("CREATE TABLE paired_guard(old_id INT,new_n INT NOT NULL)")
                    .unwrap()
                    .remove(0),
                &mut parameters,
            )
            .unwrap();
        for sql in [
            "UPDATE paired_source SET id=1 OUTPUT deleted.id,inserted.id",
            "UPDATE paired_source SET n=NULL OUTPUT deleted.id,inserted.n INTO paired_guard",
        ] {
            assert!(
                session
                    .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
                    .is_err()
            );
            assert_eq!(values(&session), before_failure);
            assert_eq!(
                session
                    .db
                    .query_row("SELECT COUNT(*) FROM paired_guard", [], |r| r
                        .get::<_, i32>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(session.db.query_row("SELECT COUNT(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'", [], |r| r.get::<_,i64>(0)).unwrap(),0);
    }

    #[test]
    fn parameterized_output_binds_without_writes_and_cleans_images_on_every_outcome() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut parameters = HashMap::from([(
            "@p".into(),
            Parameter {
                data_type: SqlType::Int,
                value: ParameterValue::Int(7),
            },
        )]);
        for statement in parse_batch("CREATE TABLE image_source(id INT PRIMARY KEY); CREATE TABLE image_sink(id INT,n INT NOT NULL)").unwrap() {
            session.execute(statement, &mut parameters).unwrap();
        }
        let sql = "INSERT INTO image_source(id) OUTPUT inserted.id,@p INTO image_sink(id,n) VALUES(1),(2)";
        session
            .validate_prepared_sql(sql, &[("@p".into(), SqlType::Int)])
            .unwrap();
        let count = |session: &Session, table: &str| {
            session
                .db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
        };
        let images = |session: &Session| {
            session.db.query_row("SELECT COUNT(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'", [], |r| r.get::<_,i64>(0)).unwrap()
        };
        assert_eq!(count(&session, "image_source"), 0);
        assert_eq!(images(&session), 0);
        let result = session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(result.count, Some(2));
        assert_eq!(result.command, 0xc3);
        assert!(result.tokens.is_empty());
        assert_eq!(
            session
                .db
                .query_row("SELECT CAST(SUM(n) AS BIGINT) FROM image_sink", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            14
        );
        parameters.get_mut("@p").unwrap().value = ParameterValue::Int(9);
        session.execute(parse_batch("INSERT INTO image_source(id) OUTPUT inserted.id,@p INTO image_sink(id,n) VALUES(3)").unwrap().remove(0), &mut parameters).unwrap();
        assert_eq!(
            session
                .db
                .query_row("SELECT n FROM image_sink WHERE id=3", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            9
        );
        assert_eq!(images(&session), 0);
        assert!(
            session
                .validate_prepared_sql(
                    "INSERT INTO image_source(id) OUTPUT SUM(inserted.id)+@p VALUES(4)",
                    &[("@p".into(), SqlType::Int)]
                )
                .is_err()
        );
        let empty = session
            .execute(
                parse_batch(
                    "INSERT INTO image_source(id) OUTPUT inserted.id,@p AS p SELECT 4 WHERE 1=0",
                )
                .unwrap()
                .remove(0),
                &mut parameters,
            )
            .unwrap();
        assert_eq!(empty.count, Some(0));
        assert_eq!(empty.tokens.first(), Some(&0x81));
        assert!(
            session
                .execute(
                    parse_batch(
                        "INSERT INTO image_source(id) OUTPUT inserted.id,@p AS p VALUES(1)"
                    )
                    .unwrap()
                    .remove(0),
                    &mut parameters
                )
                .is_err()
        );
        parameters.get_mut("@p").unwrap().value = ParameterValue::Null;
        let error = session.execute(parse_batch("INSERT INTO image_source(id) OUTPUT inserted.id,@p INTO image_sink(id,n) VALUES(4)").unwrap().remove(0), &mut parameters).err().unwrap();
        assert_eq!(error_number(&error.to_string()), 515);
        assert_eq!(count(&session, "image_source"), 3);
        assert_eq!(count(&session, "image_sink"), 3);
        assert_eq!(images(&session), 0);
    }
}

#[cfg(test)]
mod index_catalog_integration_tests {
    use super::*;

    #[test]
    fn index_catalog_startup_and_empty_result_descriptors_match_reference() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/index-catalog.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for view in ["indexes", "index_columns"] {
            let wire = &fixture["declarations"][view]["wire"]["sets"][0]["columns"];
            let columns = wire
                .as_array()
                .unwrap()
                .iter()
                .map(|c| {
                    let kind = match c["type"].as_str().unwrap() {
                        "Int" => Type::Int(4),
                        "TinyInt" => Type::Int(1),
                        "IntN" => Type::Int(c["length"].as_u64().unwrap() as u8),
                        "BitN" => Type::Bit,
                        "NVarChar" => {
                            if c["length"] == 65535 {
                                Type::Text
                            } else {
                                Type::Nvarchar(c["length"].as_u64().unwrap() as u16 / 2)
                            }
                        }
                        other => panic!("unhandled reference type {other}"),
                    };
                    let flags = c["flags"].as_u64().unwrap();
                    let collation = c["collation"].as_object().map(|_| {
                        tds::collation::Collation::new(
                            c["collation"]["lcid"].as_u64().unwrap() as u32,
                            c["collation"]["flags"].as_u64().unwrap() as u8,
                            c["collation"]["version"].as_u64().unwrap() as u8,
                            c["collation"]["sortId"].as_u64().unwrap() as u8,
                        )
                        .unwrap()
                    });
                    Column {
                        name: c["name"].as_str().unwrap().into(),
                        kind,
                        properties: msduck_core::result::Properties {
                            nullable: Some(flags & 1 != 0),
                            origin: if flags & 32 != 0 {
                                msduck_core::result::Origin::Expression
                            } else {
                                msduck_core::result::Origin::Stored
                            },
                        },
                        collation,
                    }
                })
                .collect::<Vec<_>>();
            let mut expected = vec![];
            tds::metadata(&mut expected, &columns).unwrap();
            for name in [format!("sys.{view}"), format!("[sys].[{view}]")] {
                let sql = format!("SELECT * FROM {name} WHERE 1=0");
                let (actual, ok) = session.batch_response(&sql, &Default::default(), false, None);
                assert!(ok, "{sql}: {actual:?}");
                assert!(
                    actual.starts_with(&expected),
                    "{sql}: expected {expected:?}, actual {actual:?}"
                );
            }
        }
    }

    #[test]
    fn index_creation_uses_table_owned_identity_and_transactional_catalog() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "CREATE TABLE dbo.a(id INT)",
            "CREATE TABLE dbo.b(id INT)",
            "CREATE INDEX ix ON dbo.a(id)",
            "CREATE UNIQUE INDEX ix ON dbo.b(id)",
        ] {
            let (out, ok) = session.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {out:?}");
        }
        let indexes = crate::index_catalog::acquire_complete(&session.db).unwrap();
        assert_eq!(indexes.len(), 2);
        assert!(indexes.iter().all(|i| i.name == "ix" && i.index_id == 2));
        assert_ne!(indexes[0].backend_name, indexes[1].backend_name);
        for sql in [
            "BEGIN TRAN; CREATE INDEX transient ON dbo.a(id); ROLLBACK",
            "BEGIN TRAN; DROP TABLE dbo.a; ROLLBACK",
        ] {
            assert!(
                session
                    .batch_response(sql, &Default::default(), false, None)
                    .1,
                "{sql}"
            );
            assert_eq!(
                crate::index_catalog::acquire_complete(&session.db).unwrap(),
                indexes
            );
        }
        assert!(
            session
                .batch_response("DROP TABLE dbo.a", &Default::default(), false, None)
                .1
        );
        assert_eq!(
            crate::index_catalog::acquire_complete(&session.db)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM main.__msduck_index_catalog",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
}

#[cfg(test)]
mod drop_index_runtime_tests {
    use super::*;
    #[test]
    fn captured_drop_requests_preserve_diagnostics_completion_and_remaining_indexes() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/drop-index.json")).unwrap();
        for rpc in [false, true] {
            for case in fixture["results"].as_array().unwrap() {
                let server = crate::server::Server::open(":memory:").unwrap();
                let mut session = Session::new(server.connection().unwrap()).unwrap();
                for sql in fixture["setup"].as_array().unwrap() {
                    let (out, ok) = session.batch_response(
                        sql.as_str().unwrap(),
                        &Default::default(),
                        false,
                        None,
                    );
                    assert!(ok, "setup {sql}: {out:?}");
                }
                let sql = case["sql"].as_str().unwrap();
                let (actual, ok) = session.batch_response(sql, &Default::default(), rpc, None);
                let errors = case["result"]["errors"].as_array().unwrap();
                assert_eq!(ok, errors.is_empty(), "{sql}: {actual:?}");
                let mut expected = vec![];
                for e in errors {
                    tds::sql_error(
                        &mut expected,
                        &SqlError::from_utf16(
                            e["number"].as_i64().unwrap() as i32,
                            e["state"].as_u64().unwrap() as u8,
                            e["class"].as_u64().unwrap() as u8,
                            e["message"].as_str().unwrap().encode_utf16().collect(),
                        ),
                    );
                }
                let number = errors.first().map(|e| e["number"].as_i64().unwrap() as i32);
                if rpc {
                    // SQL Server RPC captures distinguish statement errors from
                    // syntax/option failures that terminate the entire request.
                    if number.is_none() || number == Some(3701) {
                        tds::done(
                            &mut expected,
                            0xff,
                            if number.is_some() { 3 } else { 1 },
                            201,
                            0,
                        );
                    }
                    if number != Some(3748) {
                        expected.push(0x79);
                        expected.extend(number.unwrap_or(0).to_le_bytes());
                    }
                    tds::done(
                        &mut expected,
                        0xfe,
                        if matches!(number, Some(156 | 159 | 3748)) {
                            2
                        } else {
                            0
                        },
                        224,
                        0,
                    );
                } else {
                    let done = &case["completion"][0];
                    tds::done(
                        &mut expected,
                        0xfd,
                        if errors.is_empty() { 0 } else { 2 },
                        done["curCmd"].as_u64().unwrap() as u16,
                        0,
                    );
                }
                assert_eq!(actual, expected, "{sql}");
                assert_eq!(
                    serde_json::json!([[session.rowcount, session.last_error]]),
                    case["state"]["sets"][0]["rows"],
                    "{sql}"
                );
                let rows = session
                    .db
                    // Direct DuckDB observation needs explicit trailing-space
                    // equality; public T-SQL predicates are covered over TDS.
                    .prepare(
                        &fixture["inventory"]
                            .as_str()
                            .unwrap()
                            .replace("o.type='U'", "rtrim(o.type)='U'"),
                    )
                    .unwrap()
                    .query_map([], |r| {
                        Ok(vec![
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ])
                    })
                    .unwrap()
                    .collect::<duckdb::Result<Vec<_>>>()
                    .unwrap();
                assert_eq!(
                    serde_json::json!(rows),
                    case["remaining"]["sets"][0]["rows"],
                    "{sql}"
                );
            }
        }
    }
    #[test]
    fn caller_transaction_can_rollback_a_drop_before_a_later_missing_target() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    "CREATE TABLE dbo.a(id INT); CREATE INDEX ix ON dbo.a(id); BEGIN TRAN",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        assert!(
            !session
                .batch_response(
                    "DROP INDEX ix ON dbo.a, absent ON dbo.a",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        assert_eq!(session.transactions, 1);
        assert!(
            crate::index_catalog::acquire_complete(&session.db)
                .unwrap()
                .is_empty()
        );
        assert!(
            session
                .batch_response("ROLLBACK", &Default::default(), false, None)
                .1
        );
        assert_eq!(
            crate::index_catalog::acquire_complete(&session.db)
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(test)]
mod duplicate_index_runtime_tests {
    use super::*;
    #[test]
    fn duplicate_index_errors_match_captured_tokens_and_preserve_the_session() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/index-catalog.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "CREATE TABLE dbo.a(id INT,value INT)",
            "CREATE INDEX ix ON dbo.a(id)",
            "CREATE SCHEMA alt",
            "CREATE TABLE alt.[odd.table]([odd.column] INT,other INT)",
            "CREATE INDEX [odd.index] ON alt.[odd.table]([odd.column])",
        ] {
            let (out, ok) = session.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {out:?}");
        }
        let before = crate::index_catalog::acquire_complete(&session.db).unwrap();
        let mut checked = 0;
        for case in fixture["results"].as_array().unwrap() {
            if !case["id"].as_str().unwrap().starts_with("duplicate-") {
                continue;
            }
            let error = &case["result"]["errors"][0];
            let mut expected = vec![];
            tds::sql_error(
                &mut expected,
                &SqlError::from_utf16(
                    error["number"].as_i64().unwrap() as i32,
                    error["state"].as_u64().unwrap() as u8,
                    error["class"].as_u64().unwrap() as u8,
                    error["message"].as_str().unwrap().encode_utf16().collect(),
                ),
            );
            tds::done(
                &mut expected,
                0xfd,
                2,
                case["completion"][0]["curCmd"].as_u64().unwrap() as u16,
                0,
            );
            let (actual, ok) = session.batch_response(
                case["sql"].as_str().unwrap(),
                &Default::default(),
                false,
                None,
            );
            assert!(!ok);
            assert_eq!(actual, expected, "{}", case["id"]);
            assert_eq!(
                serde_json::json!([[session.rowcount, session.last_error, session.transactions]]),
                case["state"]["sets"][0]["rows"]
            );
            assert_eq!(
                crate::index_catalog::acquire_complete(&session.db).unwrap(),
                before
            );
            checked += 1;
        }
        assert_eq!(checked, 4);
        assert!(
            session
                .batch_response(
                    "CREATE INDEX usable ON dbo.a(value)",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
    }
}
