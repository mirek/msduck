//! Target-aware storage conversion. Context is an explicit argument, never global.
use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, bytes, kind, vector_units};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::character::{CharacterType, ConvertedUnicode, Error, Family, Length};
use sqlparser::ast::*;

/// Decorate the target conversion only; unrelated source expressions keep their
/// own context. The logical database is supplied by the caller.
pub fn contextualize(
    mut value: Expr,
    database: &str,
    schema: &str,
    table: &str,
    column: &str,
) -> Expr {
    if let Expr::Function(f) = &mut value {
        let name = match f.name.to_string().as_str() {
            "__msduck_store_carrier_varchar" => "__msduck_store_context_varchar",
            "__msduck_store_carrier_char" => "__msduck_store_context_char",
            "__msduck_store_carrier_nvarchar" => "__msduck_store_context_nvarchar",
            "__msduck_store_carrier_nchar" => "__msduck_store_context_nchar",
            _ => return value,
        };
        if let FunctionArguments::List(args) = &mut f.args {
            f.name = ObjectName::from(vec![Ident::new(name)]);
            for text in [format!("{database}.{schema}.{table}"), column.to_owned()] {
                args.args
                    .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                        Value::SingleQuotedString(text).into(),
                    ))));
            }
        }
    }
    value
}
const PREFIX: &str = "__msduck_truncated_utf16:";
fn encoded_error(
    target: CharacterType,
    source: &[u16],
    table: &str,
    column: &str,
) -> Result<String, Error> {
    let preview = match target.cast_utf16(source)? {
        ConvertedUnicode::Unicode(units) => units,
        ConvertedUnicode::Ansi(text) => text.encode_utf16().collect(),
    };
    let mut message:Vec<u16>=format!("String or binary data would be truncated in table '{table}', column '{column}'. Truncated value: '").encode_utf16().collect();
    message.extend(preview.into_iter().take(100));
    message.extend("'.".encode_utf16());
    let mut encoded = String::from(PREFIX);
    use std::fmt::Write;
    for unit in message {
        write!(encoded, "{unit:04x}").expect("String write");
    }
    Ok(encoded)
}
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let hex = message
        .strip_prefix("Invalid Input Error: ")?
        .strip_prefix(PREFIX)?;
    if hex.len() > 16_384 || hex.len() % 4 != 0 {
        return None;
    }
    let units = hex
        .as_bytes()
        .chunks_exact(4)
        .map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    Some(msduck_core::diagnostic::SqlError::from_utf16(
        2628, 1, 16, units,
    ))
}
// CHECK returns a materializable value/error pair. Expected SQL truncation is
// data, so rejecting a statement in the Rust shell cannot poison DuckDB's txn.
struct Store<const UNICODE: bool, const FIXED: bool, const CHECK: bool = false>;
impl<const UNICODE: bool, const FIXED: bool, const CHECK: bool> VScalar
    for Store<UNICODE, FIXED, CHECK>
{
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = input.struct_vector(0);
        let payload = structure.child(0, len);
        let widths = input.flat_vector(1);
        let tables = input.flat_vector(2);
        let columns = input.flat_vector(3);
        let mut staged = Vec::with_capacity(len);
        let mut errors = vec![None; len];
        let mut remaining = CHUNK_LIMIT;
        for (row, row_error) in errors.iter_mut().enumerate() {
            if source.row_is_null(row as u64) || widths.row_is_null(row as u64) {
                staged.push(None);
                continue;
            }
            if tables.row_is_null(row as u64) || columns.row_is_null(row as u64) {
                return Err("missing storage target context".into());
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let target = CharacterType::new(
                match (UNICODE, FIXED) {
                    (true, true) => Family::Nchar,
                    (true, false) => Family::Nvarchar,
                    (false, true) => Family::Char,
                    (false, false) => Family::Varchar,
                },
                if width == -1 {
                    Length::Max
                } else {
                    Length::Bounded(width.try_into()?)
                },
            )?;
            let units = vector_units(&source, &payload, row, len)?.expect("checked validity");
            let value = match target.store_utf16(&units) {
                Ok(value) => value,
                Err(Error::Truncated) => {
                    let table = bytes(&tables, row, len)?;
                    let column = bytes(&columns, row, len)?;
                    if table.len() > 1536 || column.len() > 512 {
                        return Err("invalid storage target context length".into());
                    }
                    let error = encoded_error(
                        target,
                        &units,
                        std::str::from_utf8(&table)?,
                        std::str::from_utf8(&column)?,
                    )?;
                    if !CHECK {
                        return Err(error.into());
                    }
                    if error.len() > remaining {
                        return Err("storage diagnostic output exceeds configured limit".into());
                    }
                    remaining -= error.len();
                    *row_error = Some(error);
                    staged.push(None);
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let value = match value {
                ConvertedUnicode::Unicode(units) => units
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
                ConvertedUnicode::Ansi(text) => text.into_bytes(),
            };
            if value.len() > CELL_LIMIT || value.len() > remaining {
                return Err("storage output exceeds configured limit".into());
            }
            remaining -= value.len();
            staged.push(Some(value.into_boxed_slice()));
        }
        if CHECK {
            let result = output.struct_vector();
            let mut values = result.child(0, len);
            let mut diagnostics = result.child(1, len);
            for (row, (value, error)) in staged.into_iter().zip(errors).enumerate() {
                if let Some(value) = value {
                    values.insert(row, value.as_ref());
                } else {
                    values.set_null(row);
                }
                if let Some(error) = error {
                    diagnostics.insert(row, error.as_str());
                } else {
                    diagnostics.set_null(row);
                }
            }
        } else if UNICODE {
            let mut result = output.struct_vector();
            let mut data = result.child(0, len);
            for (row, value) in staged.into_iter().enumerate() {
                if let Some(value) = value {
                    data.insert(row, value.as_ref());
                } else {
                    result.set_null(row);
                    data.set_null(row);
                }
            }
        } else {
            let mut result = output.flat_vector();
            for (row, value) in staged.into_iter().enumerate() {
                if let Some(value) = value {
                    result.insert(row, std::str::from_utf8(&value)?);
                } else {
                    result.set_null(row);
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                kind(),
                Id::Integer.into(),
                Id::Varchar.into(),
                Id::Varchar.into(),
            ],
            if CHECK {
                LogicalTypeHandle::struct_type(&[
                    ("value", Id::Blob.into()),
                    ("error", Id::Varchar.into()),
                ])
            } else if UNICODE {
                kind()
            } else {
                Id::Varchar.into()
            },
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Store<false, false>>("__msduck_store_context_varchar")?;
    db.register_scalar_function::<Store<false, true>>("__msduck_store_context_char")?;
    db.register_scalar_function::<Store<true, false>>("__msduck_store_context_nvarchar")?;
    db.register_scalar_function::<Store<true, true>>("__msduck_store_context_nchar")?;
    db.register_scalar_function::<Store<false, false, true>>("__msduck_check_store_varchar")?;
    db.register_scalar_function::<Store<false, true, true>>("__msduck_check_store_char")?;
    db.register_scalar_function::<Store<true, false, true>>("__msduck_check_store_nvarchar")?;
    db.register_scalar_function::<Store<true, true, true>>("__msduck_check_store_nchar")
}

/// Materialize ordinary INSERT inputs once before attempting the write. SQL
/// truncation is returned to Rust as data, leaving an explicit backend transaction
/// usable. Other write paths retain their own execution plans.
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
        let alias = Ident::with_quote('"', format!("__store_col_{index}"));
        if let Expr::Function(f) = value {
            let name = match f.name.to_string().as_str() {
                "__msduck_store_context_varchar" => Some(("__msduck_check_store_varchar", false)),
                "__msduck_store_context_char" => Some(("__msduck_check_store_char", false)),
                "__msduck_store_context_nvarchar" => Some(("__msduck_check_store_nvarchar", true)),
                "__msduck_store_context_nchar" => Some(("__msduck_check_store_nchar", true)),
                _ => None,
            };
            if let Some((name, unicode)) = name {
                f.name = ObjectName::from(vec![Ident::new(name)]);
                checked.push((index, unicode));
            }
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
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);
    let name = ObjectName::from(vec![
        Ident::new("temp"),
        Ident::new("main"),
        Ident::with_quote(
            '"',
            format!(
                "__msduck_checked_insert_{}",
                NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
            ),
        ),
    ]);
    db.execute(
        &format!("CREATE TEMP TABLE {name} AS {source}"),
        duckdb::params_from_iter(values.iter()),
    )?;
    struct Stage<'a>(&'a duckdb::Connection, ObjectName, bool);
    impl Drop for Stage<'_> {
        fn drop(&mut self) {
            if self.2 {
                let _ = self.0.execute_batch(&format!("DROP TABLE {}", self.1));
            }
        }
    }
    let mut stage = Stage(db, name, true);
    for &(index, _) in &checked {
        let alias = &aliases[index];
        let mut query = db.prepare(&format!(
            "SELECT {alias}.error FROM {} WHERE {alias}.error IS NOT NULL LIMIT 1",
            stage.1
        ))?;
        let mut rows = query.query([])?;
        if let Some(row) = rows.next()? {
            let encoded: String = row.get(0)?;
            let error = diagnostic(&format!("Invalid Input Error: {encoded}"))
                .ok_or_else(|| anyhow::anyhow!("invalid staged storage diagnostic"))?;
            return Err(error.into());
        }
    }
    let projection = aliases
        .iter()
        .enumerate()
        .map(
            |(index, alias)| match checked.iter().find(|(i, _)| *i == index) {
                Some((_, true)) => format!("__msduck_unicode_from_le({alias}.value)"),
                Some((_, false)) => format!("decode({alias}.value)"),
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
