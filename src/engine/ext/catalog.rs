//! Constraint, module and file catalog views, OBJECT_DEFINITION, sp_pkeys,
//! sp_fkeys and sp_rename (issue #728; see docs/gaps-catalog.md).
//!
//! - [`views`] defines `sys.default_constraints`, `sys.computed_columns`,
//!   `sys.triggers`, `sys.sql_modules`, `sys.procedures`, `sys.parameters`
//!   and the SQL Server definitions of `sys.check_constraints`, over the
//!   stores of the constraints, keys, computed-column and module features.
//! - [`sources`] keeps declarations as written: view text, DEFAULT and
//!   computed-column expressions and the clustering of key constraints,
//!   which other features rewrite before the catalog sees them.
//! - [`visible`] hides the backend tables of temporary objects from
//!   `sys.objects`, `sys.tables` and `INFORMATION_SCHEMA`, whose views it
//!   provides.
//! - [`procedures`] runs `sp_pkeys` and `sp_fkeys`, [`rename`] `sp_rename`.
use super::{Feature, Parameter, Session};
use anyhow::Result;
use duckdb::Connection;
use sqlparser::ast::{DataType, Expr, Statement};
use std::collections::HashMap;

mod call;
mod functions;
mod namespace;
mod procedures;
mod rename;
pub(super) mod sources;
mod views;
mod visible;

pub(crate) use sources::State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn register(&self, db: &Connection) -> Result<()> {
        functions::register(db)
    }

    fn bootstrap_database(&self, db: &Connection) -> Result<()> {
        views::bootstrap(db)
    }

    fn batch_begin(&self, session: &mut Session, _rpc: bool) {
        sources::begin(session);
    }

    fn batch(
        &self,
        session: &mut Session,
        sql: &str,
        _parameters: &HashMap<String, Parameter>,
        _rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        sources::batch(session, sql);
        None
    }

    fn batch_end(&self, session: &mut Session) {
        sources::end(session);
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<super::Execution>> {
        sources::statement(session, statement, parameters)
    }

    fn exec(
        &self,
        session: &mut Session,
        statement: &Statement,
        variables: &mut HashMap<String, Parameter>,
    ) -> Option<Result<super::Exec>> {
        let name = call::procedure(statement)?;
        match name.as_str() {
            "sp_pkeys" => Some(procedures::pkeys(session, statement, variables)),
            "sp_fkeys" => Some(procedures::fkeys(session, statement, variables)),
            "sp_rename" => Some(rename::run(session, statement, variables)),
            _ => None,
        }
    }

    fn rewrite_statement(
        &self,
        session: &Session,
        statement: &mut Statement,
        _parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        visible::rewrite(session, statement);
        Ok(())
    }

    fn lower_expr(&self, expr: &mut Expr) -> Result<(), String> {
        let Expr::Function(function) = expr else {
            return Ok(());
        };
        if !function
            .name
            .to_string()
            .eq_ignore_ascii_case("OBJECT_DEFINITION")
        {
            return Ok(());
        }
        let value = crate::function_args::unary(function, "OBJECT_DEFINITION")?
            .ok_or("OBJECT_DEFINITION requires one argument")?
            .clone();
        *expr = crate::engine::unary_function(
            "__msduck_object_definition",
            crate::assignment::convert(value, &DataType::Int(None), false),
        );
        Ok(())
    }
}
