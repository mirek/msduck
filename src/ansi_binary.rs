//! ANSI character-to-binary conversion uses SQL code-page bytes, not UTF-8.
use crate::unicode_carrier::{CHUNK_LIMIT, kind, vector_units};
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{
    character::{CharacterType, ConvertedUnicode, Family, Length},
    encoding::encode_cp1252,
};

struct Binary<const FIXED: bool>;
impl<const FIXED: bool> VScalar for Binary<FIXED> {
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
        let target = CharacterType::new(Family::Varchar, Length::Max)?;
        let mut remaining = CHUNK_LIMIT;
        let mut staged = Vec::with_capacity(len);
        for row in 0..len {
            if widths.row_is_null(row as u64) || source.row_is_null(row as u64) {
                staged.push(None);
                continue;
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            if !(1..=8000).contains(&width) && (FIXED || width != -1) {
                return Err("invalid binary conversion width".into());
            }
            let units =
                vector_units(&source, &payload, row, len)?.expect("checked parent validity");
            // Supported CP1252 conversions produce one byte per UTF16 unit.
            let selected = if width == -1 {
                units.as_slice()
            } else {
                &units[..units.len().min(width as usize)]
            };
            let ConvertedUnicode::Ansi(text) = target.cast_utf16(selected)? else {
                unreachable!()
            };
            let mut bytes = encode_cp1252(&text)?;
            if FIXED {
                bytes.resize(width as usize, 0);
            }
            if bytes.len() > remaining {
                return Err("ANSI binary output exceeds the configured chunk limit".into());
            }
            remaining -= bytes.len();
            staged.push(Some(bytes.into_boxed_slice()));
        }
        let mut result = output.flat_vector();
        for (row, value) in staged.into_iter().enumerate() {
            if let Some(value) = value {
                result.insert(row, value.as_ref());
            } else {
                result.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind(), LogicalTypeId::Integer.into()],
            LogicalTypeId::Blob.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Binary<false>>("__msduck_ansi_varbinary")?;
    db.register_scalar_function::<Binary<true>>("__msduck_ansi_binary")
}
