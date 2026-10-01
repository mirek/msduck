//! Constraint, module and file catalog views, OBJECT_DEFINITION, sp_pkeys, sp_fkeys and sp_rename.
//!
use super::Feature;
use anyhow::Result;
use duckdb::Connection;
use sqlparser::ast::{DataType, Expr};

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn bootstrap_database(&self, db: &Connection) -> Result<()> {
        // Read the shared transactional store, rather than keeping a second
        // module identity/definition cache. Replication and startup execution
        // are not supported by the module store.
        db.execute_batch(
            "CREATE OR REPLACE VIEW sys.procedures AS
               SELECT o.*, false AS is_auto_executed,
                 false AS is_execution_replicated,
                 false AS is_repl_serializable_only,
                 false AS skips_repl_constraints
               FROM sys.objects o JOIN main.__msduck_modules m USING(object_id)
               WHERE m.type_code = 'P';
             CREATE OR REPLACE MACRO main.__msduck_object_definition(value) AS
               map_extract_value((SELECT map(list(object_id),list(definition))
                 FROM main.__msduck_modules), value);",
        )?;
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
            .unwrap()
            .clone();
        *expr = crate::engine::unary_function(
            "__msduck_object_definition",
            crate::assignment::convert(value, &DataType::Int(None), false),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::modules;
    use crate::{engine::Session, server::Server};

    #[test]
    fn procedure_catalog_and_definitions_follow_transactional_module_lifecycle() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let definition = "CREATE PROCEDURE dbo.p AS\n  SELECT 'original source'";
        let id = modules::create(&session.db, None, "p", "P", 0, definition, "{}").unwrap();
        let function =
            modules::create(&session.db, None, "f", "FN", 0, "function source", "{}").unwrap();
        let rows: Vec<(i32, String)> = session
            .db
            .prepare("SELECT object_id,name FROM sys.procedures WHERE name IN ('p','f')")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(id, "p".into())]);
        let stored = |id: Option<i32>| {
            session
                .db
                .query_row("SELECT main.__msduck_object_definition(?)", [id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .unwrap()
        };
        assert_eq!(stored(Some(id)), Some(definition.into()));
        assert_eq!(stored(Some(function)), Some("function source".into()));
        assert_eq!(stored(Some(-123)), None);
        assert_eq!(stored(None), None);
        modules::rename(&session.db, id, "renamed").unwrap();
        assert_eq!(stored(Some(id)), Some(definition.into()));
        session.db.execute_batch("BEGIN TRANSACTION").unwrap();
        modules::alter(&session.db, id, "replacement", "{}").unwrap();
        assert_eq!(stored(Some(id)), Some("replacement".into()));
        session.db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(stored(Some(id)), Some(definition.into()));
        // Exercise the public T-SQL path too, including object-id lowering.
        assert!(
            session
                .batch_response(
                    "SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.renamed')), OBJECT_DEFINITION(NULL)",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        modules::remove(&session.db, id).unwrap();
        assert_eq!(
            session
                .db
                .query_row("SELECT main.__msduck_object_definition(?)", [id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .unwrap(),
            None
        );
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM sys.procedures WHERE name='renamed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
}
