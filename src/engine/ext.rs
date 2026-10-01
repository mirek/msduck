//! Extension hooks for SQL Server features that live in their own modules.
//!
//! Each feature module under `ext/` implements [`Feature`] and overrides only
//! the hooks it needs; every default is a no-op that leaves the existing
//! engine behavior unchanged. The dispatchers below call features in the
//! order of [`FEATURES`], and the first feature that claims a request wins.
//! See `docs/extension-hooks.md` for when each hook runs.
use super::{Execution, Parameter, Session};
use anyhow::Result;
use sqlparser::ast::{Expr, Statement, VisitMut, VisitorMut};
use std::{collections::HashMap, ops::ControlFlow};

pub(crate) mod modules;

mod applock;
mod backup;
mod bulk;
mod catalog;
mod computed;
mod constraints;
mod conversion;
mod functions;
mod identifiers;
mod json_string;
mod keys;
mod merge;
mod outer_dml;
mod procedures;
mod rowversion_identity;
mod temp_tables;
mod transactions;
mod triggers;

/// Outcome of a procedure call handled by an [`Feature::exec`] hook. The
/// engine writes `tokens`, then RETURNSTATUS `status` and DONEPROC, exactly
/// as for `sp_set_session_context`.
pub(crate) struct Exec {
    pub tokens: Vec<u8>,
    pub status: i32,
}

impl Exec {
    pub fn status(status: i32) -> Self {
        Self {
            tokens: Vec::new(),
            status,
        }
    }
}

/// Hooks a feature may implement. All defaults decline.
pub(super) trait Feature: Sync {
    /// Stable name, used by [`reenter`] to skip this feature's own hooks.
    fn name(&self) -> &'static str;

    /// A whole batch, before parsing. Return the complete token stream
    /// (including the final DONE) and whether the batch succeeded. Use for
    /// statements that SQL Server requires to be alone in their batch and
    /// whose source text must be kept, such as CREATE PROCEDURE.
    fn batch(
        &self,
        _session: &mut Session,
        _sql: &str,
        _parameters: &HashMap<String, Parameter>,
        _rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        None
    }

    /// A procedure call (`Statement::Execute`), after preflight and before
    /// translation. Named arguments arrive as `@name = value` binary
    /// expressions.
    fn exec(
        &self,
        _session: &mut Session,
        _statement: &Statement,
        _variables: &mut HashMap<String, Parameter>,
    ) -> Option<Result<Exec>> {
        None
    }

    /// Any other leaf statement, before database qualification and catalog
    /// bookkeeping. `Ok(Some)` replaces execution; `Ok(None)` continues with
    /// the (possibly modified) statement.
    fn statement(
        &self,
        _session: &mut Session,
        _statement: &mut Statement,
        _parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        Ok(None)
    }

    /// Session-aware rewrite of a statement about to be translated.
    fn rewrite_statement(
        &self,
        _session: &Session,
        _statement: &mut Statement,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        Ok(())
    }

    /// Session-aware rewrite of each expression (pre-order) in statements and
    /// in scalar evaluations such as SET, IF and WHILE conditions.
    fn rewrite_expr(
        &self,
        _session: &Session,
        _expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        Ok(())
    }

    /// Pure lowering in the translator's post-visit, after the built-in
    /// lowerings have run on the children.
    fn lower_expr(&self, _expr: &mut Expr) -> Result<(), String> {
        Ok(())
    }

    /// Whether a transaction isolation level (TDS numbering: 1 read
    /// uncommitted, 2 read committed, 3 repeatable read, 4 serializable,
    /// 5 snapshot) is accepted. `None` defers to the built-in rule.
    fn isolation(&self, _isolation: u8) -> Option<Result<()>> {
        None
    }

    /// The outermost transaction ended (`committed` false for rollback).
    fn transaction_end(&self, _session: &mut Session, _committed: bool) {}

    /// A transaction-manager savepoint request (TDS `Save`), inside a
    /// transaction. Return ENVCHANGE tokens, if any.
    fn save_transaction(&self, _session: &mut Session, _name: &str) -> Option<Result<Vec<u8>>> {
        None
    }

    /// ROLLBACK to `name` when it is not the outermost transaction's name
    /// (SQL `ROLLBACK TRAN name` or a TDS rollback with a name). Return
    /// ENVCHANGE tokens, if any; the transaction stays open.
    fn rollback_to(&self, _session: &mut Session, _name: &str) -> Option<Result<Vec<u8>>> {
        None
    }

    /// Instance-wide native functions, registered once per DuckDB instance.
    fn register(&self, _db: &duckdb::Connection) -> Result<()> {
        Ok(())
    }

    /// Catalog-local objects for one database, in its connection's default
    /// catalog. Runs on every startup, so it must be idempotent.
    fn bootstrap_database(&self, _db: &duckdb::Connection) -> Result<()> {
        Ok(())
    }

    /// A new session (including after RESETCONNECTION).
    fn session_start(&self, _session: &mut Session) -> Result<()> {
        Ok(())
    }

    /// The session is closing or being reset.
    fn session_end(&self, _session: &mut Session) {}
}

/// Every feature, in dispatch order.
static FEATURES: &[&dyn Feature] = &[
    &modules::Hooks,
    &identifiers::Hooks,
    &transactions::Hooks,
    &applock::Hooks,
    &backup::Hooks,
    &procedures::Hooks,
    &functions::Hooks,
    &triggers::Hooks,
    &temp_tables::Hooks,
    &keys::Hooks,
    &constraints::Hooks,
    &rowversion_identity::Hooks,
    &computed::Hooks,
    &merge::Hooks,
    &outer_dml::Hooks,
    &catalog::Hooks,
    &json_string::Hooks,
    &conversion::Hooks,
    &bulk::Hooks,
];

/// Per-session feature state.
#[derive(Default)]
#[allow(dead_code)] // Read by feature modules as their tasks land.
pub(crate) struct State {
    /// Features whose hooks are suspended by [`reenter`].
    skip: Vec<&'static str>,
    pub(super) applock: applock::State,
    pub(super) backup: backup::State,
    pub(super) bulk: bulk::State,
    pub(super) catalog: catalog::State,
    pub(super) computed: computed::State,
    pub(super) constraints: constraints::State,
    pub(super) conversion: conversion::State,
    pub(super) functions: functions::State,
    pub(super) identifiers: identifiers::State,
    pub(super) json_string: json_string::State,
    pub(super) keys: keys::State,
    pub(super) merge: merge::State,
    pub(super) outer_dml: outer_dml::State,
    pub(super) procedures: procedures::State,
    pub(super) rowversion_identity: rowversion_identity::State,
    pub(super) temp_tables: temp_tables::State,
    pub(super) transactions: transactions::State,
    pub(super) triggers: triggers::State,
}

fn active(session: &Session) -> impl Iterator<Item = &'static dyn Feature> + '_ {
    FEATURES
        .iter()
        .copied()
        .filter(|feature| !session.ext.skip.contains(&feature.name()))
}

/// Run `f` with `feature`'s statement, EXEC and batch hooks suspended, so a
/// feature can execute the statement it intercepted (or derived statements)
/// through the ordinary engine path without recursing into itself. Other
/// features still see those statements.
#[allow(dead_code)] // Used by feature modules as their tasks land.
pub(crate) fn reenter<T>(
    session: &mut Session,
    feature: &'static str,
    f: impl FnOnce(&mut Session) -> T,
) -> T {
    session.ext.skip.push(feature);
    let result = f(session);
    if let Some(index) = session.ext.skip.iter().rposition(|name| *name == feature) {
        session.ext.skip.remove(index);
    }
    result
}

pub(super) fn batch(
    session: &mut Session,
    sql: &str,
    parameters: &HashMap<String, Parameter>,
    rpc: bool,
) -> Option<(Vec<u8>, bool)> {
    let features: Vec<_> = active(session).collect();
    features
        .into_iter()
        .find_map(|feature| feature.batch(session, sql, parameters, rpc))
}

pub(super) fn exec(
    session: &mut Session,
    statement: &Statement,
    variables: &mut HashMap<String, Parameter>,
) -> Option<Result<Exec>> {
    if !matches!(statement, Statement::Execute { .. }) {
        return None;
    }
    let features: Vec<_> = active(session).collect();
    features
        .into_iter()
        .find_map(|feature| feature.exec(session, statement, variables))
}

pub(super) fn statement(
    session: &mut Session,
    statement: &mut Statement,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Option<Execution>> {
    let features: Vec<_> = active(session).collect();
    for feature in features {
        if let Some(execution) = feature.statement(session, statement, parameters)? {
            return Ok(Some(execution));
        }
    }
    Ok(None)
}

/// Apply the session-aware rewrites to a statement or expression tree.
pub(super) fn rewrite<T: VisitMut + 'static>(
    session: &Session,
    node: &mut T,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    if let Some(statement) = (node as &mut dyn std::any::Any).downcast_mut::<Statement>() {
        for feature in FEATURES {
            feature.rewrite_statement(session, statement, parameters)?;
        }
    }
    struct Rewrite<'a> {
        session: &'a Session,
        parameters: &'a HashMap<String, Parameter>,
    }
    impl VisitorMut for Rewrite<'_> {
        type Break = anyhow::Error;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
            for feature in FEATURES {
                if let Err(error) = feature.rewrite_expr(self.session, expr, self.parameters) {
                    return ControlFlow::Break(error);
                }
            }
            ControlFlow::Continue(())
        }
    }
    match node.visit(&mut Rewrite {
        session,
        parameters,
    }) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

pub(super) fn lower_expr(expr: &mut Expr) -> Result<(), String> {
    for feature in FEATURES {
        feature.lower_expr(expr)?;
    }
    Ok(())
}

pub(super) fn isolation(isolation: u8) -> Option<Result<()>> {
    FEATURES
        .iter()
        .find_map(|feature| feature.isolation(isolation))
}

pub(super) fn save_transaction(session: &mut Session, name: &str) -> Option<Result<Vec<u8>>> {
    FEATURES
        .iter()
        .find_map(|feature| feature.save_transaction(session, name))
}

pub(super) fn rollback_to(session: &mut Session, name: &str) -> Option<Result<Vec<u8>>> {
    FEATURES
        .iter()
        .find_map(|feature| feature.rollback_to(session, name))
}

pub(super) fn transaction_end(session: &mut Session, committed: bool) {
    for feature in FEATURES {
        feature.transaction_end(session, committed);
    }
}

pub(crate) fn register(db: &duckdb::Connection) -> Result<()> {
    for feature in FEATURES {
        feature.register(db)?;
    }
    Ok(())
}

pub(crate) fn bootstrap_database(db: &duckdb::Connection) -> Result<()> {
    for feature in FEATURES {
        feature.bootstrap_database(db)?;
    }
    Ok(())
}

pub(super) fn session_start(session: &mut Session) -> Result<()> {
    for feature in FEATURES {
        feature.session_start(session)?;
    }
    Ok(())
}

pub(super) fn session_end(session: &mut Session) {
    for feature in FEATURES {
        feature.session_end(session);
    }
}
