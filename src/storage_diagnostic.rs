//! Target-aware storage conversion. Context is an explicit argument, never global.
use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, bytes, kind, vector_units};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
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
struct Store<const UNICODE: bool, const FIXED: bool>;
impl<const UNICODE: bool, const FIXED: bool> VScalar for Store<UNICODE, FIXED> {
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
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
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
                    return Err(encoded_error(
                        target,
                        &units,
                        std::str::from_utf8(&table)?,
                        std::str::from_utf8(&column)?,
                    )?
                    .into());
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
        if UNICODE {
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
            if UNICODE { kind() } else { Id::Varchar.into() },
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Store<false, false>>("__msduck_store_context_varchar")?;
    db.register_scalar_function::<Store<false, true>>("__msduck_store_context_char")?;
    db.register_scalar_function::<Store<true, false>>("__msduck_store_context_nvarchar")?;
    db.register_scalar_function::<Store<true, true>>("__msduck_store_context_nchar")
}
