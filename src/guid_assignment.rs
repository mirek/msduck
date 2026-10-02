//! Root GUID conversion adapter. Registration is explicit and connection-owned.
use duckdb::{
    Connection,
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::types::uniqueidentifier::{parse_nvarchar, parse_varchar};

const ERROR_PREFIX: &str = "__msduck_guid_character_conversion:";
const CONVERSION: &str =
    "Conversion failed when converting from a character string to uniqueidentifier.";

/// Recognize only this adapter's canonical native error envelope. Ordinary
/// backend failures and application diagnostics retain their own identities.
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    checked_diagnostic(message.strip_prefix("Invalid Input Error: ")?)
}

/// Decode an error cell from this adapter's checked, materialized result.
/// The shell must validate the producing plan before using this on row data.
pub fn checked_diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let message = message.strip_prefix(ERROR_PREFIX)?;
    (message == CONVERSION).then(|| msduck_core::diagnostic::SqlError::new(8169, 2, CONVERSION))
}

/// Register the storage conversion without changing existing assignment plans.
/// The caller also owns diagnostic translation and transaction error handling.
pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<CharacterGuid>("__msduck_guid_character_text")?;
    db.register_scalar_function::<CharacterGuid<true>>("__msduck_guid_character_check")?;
    // typeof is a binding-time property: only the selected branch evaluates v.
    // DuckDB owns native UUID representation; the adapter emits canonical text
    // from the core's mixed-endian bytes instead of assuming native slot layout.
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_guid_assignment(v) AS CASE
         WHEN typeof(v)='UUID' THEN CAST(v AS UUID)
         ELSE CAST(__msduck_guid_character_text(v) AS UUID) END;
         CREATE OR REPLACE MACRO main.__msduck_guid_assignment_check(v) AS CASE
         WHEN typeof(v)='UUID' THEN struct_pack(value:=CAST(v AS VARCHAR),error:=CAST(NULL AS VARCHAR))
         ELSE __msduck_guid_character_check(v) END",
    )
}

struct CharacterGuid<const CHECK: bool = false>;
impl<const CHECK: bool> VScalar for CharacterGuid<CHECK> {
    type State = ();
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            if CHECK {
                LogicalTypeHandle::struct_type(&[
                    ("value", Id::Varchar.into()),
                    ("error", Id::Varchar.into()),
                ])
            } else {
                Id::Varchar.into()
            },
        )]
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let unicode = crate::unicode_carrier::is_logical(&logical);
        if !unicode && !matches!(logical.id(), Id::Varchar | Id::SqlNull) {
            return Err("unsupported source type for GUID storage assignment".into());
        }
        let structure = unicode.then(|| input.struct_vector(0));
        let payload = structure.as_ref().map(|vector| vector.child(0, len));
        let mut staged = Vec::with_capacity(len);
        let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
        for row in 0..len {
            if source.row_is_null(row as u64) {
                staged.push((None, None));
                continue;
            }
            let bytes = if let Some(payload) = &payload {
                let units = crate::unicode_carrier::vector_units(&source, payload, row, len)?
                    .ok_or("missing non-NULL GUID Unicode source")?;
                parse_nvarchar(&units)
            } else {
                // GUID syntax is ASCII. UTF-8 and CP1252 agree in the accepted
                // prefix; any non-ASCII prefix fails and suffixes are ignored.
                let text = crate::unicode_carrier::bytes(&source, row, len)?;
                parse_varchar(&text)
            };
            let (value, error) = match bytes {
                Ok(bytes) => (Some(uuid::Uuid::from_bytes_le(bytes).to_string()), None),
                Err(error) if CHECK => (None, Some(format!("{ERROR_PREFIX}{error}"))),
                Err(error) => return Err(format!("{ERROR_PREFIX}{error}").into()),
            };
            let size =
                value.as_ref().map_or(0, String::len) + error.as_ref().map_or(0, String::len);
            remaining = remaining
                .checked_sub(size)
                .ok_or("GUID conversion output exceeds configured chunk limit")?;
            staged.push((value, error));
        }
        if CHECK {
            let result = output.struct_vector();
            let mut values = result.child(0, len);
            let mut diagnostics = result.child(1, len);
            for (row, (value, error)) in staged.into_iter().enumerate() {
                if let Some(value) = value {
                    values.insert(row, value.as_str());
                } else {
                    values.set_null(row);
                }
                if let Some(error) = error {
                    diagnostics.insert(row, error.as_str());
                } else {
                    diagnostics.set_null(row);
                }
            }
        } else {
            let mut target = output.flat_vector();
            for (row, (value, _)) in staged.into_iter().enumerate() {
                if let Some(value) = value {
                    target.insert(row, value.as_str());
                } else {
                    target.set_null(row);
                }
            }
        }
        Ok(())
    }
}

use sqlparser::ast::*;

#[derive(Clone, Copy)]
enum Stored {
    Guid,
    Ansi,
    Unicode,
}

fn checked_conversion(value: &mut Expr) -> Option<Stored> {
    let Expr::Function(function) = value else {
        return None;
    };
    let (name, kind) = match function.name.to_string().as_str() {
        "__msduck_guid_assignment" => ("__msduck_guid_assignment_check", Stored::Guid),
        "__msduck_store_context_varchar" => ("__msduck_check_store_varchar", Stored::Ansi),
        "__msduck_store_context_char" => ("__msduck_check_store_char", Stored::Ansi),
        "__msduck_store_context_nvarchar" => ("__msduck_check_store_nvarchar", Stored::Unicode),
        "__msduck_store_context_nchar" => ("__msduck_check_store_nchar", Stored::Unicode),
        _ => return None,
    };
    function.name = ObjectName::from(vec![Ident::new(name)]);
    Some(kind)
}

fn restore_value(value: &str, kind: Stored) -> String {
    match kind {
        Stored::Guid => format!("CAST({value}.value AS UUID)"),
        Stored::Ansi => format!("decode({value}.value)"),
        Stored::Unicode => format!("__msduck_unicode_from_le({value}.value)"),
    }
}

/// GUID-containing DML stages all target conversions before effects. Character
/// targets share the existing checked adapter so mixed assignments cannot throw
/// expected conversion errors while building this stage. GUID-free statements
/// remain owned by the ordinary storage path.
pub fn checked_insert(
    db: &duckdb::Connection,
    statement: &Statement,
    values: &[duckdb::types::Value],
) -> anyhow::Result<Option<u64>> {
    let Statement::Insert(original) = statement else {
        return Ok(None);
    };
    if original.returning.is_some() || original.on.is_some() {
        return Ok(None);
    }
    let Some(source) = &original.source else {
        return Ok(None);
    };
    let mut source = source.clone();
    let SetExpr::Select(select) = source.body.as_mut() else {
        return Ok(None);
    };
    let mut checked = Vec::new();
    let mut aliases = Vec::new();
    for (index, item) in select.projection.iter_mut().enumerate() {
        let value = match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => expr,
            _ => return Ok(None),
        };
        let alias = Ident::with_quote('"', format!("__guid_store_col_{index}"));
        if let Some(kind) = checked_conversion(value) {
            checked.push((index, kind));
        }
        *item = SelectItem::ExprWithAlias {
            expr: value.clone(),
            alias: alias.clone(),
        };
        aliases.push(alias);
    }
    if checked.is_empty() {
        return Ok(None);
    }
    if !checked.iter().any(|(_, kind)| matches!(kind, Stored::Guid)) {
        return Ok(None);
    }
    let mut stage = stage(db, &source, values, &checked, &aliases)?;
    let projection = aliases
        .iter()
        .enumerate()
        .map(
            |(index, alias)| match checked.iter().find(|(i, _)| *i == index) {
                Some((_, kind)) => restore_value(&alias.to_string(), *kind),
                None => alias.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join(",");
    let mut parsed = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::DuckDbDialect {},
        &format!("SELECT {projection} FROM {}", stage.1),
    )?;
    let Statement::Query(query) = parsed.remove(0) else {
        unreachable!("generated query");
    };
    let mut write = original.clone();
    write.source = Some(query);
    let count = db.execute(&Statement::Insert(write).to_string(), [])? as u64;
    // On success, surface cleanup failures. On a write failure the enclosing
    // transaction may already be aborted; rollback removes its temporary table.
    db.execute_batch(&format!("DROP TABLE {}", stage.1))?;
    stage.2 = false;
    Ok(Some(count))
}

struct Stage<'a>(&'a duckdb::Connection, ObjectName, bool);
impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if self.2 {
            let _ = self.0.execute_batch(&format!("DROP TABLE {}", self.1));
        }
    }
}

fn stage<'a>(
    db: &'a duckdb::Connection,
    source: &Query,
    values: &[duckdb::types::Value],
    checked: &[(usize, Stored)],
    aliases: &[Ident],
) -> anyhow::Result<Stage<'a>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);
    let name = ObjectName::from(vec![
        Ident::new("temp"),
        Ident::new("main"),
        Ident::with_quote(
            '"',
            format!(
                "__msduck_checked_guid_assignment_{}",
                NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
            ),
        ),
    ]);
    db.execute(
        &format!("CREATE TEMP TABLE {name} AS {source}"),
        duckdb::params_from_iter(values.iter()),
    )?;
    let stage = Stage(db, name, true);
    for &(index, kind) in checked {
        let alias = &aliases[index];
        let mut query = db.prepare(&format!(
            "SELECT {alias}.error FROM {} WHERE {alias}.error IS NOT NULL LIMIT 1",
            stage.1
        ))?;
        let mut rows = query.query([])?;
        if let Some(row) = rows.next()? {
            let encoded: String = row.get(0)?;
            let error = match kind {
                Stored::Guid => checked_diagnostic(&encoded),
                Stored::Ansi | Stored::Unicode => crate::storage_diagnostic::diagnostic(&format!(
                    "Invalid Input Error: {encoded}"
                )),
            }
            .ok_or_else(|| anyhow::anyhow!("invalid staged assignment diagnostic"))?;
            return Err(error.into());
        }
    }
    Ok(stage)
}

/// Capture selected physical rows and all assignments together. The final write
/// uses only row IDs and captured values; neither predicates nor assignments run
/// a second time. Joined/CTE/OUTPUT plans require their own identity proofs.
pub fn checked_update(
    db: &duckdb::Connection,
    statement: &Statement,
    values: &[duckdb::types::Value],
) -> anyhow::Result<Option<u64>> {
    let Statement::Update(original) = statement else {
        return Ok(None);
    };
    if original.or.is_some()
        || !original.optimizer_hints.is_empty()
        || original.from.is_some()
        || original.returning.is_some()
        || original.output.is_some()
        || !original.table.joins.is_empty()
        || original.limit.is_some()
        || !original.order_by.is_empty()
    {
        return Ok(None);
    }
    let TableFactor::Table { name, alias, .. } = &original.table.relation else {
        return Ok(None);
    };
    if alias.as_ref().is_some_and(|a| !a.columns.is_empty()) {
        return Ok(None);
    }
    let parts = name
        .0
        .iter()
        .map(|p| p.as_ident().map(|i| i.value.as_str()))
        .collect::<Option<Vec<_>>>();
    let Some(parts) = parts else {
        return Ok(None);
    };
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => return Ok(None),
    };
    // A user column named rowid shadows DuckDB's physical identity. Never use it
    // as though it were a hidden unique row identifier.
    let shadowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE AND column_name='rowid' COLLATE NOCASE)",[schema,table],|r|r.get(0))?;
    if shadowed {
        return Ok(None);
    }
    let qualifier = alias
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_else(|| Ident::with_quote('"', table));
    let rowid = Expr::CompoundIdentifier(vec![qualifier, Ident::new("rowid")]);
    let mut checked = Vec::new();
    let mut aliases = Vec::new();
    let mut projection = Vec::new();
    for (index, assignment) in original.assignments.iter().enumerate() {
        if !matches!(assignment.target, AssignmentTarget::ColumnName(_)) {
            return Ok(None);
        }
        let mut value = assignment.value.clone();
        if let Some(kind) = checked_conversion(&mut value) {
            checked.push((index, kind));
        }
        let alias = Ident::with_quote('"', format!("__guid_store_col_{index}"));
        projection.push(SelectItem::ExprWithAlias {
            expr: value,
            alias: alias.clone(),
        });
        aliases.push(alias);
    }
    if !checked.iter().any(|(_, kind)| matches!(kind, Stored::Guid)) {
        return Ok(None);
    }
    projection.push(SelectItem::ExprWithAlias {
        expr: rowid.clone(),
        alias: Ident::new("__target_rowid"),
    });
    let mut parsed =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, "SELECT 1")?;
    let Statement::Query(mut source) = parsed.remove(0) else {
        unreachable!();
    };
    let SetExpr::Select(select) = source.body.as_mut() else {
        unreachable!();
    };
    select.projection = projection;
    select.from = vec![original.table.clone()];
    select.selection = original.selection.clone();
    let mut stage = stage(db, &source, values, &checked, &aliases)?;
    let stage_qualifier = stage.1.0.last().and_then(|p| p.as_ident()).unwrap().clone();
    let assignments = original
        .assignments
        .iter()
        .enumerate()
        .map(|(index, assignment)| {
            let value = format!("{stage_qualifier}.{}", aliases[index]);
            let value = match checked.iter().find(|(i, _)| *i == index) {
                Some((_, kind)) => restore_value(&value, *kind),
                None => value,
            };
            format!("{}={value}", assignment.target)
        })
        .collect::<Vec<_>>()
        .join(",");
    let count = db.execute(
        &format!(
            "UPDATE {} SET {assignments} FROM {} WHERE {rowid}={stage_qualifier}.__target_rowid",
            original.table, stage.1
        ),
        [],
    )? as u64;
    db.execute_batch(&format!("DROP TABLE {}", stage.1))?;
    stage.2 = false;
    Ok(Some(count))
}

/// Validate character-to-GUID column changes before DDL effects. SQL Server
/// ALTER COLUMN uses the existing column as its operand, so this path contains
/// no caller-supplied volatile USING expression. The enclosing DDL executor
/// owns transaction atomicity; a rejected conversion remains a typed SQL error.
pub fn validate_alter(db: &Connection, table: &AlterTable) -> anyhow::Result<()> {
    crate::table_alter::validate(table)?;
    for operation in &table.operations {
        let AlterTableOperation::AlterColumn {
            column_name,
            op:
                AlterColumnOperation::SetDataType {
                    data_type: DataType::Uuid,
                    using,
                    ..
                },
        } = operation
        else {
            continue;
        };
        anyhow::ensure!(
            using.is_none(),
            "unsupported explicit GUID ALTER USING expression"
        );
        let mut statement = db.prepare(&format!(
            "WITH converted AS MATERIALIZED (SELECT __msduck_guid_assignment_check({column_name}) AS g FROM {}) SELECT g.error FROM converted WHERE g.error IS NOT NULL LIMIT 1", table.name
        ))?;
        let mut rows = statement.query([])?;
        if let Some(row) = rows.next()? {
            let encoded: String = row.get(0)?;
            return Err(checked_diagnostic(&encoded)
                .ok_or_else(|| anyhow::anyhow!("invalid GUID ALTER diagnostic"))?
                .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const GUID: &str = "00112233-4455-6677-8899-aabbccddeeff";

    #[test]
    fn checked_failure_keeps_prior_transaction_writes_readable_until_shell_rollback() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE TABLE prior_write(id INT); BEGIN TRANSACTION; INSERT INTO prior_write VALUES(20)").unwrap();
        let (value, error): (Option<String>, Option<String>) = db.query_row(
            "WITH staged AS MATERIALIZED (SELECT __msduck_guid_assignment_check(?) AS g) SELECT g.value,g.error FROM staged",
            ["bad"], |row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert_eq!(value, None);
        assert_eq!(
            checked_diagnostic(&error.unwrap()),
            Some(parse_varchar(b"bad").unwrap_err())
        );
        let prior: i32 = db
            .query_row("SELECT id FROM prior_write", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            prior, 20,
            "checked SQL errors must not poison native transaction reads"
        );
        db.execute_batch("ROLLBACK").unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM prior_write", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn checked_null_and_uuid_passthrough_keep_fixed_fields() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for source in [
            "CAST(NULL AS VARCHAR)",
            "CAST(NULL AS UUID)",
            "CAST(NULL AS STRUCT(__msduck_utf16le BLOB))",
        ] {
            let actual: (Option<String>, Option<String>) = db.query_row(
                &format!("SELECT g.value,g.error FROM(SELECT __msduck_guid_assignment_check({source}) AS g) s"),
                [], |row| Ok((row.get(0)?,row.get(1)?)),
            ).unwrap();
            assert_eq!(actual, (None, None));
        }
        let actual: (String, Option<String>) = db.query_row(
            "SELECT g.value,g.error FROM(SELECT __msduck_guid_assignment_check(CAST(? AS UUID)) AS g) s",
            [GUID], |row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert_eq!(actual, (GUID.into(), None));
        let encoded: Vec<u8> = format!("{{{GUID}}}")
            .encode_utf16()
            .chain([0xd800])
            .flat_map(u16::to_le_bytes)
            .collect();
        let actual: (String, Option<String>) = db.query_row(
            "SELECT g.value,g.error FROM(SELECT __msduck_guid_assignment_check(struct_pack(__msduck_utf16le:=?)) AS g) s",
            [encoded], |row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert_eq!(actual, (GUID.into(), None));
    }

    #[test]
    fn checked_values_and_errors_share_one_materialized_evaluation_across_chunks() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE checked_guid_calls START 1")
            .unwrap();
        let counts: (i64,i64) = db.query_row(
            "WITH staged AS MATERIALIZED (SELECT __msduck_guid_assignment_check(CASE WHEN nextval('checked_guid_calls')%17=0 THEN 'bad' ELSE '00112233-4455-6677-8899-aabbccddeeffEXTRA' END) AS g FROM range(6000)) SELECT count(g.value),count(g.error) FROM staged",
            [], |row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert_eq!(counts, (6000 - 6000 / 17, 6000 / 17));
        let calls: i64 = db
            .query_row("SELECT currval('checked_guid_calls')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }

    #[test]
    fn native_error_envelope_preserves_core_identity_without_reclassifying_other_errors() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let error = db
            .query_row::<String, _, _>(
                "SELECT CAST(__msduck_guid_assignment(?) AS VARCHAR)",
                ["bad"],
                |row| row.get(0),
            )
            .unwrap_err();
        assert_eq!(
            diagnostic(&error.to_string()),
            Some(parse_varchar(b"bad").unwrap_err())
        );
        for message in [
            CONVERSION.to_owned(),
            format!("Invalid Input Error: {CONVERSION}"),
            format!("Invalid Input Error: {ERROR_PREFIX}{CONVERSION} extra"),
            format!("other error: {ERROR_PREFIX}{CONVERSION}"),
        ] {
            assert_eq!(diagnostic(&message), None);
        }
        let invalid_carrier = db
            .query_row::<String, _, _>(
                "SELECT CAST(__msduck_guid_assignment(struct_pack(__msduck_utf16le:=?)) AS VARCHAR)",
                [vec![0_u8]],
                |row| row.get(0),
            )
            .unwrap_err();
        assert_eq!(diagnostic(&invalid_carrier.to_string()), None);
    }

    #[test]
    fn characters_null_and_native_guid_have_exact_uuid_layout() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for text in [
            GUID.to_owned(),
            format!("{{{GUID}}}EXTRA"),
            format!("{GUID}EXTRA"),
        ] {
            let actual: String = db
                .query_row(
                    "SELECT CAST(__msduck_guid_assignment(?) AS VARCHAR)",
                    [text],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual, GUID);
        }
        let actual: String = db
            .query_row(
                "SELECT CAST(__msduck_guid_assignment(CAST(? AS UUID)) AS VARCHAR)",
                [GUID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, GUID);
        let absent: Option<String> = db
            .query_row(
                "SELECT CAST(__msduck_guid_assignment(CAST(NULL AS VARCHAR)) AS VARCHAR)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(absent, None);
    }

    #[test]
    fn raw_unicode_suffix_surrogates_are_preserved_until_core_conversion() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let encoded: Vec<u8> = format!("{{{GUID}}}")
            .encode_utf16()
            .chain([0xd800])
            .flat_map(u16::to_le_bytes)
            .collect();
        let actual: String = db.query_row(
            "SELECT CAST(__msduck_guid_assignment(struct_pack(__msduck_utf16le:=?)) AS VARCHAR)",
            [encoded], |row| row.get(0),
        ).unwrap();
        assert_eq!(actual, GUID);
        let invalid = db.query_row::<String, _, _>(
            "SELECT CAST(__msduck_guid_assignment(struct_pack(__msduck_utf16le:=?)) AS VARCHAR)",
            [vec![0, 0xd8]], |row| row.get(0),
        ).unwrap_err();
        assert!(invalid.to_string().contains(
            "Conversion failed when converting from a character string to uniqueidentifier."
        ));
    }

    #[test]
    fn native_conversion_evaluates_each_operand_once_across_chunks() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE guid_calls START 1")
            .unwrap();
        let count: i64 = db.query_row(
            "SELECT count(__msduck_guid_assignment('00112233-4455-6677-8899-aabbccddeeff'||CAST(nextval('guid_calls') AS VARCHAR))) FROM range(6000)",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 6000);
        let calls: i64 = db
            .query_row("SELECT currval('guid_calls')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }
}
