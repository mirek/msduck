//! CREATE TABLE with rowversion columns or decimal identity columns.
use super::{atomically, decimal, execute, names, rowversion};
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use sqlparser::ast::Statement;
use std::collections::HashMap;

pub(super) fn create(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let Statement::CreateTable(table) = statement else {
        return Ok(None);
    };
    if table.query.is_some() {
        return Ok(None);
    }
    let Some(target) = names::table(session, &table.name) else {
        return Ok(None);
    };
    let mut planned = table.clone();
    let rowversions = rowversion::plan_create(&mut planned)?;
    let allocators = decimal::plan_create(&session.db, &mut planned)?;
    if rowversions.is_empty() && allocators.is_empty() {
        return Ok(None);
    }
    // An existing table is reported (or skipped) by CREATE TABLE itself;
    // no allocator may be created for it.
    if !names::columns(&session.db, &target)?.is_empty() {
        if allocators.is_empty() {
            *statement = Statement::CreateTable(planned);
        }
        return Ok(None);
    }
    let name = planned.name.clone();
    atomically(session, |session| {
        for allocator in &allocators {
            allocator.install(&session.db)?;
        }
        let execution = execute(session, Statement::CreateTable(planned), parameters)?;
        rowversion::record(&session.db, &name, &rowversions)?;
        Ok(execution)
    })
    .map(Some)
}
