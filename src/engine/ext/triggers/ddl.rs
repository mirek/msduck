//! CREATE/ALTER/DROP TRIGGER, DISABLE/ENABLE TRIGGER and the triggers of
//! dropped tables. Definitions live in the module store (type `TR`).
use super::super::modules;
use super::{
    Execution, Parameter, Session, Table, Trigger, any_triggers, find_trigger, properties, resolve,
    triggers_on,
};
use crate::tds;
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use msduck_sql::dialect::ext::triggers::{self as syntax, Action, Command, Definition, Target};
use sqlparser::ast::{ObjectType, Statement};
use std::collections::HashMap;

/// Completion command SQL Server reports for CREATE/ALTER TRIGGER.
const CREATE_COMMAND: u16 = 221;
const DROP_COMMAND: u16 = 225;
const TOGGLE_COMMAND: u16 = 253;
const ALTER_TABLE_COMMAND: u16 = 216;

/// A trigger definition must be alone in its batch and keeps its source text.
pub(super) fn batch(session: &mut Session, sql: &str, rpc: bool) -> Option<(Vec<u8>, bool)> {
    let definition = match syntax::definition(sql) {
        Some(definition) => definition,
        None => {
            let statement = syntax::misplaced(sql)?;
            let error = SqlError::syntax(
                111,
                6,
                format!("'{statement}' must be the first statement in a query batch."),
            );
            return Some(respond(session, rpc, Err(error.into()), TOGGLE_COMMAND));
        }
    };
    let result = definition
        .map_err(anyhow::Error::from)
        .and_then(|definition| create(session, sql, &definition));
    let command = match &result {
        Err(error)
            if error
                .downcast_ref::<SqlError>()
                .is_some_and(|error| !matches!(error.number, 2714 | 2111 | 2110 | 2103)) =>
        {
            TOGGLE_COMMAND
        }
        _ => CREATE_COMMAND,
    };
    Some(respond(session, rpc, result, command))
}

fn respond(session: &mut Session, rpc: bool, result: Result<()>, command: u16) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    match result {
        Ok(()) => {
            session.last_error = 0;
            session.rowcount = 0;
            if rpc {
                if !session.nocount {
                    tds::done(&mut out, 0xff, 1, CREATE_COMMAND, 0);
                }
                out.push(0x79);
                out.extend(0i32.to_le_bytes());
                tds::done(&mut out, 0xfe, 0, 0xe0, 0);
            } else {
                tds::done(&mut out, 0xfd, 0, CREATE_COMMAND, 0);
            }
            (out, true)
        }
        Err(error) => {
            session.last_error = super::super::super::emit_error(&mut out, &error);
            tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, command, 0);
            (out, false)
        }
    }
}

fn written(parts: &[String]) -> String {
    parts.join(".")
}

fn not_found(name: &str, state: u8) -> SqlError {
    SqlError::new(
        8197,
        state,
        format!("The object '{name}' does not exist or is invalid for this operation."),
    )
}

/// CREATE, ALTER or CREATE OR ALTER TRIGGER.
fn create(session: &mut Session, sql: &str, definition: &Definition) -> Result<()> {
    if let Err(error) = msduck_sql::batch::parse(&sql[definition.body..]) {
        if error.downcast_ref::<SqlError>().is_some() {
            return Err(error);
        }
        return Err(SqlError::syntax(102, 1, error.to_string()).into());
    }
    let Target::Object(parts) = &definition.target else {
        bail!("DDL triggers (ON DATABASE or ON ALL SERVER) are not supported");
    };
    let table_name = written(parts);
    let table = resolve(session, parts)?.ok_or_else(|| not_found(&table_name, 4))?;
    match table.kind.as_str() {
        "U" => {}
        "V" if definition.instead_of => {
            bail!("INSTEAD OF triggers on views are not supported")
        }
        "V" => return Err(not_found(&table_name, 6).into()),
        _ => return Err(not_found(&table_name, 4).into()),
    }
    if let Some(schema) = &definition.schema
        && !schema.eq_ignore_ascii_case(&table.schema)
    {
        return Err(SqlError::syntax(
            2103,
            1,
            format!(
                "Cannot create trigger '{schema}.{}' because its schema is different from the schema of the target table or view.",
                definition.name
            ),
        )
        .into());
    }
    let db = &session.db;
    let existing = find_trigger(db, Some(&table.schema), &definition.name)?;
    let replace = match (definition.action, &existing) {
        (Action::Create, _) | (Action::CreateOrAlter, None) => None,
        (Action::Alter | Action::CreateOrAlter, Some(existing)) => Some(existing),
        (Action::Alter, None) => {
            return Err(SqlError::new(
                208,
                6,
                format!("Invalid object name '{}'.", definition.name),
            )
            .into());
        }
    };
    if let Some(existing) = replace
        && existing.parent != table.object_id
    {
        return Err(SqlError::syntax(
            2110,
            1,
            format!(
                "Cannot alter trigger '{}' on '{table_name}' because this trigger does not belong to this object. Specify the correct trigger name or the correct target object name.",
                definition.name
            ),
        )
        .into());
    }
    if replace.is_none() {
        let exists: bool = db.query_row(
            "SELECT EXISTS (SELECT 1 FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE lower(s.name) = lower(?) AND lower(o.name) = lower(?))",
            [&table.schema, &definition.name],
            |row| row.get(0),
        )?;
        if exists {
            return Err(SqlError::new(
                2714,
                2,
                format!(
                    "There is already an object named '{}' in the database.",
                    definition.name
                ),
            )
            .into());
        }
    }
    if definition.instead_of {
        for other in triggers_on(db, table.object_id)? {
            if Some(other.object_id) == replace.map(|existing| existing.object_id)
                || !other.instead_of
            {
                continue;
            }
            if let Some(event) = definition
                .events
                .iter()
                .find(|event| other.events.contains(event))
            {
                return Err(SqlError::new(
                    2111,
                    1,
                    format!(
                        "Cannot create trigger '{}' on table '{}' because an INSTEAD OF {} trigger already exists on this object.",
                        definition.name,
                        parts.last().unwrap(),
                        event.name()
                    ),
                )
                .into());
            }
        }
    }
    let text = syntax::stored_text(sql, definition);
    let properties = properties(&definition.events, definition.instead_of);
    match replace {
        Some(existing) => modules::alter(db, existing.object_id, &text, &properties)?,
        None => {
            modules::create(
                db,
                Some(&table.schema),
                &definition.name,
                modules::kind::TRIGGER,
                table.object_id,
                &text,
                &properties,
            )?;
        }
    }
    Ok(())
}

/// DROP TRIGGER and DISABLE/ENABLE TRIGGER.
pub(super) fn command(session: &mut Session, command: Command) -> Result<Execution> {
    match command {
        Command::Drop {
            if_exists,
            names,
            ddl,
        } => drop(session, if_exists, &names, ddl),
        Command::Toggle {
            enable,
            names,
            target,
            alter_table,
        } => toggle(session, enable, names.as_deref(), &target, alter_table),
    }
}

fn split(name: &[String]) -> (Option<&str>, &str) {
    match name {
        [.., schema, trigger] => (Some(schema.as_str()), trigger.as_str()),
        [trigger] => (None, trigger.as_str()),
        [] => (None, ""),
    }
}

fn errors(mut errors: Vec<SqlError>) -> anyhow::Error {
    if errors.len() == 1 {
        errors.remove(0).into()
    } else {
        super::super::super::StatementErrors(errors).into()
    }
}

fn drop(
    session: &mut Session,
    if_exists: bool,
    names: &[Vec<String>],
    ddl: bool,
) -> Result<Execution> {
    let mut failures = Vec::new();
    for name in names {
        let (schema, trigger) = split(name);
        let found = if ddl {
            None
        } else {
            find_trigger(&session.db, schema, trigger)?
        };
        match found {
            Some(found) => modules::remove(&session.db, found.object_id)?,
            None if if_exists => {}
            None => {
                let mut error = SqlError::new(
                    3701,
                    5,
                    format!(
                        "Cannot drop the trigger '{}', because it does not exist or you do not have permission.",
                        written(name)
                    ),
                );
                error.severity = 11;
                failures.push(error);
            }
        }
    }
    if !failures.is_empty() {
        return Err(errors(failures));
    }
    Ok(Execution::statement(vec![], None, DROP_COMMAND))
}

fn missing(name: &str, state: u8) -> SqlError {
    SqlError::new(
        1088,
        state,
        format!(
            "Cannot find the object \"{name}\" because it does not exist or you do not have permissions."
        ),
    )
}

fn toggle(
    session: &mut Session,
    enable: bool,
    names: Option<&[Vec<String>]>,
    target: &Target,
    alter_table: bool,
) -> Result<Execution> {
    let command = if alter_table {
        ALTER_TABLE_COMMAND
    } else {
        TOGGLE_COMMAND
    };
    let parts = match target {
        // No DDL triggers exist, so ALL is a no-op and names are missing.
        Target::Ddl => {
            return match names.and_then(<[_]>::first) {
                None => Ok(Execution::statement(vec![], None, command)),
                Some(name) => Err(missing(&written(name), 119).into()),
            };
        }
        Target::Object(parts) => parts,
    };
    let table_name = written(parts);
    let table: Option<Table> = resolve(session, parts)?.filter(|table| table.kind == "U");
    let Some(table) = table else {
        return Err(if alter_table {
            SqlError::new(
                4902,
                1,
                format!(
                    "Cannot find the object \"{table_name}\" because it does not exist or you do not have permissions."
                ),
            )
        } else {
            missing(&table_name, 21)
        }
        .into());
    };
    let triggers = triggers_on(&session.db, table.object_id)?;
    let selected: Vec<&Trigger> = match names {
        None => triggers.iter().collect(),
        Some(names) => {
            let mut selected = Vec::new();
            for name in names {
                let (schema, trigger) = split(name);
                let found = triggers.iter().find(|candidate| {
                    candidate.name.eq_ignore_ascii_case(trigger)
                        && schema.is_none_or(|schema| candidate.schema.eq_ignore_ascii_case(schema))
                });
                match found {
                    Some(found) => selected.push(found),
                    None if alter_table => {
                        return Err(SqlError::new(
                            4920,
                            0,
                            format!(
                                "ALTER TABLE failed because trigger '{trigger}' on table '{}' does not exist.",
                                parts.last().unwrap()
                            ),
                        )
                        .into());
                    }
                    None => return Err(missing(&written(name), 119).into()),
                }
            }
            selected
        }
    };
    for trigger in selected {
        modules::set_disabled(&session.db, trigger.object_id, !enable)?;
    }
    Ok(Execution::statement(vec![], None, command))
}

/// DROP TABLE also drops the table's triggers.
pub(super) fn drop_table(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::Drop {
        object_type: ObjectType::Table,
        names,
        ..
    } = statement
    else {
        return Ok(None);
    };
    if !any_triggers(&session.db) {
        return Ok(None);
    }
    let mut tables = Vec::new();
    for name in names.iter() {
        let parts = name
            .0
            .iter()
            .filter_map(|part| part.as_ident().map(|ident| ident.value.clone()))
            .collect::<Vec<_>>();
        if let Ok(Some(table)) = resolve(session, &parts)
            && table.kind == "U"
            && !triggers_on(&session.db, table.object_id)?.is_empty()
        {
            tables.push(table.object_id);
        }
    }
    if tables.is_empty() {
        return Ok(None);
    }
    let statement = statement.clone();
    super::atomically(session, |session| {
        let execution = super::super::reenter(session, "triggers", |session| {
            session.execute(statement, parameters)
        })?;
        for table in tables {
            let exists: bool = session.db.query_row(
                "SELECT EXISTS (SELECT 1 FROM sys.objects WHERE object_id = ?)",
                [table],
                |row| row.get(0),
            )?;
            if !exists {
                for trigger in triggers_on(&session.db, table)? {
                    modules::remove(&session.db, trigger.object_id)?;
                }
            }
        }
        Ok(Some(execution))
    })
}
