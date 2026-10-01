//! FOR JSON AUTO, JSON_MODIFY, ordered STRING_AGG, STRING_SPLIT and HASHBYTES.
//! See docs/gaps-json_string.md.
//!
//! HASHBYTES, JSON_MODIFY, STRING_AGG and STRING_SPLIT are lowered to native
//! functions before result metadata is derived ([`rewrite`]); FOR JSON AUTO
//! is formatted by the FOR JSON adapter in `src/for_json/auto.rs`.
use super::Feature;
use crate::engine::{Execution, Parameter, Session};
use anyhow::Result;
use sqlparser::ast::{Expr, Statement};
use std::collections::HashMap;

#[path = "../../../crates/msduck-core/src/hashbytes.rs"]
#[allow(dead_code)] // Only the digests and algorithm names are used here.
mod digest;
mod native;
mod rewrite;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "json_string"
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        rewrite::lower(session, statement, parameters)?;
        Ok(None)
    }

    fn rewrite_statement(
        &self,
        session: &Session,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        rewrite::lower(session, statement, parameters)
    }

    fn rewrite_expr(
        &self,
        session: &Session,
        expr: &mut Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        rewrite::lower(session, expr, parameters)
    }

    fn register(&self, db: &duckdb::Connection) -> Result<()> {
        native::register(db)?;
        // STRING_AGG input text: carriers decode to VARCHAR, other values
        // use their VARCHAR spelling. typeof is a bind-time property.
        db.execute_batch(
            "CREATE OR REPLACE MACRO __msduck_json_string_input(v) AS CASE WHEN typeof(v)='STRUCT(__msduck_utf16le BLOB)' THEN __msduck_json_string_text(v) WHEN typeof(v) LIKE 'STRUCT(__msduck_datetime2_%' THEN __msduck_json_string_text(v) WHEN typeof(v)='BLOB' THEN error('__msduck_sql_error_v1:8116:1:16:Argument data type varbinary is invalid for argument 1 of string_agg function.') ELSE CAST(v AS VARCHAR) END",
        )?;
        Ok(())
    }
}
