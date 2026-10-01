//! UPDATE and DELETE whose target tree contains outer joins.
//!
//! SQL Server evaluates the whole FROM tree and changes each target row that
//! appears in it once, with the values of one of its joined rows. DELETE is
//! canonicalized to a row-identity subquery over the intact tree
//! (`msduck_sql::delete`), which deletes each row once. This hook applies it
//! before OUTPUT planning, so `OUTPUT deleted.*` sees the base table and its
//! declarations. UPDATE must choose one joined row per target row before evaluating
//! its assignments, which is what the joined OUTPUT image stages already do
//! (`super::super::joined_output`). This hook routes an outer-tree UPDATE
//! without OUTPUT through those stages by giving it an empty OUTPUT list,
//! which the stages treat as "write, but return no rows". See
//! docs/gaps-outer_dml.md.
use super::super::{Execution, Parameter, Session};
use super::Feature;
use anyhow::Result;
use sqlparser::ast::{
    Delete, OutputClause, SetExpr, Statement, Update, helpers::attached_token::AttachedToken,
};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "outer_dml"
    }

    fn statement(
        &self,
        _session: &mut Session,
        statement: &mut Statement,
        _parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        if let Some(update) = target(statement)
            && msduck_sql::update::has_outer_target(update)
        {
            msduck_sql::update::unqualify_assignments(update);
            if update.output.is_none() {
                update.output = Some(OutputClause::Output {
                    output_token: AttachedToken::empty(),
                    select_items: vec![],
                    into_table: None,
                });
            }
        }
        if let Some(delete) = removal(statement)
            && msduck_sql::delete::has_outer_target(delete)
        {
            let mut canonical = Statement::Delete(delete.clone());
            msduck_sql::delete::canonicalize(&mut canonical)?;
            let Statement::Delete(canonical) = canonical else {
                unreachable!()
            };
            *delete = canonical;
        }
        Ok(None)
    }
}

/// The DELETE of a plain or CTE-wrapped DELETE statement.
fn removal(statement: &mut Statement) -> Option<&mut Delete> {
    match statement {
        Statement::Delete(delete) => Some(delete),
        Statement::Query(query) => match query.body.as_mut() {
            SetExpr::Delete(Statement::Delete(delete)) => Some(delete),
            _ => None,
        },
        _ => None,
    }
}

/// The UPDATE of a plain or CTE-wrapped UPDATE statement.
fn target(statement: &mut Statement) -> Option<&mut Update> {
    match statement {
        Statement::Update(update) => Some(update),
        Statement::Query(query) => match query.body.as_mut() {
            SetExpr::Update(Statement::Update(update)) => Some(update),
            _ => None,
        },
        _ => None,
    }
}
