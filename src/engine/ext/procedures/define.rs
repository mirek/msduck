//! CREATE, ALTER, CREATE OR ALTER and DROP PROCEDURE over the module store.
//!
//! The stored definition is the whole batch text, as SQL Server keeps it in
//! `sys.sql_modules`. `properties` records the parameters as
//! `{"parameters":[{"name":"@x","type":"int","output":false,"default":null}]}`
//! (`default` is the default's source text, such as `"5"` or `"NULL"`).
use super::super::modules::{self, kind};
use crate::engine::{Execution, Session, StatementErrors};
use crate::tds;
use anyhow::Result;
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::procedures::Definition;
use sqlparser::ast::{FunctionDesc, ObjectNamePart};

/// Completion command of CREATE and ALTER PROCEDURE.
const CREATE: u16 = 222;
/// Completion command of DROP PROCEDURE.
const DROP: u16 = 223;

pub(super) fn properties(definition: &Definition) -> String {
    let parameters: Vec<serde_json::Value> = definition
        .parameters
        .iter()
        .map(|parameter| {
            serde_json::json!({
                "name": parameter.name,
                "type": parameter.data_type.to_string().to_lowercase(),
                "output": parameter.output,
                "default": parameter.default.as_ref().map(|(_, text)| text.as_str()),
            })
        })
        .collect();
    serde_json::json!({ "parameters": parameters }).to_string()
}

/// The procedure's body statements, checked as SQL Server compiles them at
/// CREATE: syntax, then declarations and variable references (137, 134)
/// with the parameters in scope.
pub(super) fn compile(
    sql: &str,
    definition: &Definition,
) -> Result<(
    Vec<sqlparser::ast::Statement>,
    std::collections::HashMap<String, crate::engine::Parameter>,
)> {
    let body = &sql[definition.body..];
    // A nested CREATE PROCEDURE is a syntax error inside a module body.
    let statements = crate::engine::parse_batch(body).map_err(|error| {
        match error.downcast_ref::<SqlError>() {
            Some(error) if error.number == 111 => nested_create().into(),
            _ => syntax_failure(error),
        }
    })?;
    if statements.is_empty() {
        anyhow::bail!(SqlError::syntax(102, 1, "Incorrect syntax near 'AS'."));
    }
    if statements.iter().any(uses_database) {
        anyhow::bail!(SqlError::syntax(
            154,
            1,
            "a USE database statement is not allowed in a procedure, function or trigger."
        ));
    }
    let mut parameters = std::collections::HashMap::new();
    for parameter in &definition.parameters {
        let data_type = msduck_sql::batch::variable_type(parameter.data_type.clone())?;
        parameters.insert(
            parameter.name.to_lowercase(),
            crate::engine::Parameter {
                value: msduck_core::value::Value::Null,
                data_type,
            },
        );
    }
    let variables = preflight(&statements, &parameters)?;
    Ok((statements, variables))
}

/// Batch-scope declaration checks for a module body, with SQL Server's
/// diagnostics for undeclared (137) and redeclared (134) variables.
pub(super) fn preflight(
    statements: &[sqlparser::ast::Statement],
    parameters: &std::collections::HashMap<String, crate::engine::Parameter>,
) -> Result<std::collections::HashMap<String, crate::engine::Parameter>> {
    msduck_sql::preflight::variables(statements, parameters).map_err(|error| {
        let message = error.to_string();
        if let Some(name) = message.strip_prefix("Must declare the scalar variable ") {
            return SqlError::syntax(
                137,
                2,
                format!("Must declare the scalar variable \"{name}\"."),
            )
            .into();
        }
        if let Some(name) = message
            .strip_prefix("The variable name ")
            .and_then(|rest| rest.strip_suffix(" has already been declared"))
        {
            return SqlError::syntax(
                134,
                1,
                format!(
                    "The variable name '{name}' has already been declared. Variable names must be unique within a query batch or stored procedure."
                ),
            )
            .into();
        }
        error
    })
}

/// CREATE PROCEDURE where it cannot begin the batch, as SQL Server reports
/// it inside a module body or after sp_executesql's declarations.
pub(super) fn nested_create() -> SqlError {
    SqlError::syntax(156, 1, "Incorrect syntax near the keyword 'PROCEDURE'.")
}

fn uses_database(statement: &sqlparser::ast::Statement) -> bool {
    use sqlparser::ast::{Statement, Visit, Visitor};
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_statement(&mut self, statement: &Statement) -> std::ops::ControlFlow<()> {
            if matches!(statement, Statement::Use(_)) {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    statement.visit(&mut Find).is_break()
}

/// Parse failures carry SQL Server numbers like the batch parser's.
pub(super) fn syntax_failure(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<SqlError>().is_some() {
        return error;
    }
    let message = error.to_string();
    let number = crate::merge::error_number(&message)
        .or_else(|| msduck_sql::query_options::error_number(&message))
        .unwrap_or(102);
    let mut tokens = Vec::new();
    tds::error(&mut tokens, number, &message);
    super::decode_error(&tokens)
        .unwrap_or_else(|| SqlError::syntax(number, 1, message))
        .into()
}

fn respond(session: &mut Session, rpc: bool, result: Result<()>) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    match result {
        Ok(()) => {
            session.last_error = 0;
            if rpc {
                if !session.nocount {
                    tds::done(&mut out, 0xff, 1, CREATE, 0);
                }
                out.push(0x79);
                out.extend(0i32.to_le_bytes());
                tds::done(&mut out, 0xfe, 0, 0xe0, 0);
            } else {
                tds::done(&mut out, 0xfd, 0, CREATE, 0);
            }
            (out, true)
        }
        Err(error) => {
            session.last_error = crate::engine::emit_error(&mut out, &error);
            // Captured: name conflicts complete as CREATE, compilation
            // failures as an aborted batch.
            let command = if matches!(session.last_error, 2714 | 2010) {
                CREATE
            } else {
                253
            };
            if rpc {
                // Captured for sp_executesql: the error number is the status.
                out.push(0x79);
                out.extend(session.last_error.to_le_bytes());
                tds::done(&mut out, 0xfe, 2, 0xe0, 0);
            } else {
                tds::done(&mut out, 0xfd, 2, command, 0);
            }
            (out, false)
        }
    }
}

pub(super) fn create(
    session: &mut Session,
    sql: &str,
    definition: Result<Definition, SqlError>,
    rpc: bool,
) -> (Vec<u8>, bool) {
    let result = define(session, sql, definition);
    respond(session, rpc, result)
}

/// Store a CREATE, ALTER or CREATE OR ALTER PROCEDURE batch.
pub(super) fn define(
    session: &mut Session,
    sql: &str,
    definition: Result<Definition, SqlError>,
) -> Result<()> {
    {
        let definition = definition?;
        compile(sql, &definition)?;
        let properties = properties(&definition);
        let db = &session.db;
        let schema = definition.schema.as_deref();
        let name = definition.name.as_str();
        let existing = modules::find(db, schema, name)?;
        match existing {
            Some(module) if module.type_code == kind::PROCEDURE && definition.alter => {
                modules::alter(db, module.object_id, sql, &properties)?;
            }
            Some(_) if !definition.alter => {
                anyhow::bail!(SqlError::new(
                    2714,
                    3,
                    format!("There is already an object named '{name}' in the database.")
                ));
            }
            None if !definition.create => {
                if object_type(session, schema, name)?.is_some() {
                    anyhow::bail!(incompatible(name));
                }
                anyhow::bail!(SqlError::new(
                    208,
                    6,
                    format!("Invalid object name '{name}'.")
                ));
            }
            None => {
                if definition.alter && object_type(session, schema, name)?.is_some() {
                    anyhow::bail!(incompatible(name));
                }
                modules::create(db, schema, name, kind::PROCEDURE, 0, sql, &properties).map_err(
                    |error| match error.downcast::<SqlError>() {
                        Ok(mut error) if error.number == 2714 => {
                            error.state = 3;
                            error.into()
                        }
                        Ok(error) => error.into(),
                        Err(error) => error,
                    },
                )?;
            }
            Some(_) => anyhow::bail!(incompatible(name)),
        }
        Ok(())
    }
}

fn incompatible(name: &str) -> SqlError {
    SqlError::new(
        2010,
        1,
        format!("Cannot perform alter on '{name}' because it is an incompatible object type."),
    )
}

/// The `sys.objects` type of a schema object, if one has that name.
fn object_type(session: &Session, schema: Option<&str>, name: &str) -> Result<Option<String>> {
    use duckdb::OptionalExt;
    let schema_id = modules::schema_id(&session.db, schema)?;
    Ok(session
        .db
        .query_row(
            "SELECT rtrim(type) FROM sys.objects WHERE schema_id = ? AND lower(name) = lower(?) AND parent_object_id = 0",
            duckdb::params![schema_id, name],
            |row| row.get(0),
        )
        .optional()?)
}

/// Split a (possibly database-qualified) procedure name. A database part
/// must name the current database.
pub(super) fn split(
    session: &Session,
    parts: &[String],
) -> Result<Option<(Option<String>, String)>> {
    Ok(match parts {
        [name] => Some((None, name.clone())),
        [schema, name] => Some((Some(schema.clone()), name.clone())),
        [database, schema, name] => {
            if !database.eq_ignore_ascii_case(&session.database().name) {
                anyhow::bail!("unsupported procedure in another database: {database}");
            }
            Some(((!schema.is_empty()).then(|| schema.clone()), name.clone()))
        }
        _ => None,
    })
}

pub(super) fn identifiers(name: &sqlparser::ast::ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => Some(ident.value.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn drop(
    session: &mut Session,
    if_exists: bool,
    names: &[FunctionDesc],
) -> Result<Execution> {
    let mut errors = Vec::new();
    for desc in names {
        let parts = identifiers(&desc.name)
            .ok_or_else(|| anyhow::anyhow!("unsupported procedure name {}", desc.name))?;
        let Some((schema, name)) = split(session, &parts)? else {
            anyhow::bail!("unsupported procedure name {}", desc.name);
        };
        let found = modules::find(&session.db, schema.as_deref(), &name)?;
        match found {
            Some(module) if module.type_code == kind::PROCEDURE => {
                modules::remove(&session.db, module.object_id)?;
            }
            _ if if_exists => {}
            _ => {
                let kind = object_type(session, schema.as_deref(), &name)?;
                let object = match kind.as_deref() {
                    Some("U") => Some(("a table", "TABLE")),
                    Some("V") => Some(("a view", "VIEW")),
                    Some("FN" | "IF" | "TF") => Some(("a function", "FUNCTION")),
                    Some("TR") => Some(("a trigger", "TRIGGER")),
                    _ => None,
                };
                errors.push(match object {
                    Some((what, statement)) => SqlError::new(
                        3705,
                        1,
                        format!(
                            "Cannot use DROP PROCEDURE with '{name}' because '{name}' is {what}. Use DROP {statement}."
                        ),
                    ),
                    None => {
                        let mut error = SqlError::new(
                            3701,
                            5,
                            format!(
                                "Cannot drop the procedure '{name}', because it does not exist or you do not have permission."
                            ),
                        );
                        error.severity = 11;
                        error
                    }
                });
            }
        }
    }
    if !errors.is_empty() {
        return Err(StatementErrors(errors).into());
    }
    Ok(Execution::statement(vec![], None, DROP))
}

/// Whether a batch begins with the name of an existing procedure, which
/// SQL Server runs as `EXEC name ...` when it is the first statement.
pub(super) fn bare_call(session: &Session, sql: &str) -> bool {
    use sqlparser::tokenizer::Token;
    let Ok(tokens) = msduck_sql::dialect::tokenize(sql) else {
        return false;
    };
    let tokens: Vec<_> = tokens
        .into_iter()
        .filter(|token| !matches!(token.token, Token::Whitespace(_)))
        .collect();
    match tokens.first().map(|token| &token.token) {
        Some(Token::Word(first))
            if !first.value.starts_with(['@', '#'])
                && (first.quote_style.is_some()
                    || !msduck_sql::dialect::ext::procedures::starts_statement(&first.value)) => {}
        _ => return false,
    }
    let dialect = msduck_sql::dialect::ServerDialect;
    let mut parser = sqlparser::parser::Parser::new(&dialect).with_tokens_with_locations(tokens);
    let Ok(name) = parser.parse_object_name(false) else {
        return false;
    };
    if matches!(
        parser.peek_token().token,
        Token::Colon | Token::Eq | Token::LParen | Token::Period
    ) {
        return false;
    }
    let Some(parts) = identifiers(&name) else {
        return false;
    };
    let Ok(Some((schema, name))) = split(session, &parts) else {
        return false;
    };
    matches!(
        modules::find(&session.db, schema.as_deref(), &name),
        Ok(Some(module)) if module.type_code == kind::PROCEDURE
    )
}
