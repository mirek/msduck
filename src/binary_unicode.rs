//! Bounded binary-to-Unicode native adapters; byte rules live in msduck-core.
use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, bytes, kind};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{
    binary_unicode::{Error, convert},
    character::{CharacterType, Family, Length},
};

struct Convert<const FIXED: bool, const TRY: bool>;
impl<const FIXED: bool, const TRY: bool> VScalar for Convert<FIXED, TRY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let widths = input.flat_vector(1);
        let styles = input.flat_vector(2);
        let source_types = input.flat_vector(3);
        let mut remaining = CHUNK_LIMIT;
        let mut staged = Vec::with_capacity(len);
        for row in 0..len {
            if source.row_is_null(row as u64)
                || widths.row_is_null(row as u64)
                || styles.row_is_null(row as u64)
            {
                staged.push(None);
                continue;
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let style = unsafe { styles.as_slice_with_len::<i32>(len)[row] };
            let target = CharacterType::new(
                if FIXED {
                    Family::Nchar
                } else {
                    Family::Nvarchar
                },
                if width == -1 {
                    Length::Max
                } else {
                    Length::Bounded(width.try_into().map_err(|_| "invalid Unicode width")?)
                },
            )?;
            let source_id = if source_types.row_is_null(row as u64) {
                return Err("missing binary source declaration".into());
            } else {
                unsafe { source_types.as_slice_with_len::<i32>(len)[row] }
            };
            if !matches!(source_id, 165 | 173) {
                return Err("invalid binary source declaration".into());
            }
            let raw = bytes(&source, row, len)?;
            let units = match convert(&raw, target, style, remaining.min(CELL_LIMIT) / 2) {
                Ok(units) => units,
                Err(Error::UnsupportedStyle(_)) if TRY => {
                    staged.push(None);
                    continue;
                }
                Err(Error::UnsupportedStyle(style)) => {
                    return Err(format!(
                        "__msduck_binary_unicode_style:{style}:{source_id}:{}",
                        if FIXED { 239 } else { 231 }
                    )
                    .into());
                }
                Err(e) => return Err(e.into()),
            };
            let encoded: Vec<_> = units.into_iter().flat_map(u16::to_le_bytes).collect();
            remaining -= encoded.len();
            staged.push(Some(encoded.into_boxed_slice()));
        }
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
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                Id::Blob.into(),
                Id::Integer.into(),
                Id::Integer.into(),
                Id::Integer.into(),
            ],
            kind(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Convert<false, false>>("__msduck_binary_nvarchar")?;
    db.register_scalar_function::<Convert<true, false>>("__msduck_binary_nchar")?;
    db.register_scalar_function::<Convert<false, true>>("__msduck_try_binary_nvarchar")?;
    db.register_scalar_function::<Convert<true, true>>("__msduck_try_binary_nchar")
}

/// Decode only the native adapter's structured error, preserving SQL identity.
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let payload = message.strip_prefix("Invalid Input Error: __msduck_binary_unicode_style:")?;
    let mut parts = payload.split(':');
    let style = parts.next()?.parse::<i32>().ok()?;
    if (0..=2).contains(&style) {
        return None;
    }
    let source = match parts.next()? {
        "165" => "varbinary",
        "173" => "binary",
        _ => return None,
    };
    let target = match parts.next()? {
        "231" => "nvarchar",
        "239" => "nchar",
        _ => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(msduck_core::diagnostic::SqlError::new(
        9809,
        1,
        format!("The style {style} is not supported for conversions from {source} to {target}."),
    ))
}
