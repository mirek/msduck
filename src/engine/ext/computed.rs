//! Computed columns over Unicode and JSON expressions, and column DEFAULTs
//! that read session state (issue #718; see docs/gaps-computed.md).
//!
//! - [`columns`] lowers computed-column expressions over carrier-stored
//!   types (nvarchar, nchar, datetime2, ...) through the ordinary query
//!   pipeline before the table is created.
//! - [`variables`] mirrors the session's login, client names and
//!   SESSION_CONTEXT values into DuckDB variables of its connection, which
//!   stored defaults and `HOST_NAME()`/`APP_NAME()` read.
use super::{Execution, Feature, Parameter, Session};
use anyhow::Result;
use msduck_sql::dialect::ext::computed::session as stored;
use sqlparser::ast::{Expr, Statement};
use std::collections::HashMap;

mod columns;
mod text;
mod variables;

#[derive(Default)]
pub(crate) struct State {
    variables: variables::State,
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "computed"
    }

    fn register(&self, db: &duckdb::Connection) -> Result<()> {
        text::register(db)
    }

    fn batch_begin(&self, session: &mut Session, _rpc: bool) {
        variables::sync(session);
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        variables::sync(session);
        stored::rewrite_defaults(statement)?;
        match statement {
            Statement::CreateTable(table) => columns::lower(session, table, parameters)?,
            Statement::CreateIndex(index) => columns::check_index_keys(session, index)?,
            _ => {}
        }
        Ok(None)
    }

    fn rewrite_expr(
        &self,
        _session: &Session,
        expr: &mut Expr,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        stored::lower_client_name(expr);
        Ok(())
    }
}
