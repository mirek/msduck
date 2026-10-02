//! Constraint, module and file catalog views, OBJECT_DEFINITION, sp_pkeys, sp_fkeys and sp_rename.
//!
use super::Feature;
use anyhow::Result;
use duckdb::Connection;
use sqlparser::ast::{DataType, Expr, Statement};

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "catalog"
    }

    fn statement(
        &self,
        session: &mut super::Session,
        statement: &mut Statement,
        _parameters: &mut std::collections::HashMap<String, super::Parameter>,
    ) -> Result<Option<super::Execution>> {
        let name = match statement {
            Statement::CreateTable(table) => &table.name,
            Statement::CreateView(view) if !view.or_replace && !view.or_alter => &view.name,
            _ => return Ok(None),
        };
        let parts = name
            .0
            .iter()
            .map(|part| part.as_ident().map(|ident| ident.value.clone()))
            .collect::<Option<Vec<_>>>();
        let Some(parts) = parts else {
            return Ok(None);
        };
        let (schema, name) = match parts.as_slice() {
            [name] => ("dbo", name.as_str()),
            [schema, name] => (schema.as_str(), name.as_str()),
            _ => return Ok(None),
        };
        let target_exists: bool = session.db.query_row(
            "SELECT count(*)>0 FROM sys.objects o JOIN main.__msduck_schemas s USING(schema_id) WHERE lower(s.name)=lower(?) AND lower(o.name)=lower(?) AND rtrim(o.type) IN ('U','V')",
            [schema, name], |r| r.get(0)
        )?;
        if target_exists {
            anyhow::bail!(msduck_core::diagnostic::SqlError::new(
                2714,
                6,
                format!("There is already an object named '{name}' in the database.")
            ));
        }
        if let Statement::CreateTable(table) = statement {
            // Native DDL must not succeed before a named DEFAULT's object
            // namespace conflict is detected in a caller-owned transaction.
            let mut declared = std::collections::HashSet::new();
            let normalized: String = session
                .db
                .query_row("SELECT lower(?)", [name], |r| r.get(0))?;
            declared.insert(normalized);
            let constraints = table
                .columns
                .iter()
                .flat_map(|column| &column.options)
                .filter_map(|option| match &option.option {
                    sqlparser::ast::ColumnOption::Default(_) => option.name.as_ref(),
                    sqlparser::ast::ColumnOption::Check(check) => {
                        option.name.as_ref().or(check.name.as_ref())
                    }
                    _ => None,
                })
                .chain(
                    table
                        .constraints
                        .iter()
                        .filter_map(|constraint| match constraint {
                            sqlparser::ast::TableConstraint::Check(check) => check.name.as_ref(),
                            _ => None,
                        }),
                );
            for constraint in constraints {
                let normalized: String =
                    session
                        .db
                        .query_row("SELECT lower(?)", [&constraint.value], |r| r.get(0))?;
                if !declared.insert(normalized)
                    || crate::object_catalog::key_name_exists(
                        &session.db,
                        schema,
                        &constraint.value,
                    )?
                {
                    return Err(crate::engine::StatementErrors(vec![
                        msduck_core::diagnostic::SqlError::new(
                            2714,
                            5,
                            format!(
                                "There is already an object named '{}' in the database.",
                                constraint.value
                            ),
                        ),
                        msduck_core::diagnostic::SqlError::new(
                            1750,
                            1,
                            "Could not create constraint or index. See previous errors.",
                        ),
                    ])
                    .into());
                }
            }
        }
        let exists: bool = session.db.query_row(
            "SELECT count(*)>0 FROM sys.objects o JOIN main.__msduck_schemas s USING(schema_id) WHERE lower(s.name)=lower(?) AND lower(o.name)=lower(?) AND rtrim(o.type) IN ('D','PK','UQ','C')",
            [schema,name], |r| r.get(0)
        )?;
        if exists {
            anyhow::bail!(msduck_core::diagnostic::SqlError::new(
                2714,
                6,
                format!("There is already an object named '{name}' in the database.")
            ));
        }
        Ok(None)
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
             CREATE OR REPLACE VIEW sys.default_constraints AS
               SELECT o.*, d.column_id AS parent_column_id,
                 d.definition, false AS is_system_named
               FROM sys.objects o JOIN main.__msduck_default_constraints d USING(object_id);
             CREATE OR REPLACE VIEW sys.key_constraints AS
               SELECT o.*, CAST(NULL AS INTEGER) AS unique_index_id,
                 k.is_system_named, true AS is_enforced
               FROM sys.objects o JOIN main.__msduck_key_objects k USING(object_id);
             CREATE OR REPLACE VIEW sys.check_constraints AS
               SELECT o.*,false AS is_disabled,false AS is_not_for_replication,
                 false AS is_not_trusted,k.parent_column_id,k.definition,
                 CASE WHEN k.definition IS NOT NULL THEN true ELSE CAST(NULL AS BOOLEAN) END AS uses_database_collation,
                 false AS is_system_named
               FROM sys.objects o JOIN main.__msduck_check_objects k USING(object_id);
             CREATE OR REPLACE VIEW sys.computed_columns AS
               SELECT c.object_id,c.name,c.column_id,c.system_type_id,c.user_type_id,c.max_length,
                 c.precision,c.scale,c.collation_name,d.is_nullable,c.is_ansi_padded,
                 c.is_rowguidcol,c.is_identity,c.is_filestream,c.is_replicated,
                 c.is_non_sql_subscribed,c.is_merge_published,c.is_dts_replicated,
                 c.is_xml_document,c.xml_collection_id,c.default_object_id,c.rule_object_id,
                 d.definition,CASE WHEN d.definition IS NOT NULL THEN true ELSE CAST(NULL AS BOOLEAN) END AS uses_database_collation,
                 cc.is_persisted,c.is_computed,c.is_sparse,c.is_column_set,
                 c.generated_always_type,c.generated_always_type_desc,c.encryption_type,
                 c.encryption_type_desc,c.encryption_algorithm_name,c.column_encryption_key_id,
                 c.column_encryption_key_database_name,c.is_hidden,c.is_masked,
                 c.graph_type,c.graph_type_desc,c.is_data_deletion_filter_column,
                 c.ledger_view_column_type,c.ledger_view_column_type_desc,
                 c.is_dropped_ledger_column,false AS is_index_column_expression
               FROM sys.columns c JOIN main.__msduck_computed_columns cc
                 ON cc.object_id=c.object_id AND cc.name_key=lower(c.name)
               LEFT JOIN main.__msduck_computed_definitions d
                 ON d.object_id=c.object_id AND d.column_id=c.column_id;
             CREATE OR REPLACE MACRO main.__msduck_object_definition(value) AS
               map_extract_value((SELECT map(list(object_id),list(definition))
                 FROM (SELECT object_id,definition FROM main.__msduck_modules
                   UNION ALL SELECT object_id,definition
                   FROM main.__msduck_default_constraints
                   UNION ALL SELECT object_id,definition FROM main.__msduck_check_objects)), value);",
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
