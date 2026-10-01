//! User-defined scalar, inline and multi-statement table-valued functions.
//!
//! Definitions live in the module store (`FN`, `IF`, `TF`). A call is folded
//! into the calling statement: a scalar call becomes the expression its body
//! computes and a table-valued call a derived table, so functions work over
//! many rows, in joins and in APPLY with the engine's ordinary typing and
//! metadata. See docs/gaps-functions.md.
use super::{Feature, modules};
use crate::engine::{Execution, Parameter, Session, emit_error};
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::functions::{
    Action, Definition, Returns, definition, misplaced_definition, validate,
};
use sqlparser::ast::{Expr, ObjectName, Statement};
use std::collections::HashMap;

mod expand;
mod run;

/// Per-session state: a counter that keeps the aliases of derived argument
/// tables unique within a statement.
#[derive(Default)]
pub(crate) struct State {
    aliases: std::cell::Cell<u64>,
    /// Nesting level of interpreted calls that are running.
    depth: std::cell::Cell<usize>,
}

impl State {
    fn alias(&self) -> String {
        let next = self.aliases.get() + 1;
        self.aliases.set(next);
        format!("__msduck_fn_arguments_{next}")
    }
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "functions"
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        // Cheap filter before tokenizing: every definition names FUNCTION.
        if !contains_ignore_case(sql, "function") {
            return None;
        }
        if let Some(action) = Action::of(sql) {
            let result = define(session, sql, action);
            return Some(respond(session, rpc, result));
        }
        let statement = misplaced_definition(sql)?;
        let error = SqlError::syntax(
            111,
            1,
            format!("'{statement}' must be the first statement in a query batch."),
        );
        Some(respond_with(session, rpc, Err(error.into()), 253))
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        _parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        let Statement::DropFunction(drop) = statement else {
            return Ok(None);
        };
        session.require_committable()?;
        for function in &drop.func_desc {
            let (schema, name) = split(&function.name)?;
            let found = modules::schema_id(&session.db, schema.as_deref())
                .ok()
                .map(|_| modules::find(&session.db, schema.as_deref(), &name))
                .transpose()?
                .flatten()
                .filter(|module| is_function(&module.type_code));
            match found {
                Some(module) => {
                    modules::remove(&session.db, module.object_id)?;
                    expand::forget(&module.definition);
                }
                None if drop.if_exists => {}
                None => bail!(SqlError::new(
                    3701,
                    5,
                    format!(
                        "Cannot drop the function '{}', because it does not exist or you do not have permission.",
                        function.name
                    )
                )),
            }
        }
        Ok(Some(Execution::statement(vec![], None, 179)))
    }

    fn rewrite_statement(
        &self,
        session: &Session,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        expand::statement(session, statement, parameters)
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        expand::expression(session, expr, parameters)
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

pub(super) fn is_function(type_code: &str) -> bool {
    matches!(type_code, "FN" | "IF" | "TF")
}

/// Schema (when given) and name of a one- or two-part name.
fn split(name: &ObjectName) -> Result<(Option<String>, String)> {
    let parts: Vec<String> = name
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|ident| ident.value.clone())
                .ok_or_else(|| anyhow::anyhow!("unsupported function name {name}"))
        })
        .collect::<Result<_>>()?;
    match parts.as_slice() {
        [name] => Ok((None, name.clone())),
        [schema, name] => Ok((Some(schema.clone()), name.clone())),
        _ => bail!("unsupported function name {name}"),
    }
}

/// CREATE, ALTER or CREATE OR ALTER FUNCTION.
fn define(session: &mut Session, sql: &str, action: Action) -> Result<()> {
    let definition = definition::parse(sql)?;
    validate::validate(&definition)?;
    session.require_committable()?;
    let schema = definition.schema.as_deref();
    let display = match schema {
        Some(schema) => format!("{schema}.{}", definition.name),
        None => definition.name.clone(),
    };
    modules::schema_id(&session.db, schema)?;
    let existing = modules::find(&session.db, schema, &definition.name)?;
    let properties = properties(&definition).to_string();
    let text = sql.trim();
    match (action, existing) {
        (Action::Create, _) | (Action::CreateOrAlter, None) => {
            if let Some(object) = other_object(session, schema, &definition.name)? {
                let _ = object;
                bail!(SqlError::new(
                    2714,
                    3,
                    format!(
                        "There is already an object named '{}' in the database.",
                        definition.name
                    )
                ));
            }
            let created = modules::create(
                &session.db,
                schema,
                &definition.name,
                definition.type_code(),
                0,
                text,
                &properties,
            );
            if let Err(error) = created {
                // modules::create reports duplicates with state 6.
                if let Some(error) = error.downcast_ref::<SqlError>()
                    && error.number == 2714
                {
                    bail!(SqlError::new(2714, 3, error.message.clone()));
                }
                return Err(error);
            }
        }
        (Action::Alter, None) => {
            if other_object(session, schema, &definition.name)?.is_some() {
                bail!(incompatible(&display));
            }
            bail!(SqlError::new(
                208,
                6,
                format!("Invalid object name '{display}'.")
            ));
        }
        (Action::Alter | Action::CreateOrAlter, Some(module)) => {
            if !compatible(&module.type_code, definition.type_code()) {
                bail!(incompatible(&display));
            }
            expand::forget(&module.definition);
            modules::alter(&session.db, module.object_id, text, &properties)?;
            if module.type_code != definition.type_code() {
                session.db.execute(
                    "UPDATE main.__msduck_modules SET type_code = ? WHERE object_id = ?",
                    duckdb::params![definition.type_code(), module.object_id],
                )?;
            }
        }
    }
    Ok(())
}

fn incompatible(display: &str) -> SqlError {
    SqlError::new(
        2010,
        1,
        format!("Cannot perform alter on '{display}' because it is an incompatible object type."),
    )
}

/// Scalar functions alter only to scalar functions; table-valued functions
/// alter to either table-valued kind.
fn compatible(existing: &str, new: &str) -> bool {
    match existing {
        "FN" => new == "FN",
        "IF" | "TF" => matches!(new, "IF" | "TF"),
        _ => false,
    }
}

/// A table, view or other non-module object with this name.
fn other_object(session: &Session, schema: Option<&str>, name: &str) -> Result<Option<i32>> {
    let schema_id = modules::schema_id(&session.db, schema)?;
    let id: Option<i32> = duckdb::OptionalExt::optional(session.db.query_row(
        "SELECT object_id FROM sys.objects WHERE schema_id = ? AND lower(name) = lower(?) LIMIT 1",
        duckdb::params![schema_id, name],
        |row| row.get(0),
    ))?;
    Ok(id)
}

fn type_name(data_type: &sqlparser::ast::DataType) -> String {
    data_type.to_string().to_lowercase()
}

/// Feature-owned properties: parameters and return type in the format shared
/// with procedures, plus the WITH options.
fn properties(definition: &Definition) -> serde_json::Value {
    use serde_json::json;
    let parameters: Vec<_> = definition
        .parameters
        .iter()
        .map(|parameter| {
            json!({
                "name": parameter.name,
                "type": type_name(&parameter.data_type),
                "output": false,
                "default": parameter.default.as_ref().map(|value| value.to_string()),
                "readonly": parameter.readonly,
            })
        })
        .collect();
    let returns = match &definition.returns {
        Returns::Scalar(data_type) => json!(type_name(data_type)),
        Returns::Inline => json!("TABLE"),
        Returns::Table { variable, columns } => json!({
            "variable": variable,
            "columns": columns.iter().map(|column| json!({
                "name": column.name.value,
                "type": type_name(&column.data_type),
                "nullable": !column.options.iter().any(|option| matches!(
                    option.option,
                    sqlparser::ast::ColumnOption::NotNull
                        | sqlparser::ast::ColumnOption::PrimaryKey(_)
                )),
            })).collect::<Vec<_>>(),
        }),
    };
    let options = &definition.options;
    json!({
        "parameters": parameters,
        "returns": returns,
        "options": {
            "schemabinding": options.schemabinding,
            "encryption": options.encryption,
            "native_compilation": options.native_compilation,
            "returns_null_on_null_input": options.returns_null_on_null_input,
            "execute_as": options.execute_as,
            "inline": options.inline,
        },
    })
}

fn respond(session: &mut Session, rpc: bool, result: Result<()>) -> (Vec<u8>, bool) {
    respond_with(session, rpc, result, 222)
}

/// The completion of a definition batch: DONE (CurCmd 222 as captured), or
/// the RPC epilogue.
fn respond_with(
    session: &mut Session,
    rpc: bool,
    result: Result<()>,
    command: u16,
) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    match result {
        Ok(()) => {
            session.last_error = 0;
            session.rowcount = 0;
            if rpc {
                if !session.nocount {
                    crate::tds::done(&mut out, 0xff, 1, command, 0);
                }
                out.push(0x79);
                out.extend(0i32.to_le_bytes());
                crate::tds::done(&mut out, 0xfe, 0, 0xe0, 0);
            } else {
                crate::tds::done(&mut out, 0xfd, 0, command, 0);
            }
            (out, true)
        }
        Err(error) => {
            session.last_error = emit_error(&mut out, &error);
            if rpc {
                crate::tds::done(&mut out, 0xfe, 2, 0, 0);
            } else {
                crate::tds::done(&mut out, 0xfd, 2, command, 0);
            }
            (out, false)
        }
    }
}
