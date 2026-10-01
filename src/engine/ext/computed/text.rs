//! UTF-8 text from a stored Unicode carrier, for computed columns.
//!
//! Expressions bind `nvarchar` and `nchar` operands of derived rows as UTF-8
//! VARCHAR, while table columns store the UTF-16 carrier. A lowered
//! computed-column expression reads its carrier columns through
//! `__msduck_carrier_utf8`, and stores a Unicode result as UTF-8 text
//! through `__msduck_computed_text`, the layout views and derived tables use.
use crate::unicode_carrier::{CHUNK_LIMIT, bytes, kind};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub(super) const CARRIER_UTF8: &str = "__msduck_carrier_utf8";
pub(super) const COMPUTED_TEXT: &str = "__msduck_computed_text";

/// UTF-16LE carrier to UTF-8. An unpaired surrogate, which UTF-8 cannot
/// hold, becomes U+FFFD.
struct CarrierUtf8;
impl VScalar for CarrierUtf8 {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = input.struct_vector(0);
        let data = structure.child(0, len);
        let mut staged = Vec::with_capacity(len);
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            if source.row_is_null(row as u64) {
                staged.push(None);
                continue;
            }
            if data.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let encoded = bytes(&data, row, len)?;
            if encoded.len() % 2 != 0 {
                return Err("invalid Unicode carrier: odd byte length".into());
            }
            let text = String::from_utf16_lossy(
                &encoded
                    .chunks_exact(2)
                    .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                    .collect::<Vec<_>>(),
            );
            if text.len() > remaining {
                return Err("Unicode text output exceeds the configured chunk limit".into());
            }
            remaining -= text.len();
            staged.push(Some(text));
        }
        let mut result = output.flat_vector();
        for (row, text) in staged.into_iter().enumerate() {
            match text {
                Some(text) => result.insert(row, text.as_str()),
                None => result.set_null(row),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind()],
            Id::Varchar.into(),
        )]
    }
}

pub(super) fn register(db: &duckdb::Connection) -> anyhow::Result<()> {
    db.register_scalar_function::<CarrierUtf8>(CARRIER_UTF8)?;
    // typeof is a bind-time property, so only the selected branch evaluates.
    db.execute_batch(&format!(
        "CREATE OR REPLACE MACRO {COMPUTED_TEXT}(v) AS CASE WHEN typeof(v)='STRUCT(__msduck_utf16le BLOB)' THEN {CARRIER_UTF8}(CAST(v AS STRUCT(__msduck_utf16le BLOB))) ELSE CAST(v AS VARCHAR) END"
    ))?;
    Ok(())
}
