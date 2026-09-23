//! Native Unicode carrier. The named STRUCT distinguishes UTF-16LE text from
//! SQL binary values and survives materialization without UTF-8 conversion.
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::left_right::Side;

const CELL_LIMIT: usize = 16 * 1024 * 1024;
const CHUNK_LIMIT: usize = 64 * 1024 * 1024;
fn kind() -> LogicalTypeHandle {
    LogicalTypeHandle::struct_type(&[("__msduck_utf16le", Id::Blob.into())])
}

pub fn is_storage_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("STRUCT(__msduck_utf16le BLOB)")
}

// Copy while the local string_t is alive: inline bytes belong to that value,
// while long strings belong to DuckDB. Callers check validity before reading.
fn bytes(
    vector: &FlatVector<'_>,
    row: usize,
    len: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut value = unsafe { vector.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
    let size = unsafe { duckdb::ffi::duckdb_string_t_length(value) as usize };
    if size > CELL_LIMIT {
        return Err("Unicode input exceeds the configured cell limit".into());
    }
    Ok(unsafe {
        std::slice::from_raw_parts(
            duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
            size,
        )
    }
    .to_vec())
}

pub fn is_logical(kind: &LogicalTypeHandle) -> bool {
    kind.id() == Id::Struct
        && kind.num_children() == 1
        && kind.child_name(0) == "__msduck_utf16le"
        && kind.child(0).id() == Id::Blob
}

/// Only the named one-field binary struct is a Unicode carrier.
pub fn is_arrow(kind: &duckdb::arrow::datatypes::DataType) -> bool {
    use duckdb::arrow::datatypes::DataType;
    matches!(kind, DataType::Struct(fields) if fields.len() == 1
        && fields[0].name() == "__msduck_utf16le"
        && matches!(fields[0].data_type(), DataType::Binary | DataType::LargeBinary))
}

pub fn read(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
) -> anyhow::Result<duckdb::types::Value> {
    use duckdb::arrow::array::{Array, BinaryArray, LargeBinaryArray, StructArray};
    anyhow::ensure!(
        is_arrow(array.data_type()) && row < array.len(),
        "invalid Unicode carrier array"
    );
    if array.is_null(row) {
        return Ok(duckdb::types::Value::Null);
    }
    let values = array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| anyhow::anyhow!("invalid Unicode carrier struct"))?;
    let column = values.column(0);
    anyhow::ensure!(
        !column.is_null(row),
        "invalid Unicode carrier: NULL payload"
    );
    let bytes = if let Some(values) = column.as_any().downcast_ref::<BinaryArray>() {
        values.value(row)
    } else if let Some(values) = column.as_any().downcast_ref::<LargeBinaryArray>() {
        values.value(row)
    } else {
        anyhow::bail!("invalid Unicode carrier byte array");
    };
    anyhow::ensure!(
        bytes.len() <= CELL_LIMIT && bytes.len() % 2 == 0,
        "invalid Unicode carrier byte length"
    );
    Ok(duckdb::types::Value::Struct(
        vec![(
            "__msduck_utf16le".into(),
            duckdb::types::Value::Blob(bytes.to_vec()),
        )]
        .into(),
    ))
}

pub fn units(value: &duckdb::types::Value) -> anyhow::Result<Vec<u16>> {
    use duckdb::types::Value;
    let Value::Struct(fields) = value else {
        anyhow::bail!("invalid Unicode carrier value");
    };
    anyhow::ensure!(fields.keys().count() == 1, "invalid Unicode carrier fields");
    let Some(Value::Blob(bytes)) = fields.get(&"__msduck_utf16le".to_owned()) else {
        anyhow::bail!("invalid Unicode carrier payload");
    };
    anyhow::ensure!(
        bytes.len() <= CELL_LIMIT && bytes.len() % 2 == 0,
        "invalid Unicode carrier byte length"
    );
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    Ok(units)
}
pub fn encode(
    out: &mut Vec<u8>,
    kind: &crate::tds::Type,
    value: &duckdb::types::Value,
) -> anyhow::Result<()> {
    crate::tds::unicode_value(out, kind, Some(&units(value)?))
}

struct Pack<const RAW: bool = false>;
impl<const RAW: bool> VScalar for Pack<RAW> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = is_logical(&source.logical_type()).then(|| input.struct_vector(0));
        let stored = structure.as_ref().map(|s| s.child(0, len));
        let mut result = output.struct_vector();
        let mut data = result.child(0, len);
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                data.set_null(row);
                continue;
            }
            if let Some(stored) = &stored {
                if stored.row_is_null(row as u64) {
                    return Err("invalid Unicode carrier: NULL payload".into());
                }
                let encoded = bytes(stored, row, len)?;
                if encoded.len() % 2 != 0 {
                    return Err("invalid Unicode carrier: odd byte length".into());
                }
                if encoded.len() > remaining {
                    return Err("Unicode output exceeds the configured limit".into());
                }
                remaining -= encoded.len();
                data.insert(row, encoded.as_slice());
                continue;
            }
            let source = bytes(&source, row, len)?;
            if RAW {
                if source.len() % 2 != 0 {
                    return Err("invalid Unicode binding: odd byte length".into());
                }
                if source.len() > remaining {
                    return Err("Unicode binding exceeds the configured chunk limit".into());
                }
                remaining -= source.len();
                data.insert(row, source.as_slice());
                continue;
            }
            let text = std::str::from_utf8(&source)?;
            let size = text
                .encode_utf16()
                .count()
                .checked_mul(2)
                .ok_or("Unicode length overflow")?;
            if size > CELL_LIMIT || size > remaining {
                return Err("Unicode output exceeds the configured limit".into());
            }
            remaining -= size;
            let encoded: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            data.insert(row, encoded.as_slice());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        if RAW {
            return vec![ScalarFunctionSignature::exact(
                vec![Id::Blob.into()],
                kind(),
            )];
        }
        vec![
            ScalarFunctionSignature::exact(vec![Id::Varchar.into()], kind()),
            ScalarFunctionSignature::exact(vec![kind()], kind()),
        ]
    }
}

struct Slice<const RIGHT: bool, const TYPED: bool = false>;
impl<const RIGHT: bool, const TYPED: bool> VScalar for Slice<RIGHT, TYPED> {
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
        let counts = input.flat_vector(1);
        let mut result = output.struct_vector();
        let mut values = result.child(0, len);
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            if source.row_is_null(row as u64) || counts.row_is_null(row as u64) {
                result.set_null(row);
                values.set_null(row);
                continue;
            }
            if data.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let encoded = bytes(&data, row, len)?;
            if encoded.len() % 2 != 0 {
                return Err("invalid Unicode carrier: odd byte length".into());
            }
            let units: Vec<_> = encoded
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let count = unsafe { counts.as_slice_with_len::<i32>(len)[row] };
            let side = if RIGHT { Side::Right } else { Side::Left };
            let selected = side.utf16(&units, count)?;
            let size = selected.len() * 2;
            if size > remaining {
                return Err("Unicode output exceeds the configured chunk limit".into());
            }
            remaining -= size;
            let encoded: Vec<_> = selected.iter().flat_map(|u| u.to_le_bytes()).collect();
            values.insert(row, encoded.as_slice());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            if TYPED {
                vec![kind(), Id::Integer.into(), Id::Integer.into()]
            } else {
                vec![kind(), Id::Integer.into()]
            },
            kind(),
        )]
    }
}

struct Ansi<const RIGHT: bool, const BINARY: bool>;
impl<const RIGHT: bool, const BINARY: bool> VScalar for Ansi<RIGHT, BINARY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let counts = input.flat_vector(1);
        let mut result = output.flat_vector();
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            if source.row_is_null(row as u64) || counts.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let source = bytes(&source, row, len)?;
            let encoded = if BINARY {
                source
            } else {
                msduck_core::encoding::encode_cp1252(std::str::from_utf8(&source)?)?
            };
            let count = unsafe { counts.as_slice_with_len::<i32>(len)[row] };
            let side = if RIGHT { Side::Right } else { Side::Left };
            let selected = side.ansi(&encoded, count)?;
            let text = msduck_core::encoding::decode_cp1252(selected);
            if text.len() > CELL_LIMIT || text.len() > remaining {
                return Err("LEFT/RIGHT output exceeds the configured limit".into());
            }
            remaining -= text.len();
            result.insert(row, text.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                if BINARY {
                    Id::Blob.into()
                } else {
                    Id::Varchar.into()
                },
                Id::Integer.into(),
            ],
            Id::Varchar.into(),
        )]
    }
}

struct Cast<const UNICODE: bool, const FIXED: bool, const STORAGE: bool = false>;
impl<const UNICODE: bool, const FIXED: bool, const STORAGE: bool> VScalar
    for Cast<UNICODE, FIXED, STORAGE>
{
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use msduck_core::character::{CharacterType, ConvertedUnicode, Family, Length};
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = input.struct_vector(0);
        let data = structure.child(0, len);
        let widths = input.flat_vector(1);
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            if source.row_is_null(row as u64) || widths.row_is_null(row as u64) {
                if UNICODE {
                    let mut result = output.struct_vector();
                    result.set_null(row);
                    result.child(0, len).set_null(row);
                } else {
                    output.flat_vector().set_null(row);
                }
                continue;
            }
            if data.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let encoded = bytes(&data, row, len)?;
            if encoded.len() % 2 != 0 {
                return Err("invalid Unicode carrier: odd byte length".into());
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let family = match (UNICODE, FIXED) {
                (true, true) => Family::Nchar,
                (true, false) => Family::Nvarchar,
                (false, true) => Family::Char,
                (false, false) => Family::Varchar,
            };
            let target = CharacterType::new(
                family,
                if width == -1 {
                    Length::Max
                } else {
                    Length::Bounded(width.try_into()?)
                },
            )?;
            let units: Vec<_> = encoded
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let converted = if STORAGE {
                target.store_utf16(&units)?
            } else {
                target.cast_utf16(&units)?
            };
            match converted {
                ConvertedUnicode::Unicode(units) => {
                    let size = units.len() * 2;
                    if size > CELL_LIMIT || size > remaining {
                        return Err("Unicode cast output exceeds configured limit".into());
                    }
                    remaining -= size;
                    let encoded: Vec<_> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
                    output
                        .struct_vector()
                        .child(0, len)
                        .insert(row, encoded.as_slice());
                }
                ConvertedUnicode::Ansi(text) => {
                    if text.len() > CELL_LIMIT || text.len() > remaining {
                        return Err("Unicode cast output exceeds configured limit".into());
                    }
                    remaining -= text.len();
                    output.flat_vector().insert(row, text.as_str());
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind(), Id::Integer.into()],
            if UNICODE { kind() } else { Id::Varchar.into() },
        )]
    }
}

/// UNICODE uses the first code unit under the current non-SC semantics, even
/// when that unit is an isolated surrogate. Empty input produces SQL NULL.
struct FirstUnit;
impl VScalar for FirstUnit {
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
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            if data.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let encoded = bytes(&data, row, len)?;
            if encoded.len() % 2 != 0 {
                return Err("invalid Unicode carrier: odd byte length".into());
            }
            if encoded.is_empty() {
                result.set_null(row);
            } else {
                let unit = u16::from_le_bytes([encoded[0], encoded[1]]);
                unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[row] = i32::from(unit);
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind()],
            Id::Integer.into(),
        )]
    }
}

fn vector_units(
    source: &FlatVector<'_>,
    data: &FlatVector<'_>,
    row: usize,
    len: usize,
) -> Result<Option<Vec<u16>>, Box<dyn std::error::Error>> {
    if source.row_is_null(row as u64) {
        return Ok(None);
    }
    if data.row_is_null(row as u64) {
        return Err("invalid Unicode carrier: NULL payload".into());
    }
    let encoded = bytes(data, row, len)?;
    if encoded.len() % 2 != 0 {
        return Err("invalid Unicode carrier: odd byte length".into());
    }
    Ok(Some(
        encoded
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect(),
    ))
}

// Capacity is bound by the SQL planner: -1 means MAX, otherwise bytes/units.
// Each child is a single native argument; intermediate results stay materialized.
struct Concat<const UNICODE: bool>;
impl<const UNICODE: bool> VScalar for Concat<UNICODE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use msduck_core::character::Length;
        let len = input.len();
        let left = input.flat_vector(0);
        let right = input.flat_vector(1);
        let widths = input.flat_vector(2);
        let left_struct = UNICODE.then(|| input.struct_vector(0));
        let right_struct = UNICODE.then(|| input.struct_vector(1));
        let left_data = left_struct.as_ref().map(|s| s.child(0, len));
        let right_data = right_struct.as_ref().map(|s| s.child(0, len));
        // Only obtain the output representation selected by the signature.
        let mut remaining = CHUNK_LIMIT;
        let mut rows = Vec::with_capacity(len);
        for row in 0..len {
            if widths.row_is_null(row as u64) {
                return Err("concatenation capacity must be bound".into());
            }
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let capacity = match width {
                -1 => Length::Max,
                n if (0..=if UNICODE { 4000 } else { 8000 }).contains(&n) => {
                    Length::Bounded(n as u16)
                }
                _ => return Err("invalid concatenation capacity".into()),
            };
            if left.row_is_null(row as u64) || right.row_is_null(row as u64) {
                rows.push(None);
                continue;
            }
            let encoded = if UNICODE {
                let a = vector_units(&left, left_data.as_ref().unwrap(), row, len)?.unwrap();
                let b = vector_units(&right, right_data.as_ref().unwrap(), row, len)?.unwrap();
                let value =
                    msduck_core::concat::utf16(&a, &b, capacity, CELL_LIMIT.min(remaining) / 2)
                        .ok_or("concatenation output exceeds the configured limit")?;
                value
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            } else {
                let a = bytes(&left, row, len)?;
                let b = bytes(&right, row, len)?;
                let a = msduck_core::encoding::encode_cp1252(std::str::from_utf8(&a)?)?;
                let b = msduck_core::encoding::encode_cp1252(std::str::from_utf8(&b)?)?;
                let value = msduck_core::concat::ansi(&a, &b, capacity, CELL_LIMIT.min(remaining))
                    .ok_or("concatenation output exceeds the configured limit")?;
                msduck_core::encoding::decode_cp1252(&value).into_bytes()
            };
            if encoded.len() > CELL_LIMIT || encoded.len() > remaining {
                return Err("concatenation output exceeds the configured limit".into());
            }
            remaining -= encoded.len();
            rows.push(Some(encoded));
        }
        if UNICODE {
            let mut result = output.struct_vector();
            let mut data = result.child(0, len);
            for (row, value) in rows.iter().enumerate() {
                if let Some(value) = value {
                    data.insert(row, value.as_slice());
                } else {
                    result.set_null(row);
                    data.set_null(row);
                }
            }
        } else {
            let mut result = output.flat_vector();
            for (row, value) in rows.iter().enumerate() {
                if let Some(value) = value {
                    result.insert(row, std::str::from_utf8(value)?);
                } else {
                    result.set_null(row);
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let text = || if UNICODE { kind() } else { Id::Varchar.into() };
        vec![ScalarFunctionSignature::exact(
            vec![text(), text(), Id::Integer.into()],
            text(),
        )]
    }
}

struct Bin2Key;
impl VScalar for Bin2Key {
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
        let mut result = output.flat_vector();
        let mut remaining = CHUNK_LIMIT;
        for row in 0..len {
            let Some(units) = vector_units(&source, &data, row, len)? else {
                result.set_null(row);
                continue;
            };
            let key = msduck_core::bin2::equality_key(&units);
            if key.len() > remaining {
                return Err("Unicode equality keys exceed configured chunk limit".into());
            }
            remaining -= key.len();
            result.insert(row, key.as_slice());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind()],
            Id::Blob.into(),
        )]
    }
}

struct Bin2Compare<const ANSI: bool>;
impl<const ANSI: bool> VScalar for Bin2Compare<ANSI> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let left = input.flat_vector(0);
        let right = input.flat_vector(1);
        let left_struct = input.struct_vector(0);
        let right_struct = input.struct_vector(1);
        let left_data = left_struct.child(0, len);
        let right_data = right_struct.child(0, len);
        let mut result = output.flat_vector();
        for row in 0..len {
            let (Some(left), Some(right)) = (
                vector_units(&left, &left_data, row, len)?,
                vector_units(&right, &right_data, row, len)?,
            ) else {
                result.set_null(row);
                continue;
            };
            let order = if ANSI {
                // Logical CHAR/VARCHAR conversion has already happened. Restore
                // the exact CP1252 bytes; do not add lossy conversion here.
                let left = msduck_core::encoding::encode_cp1252(&String::from_utf16(&left)?)?;
                let right = msduck_core::encoding::encode_cp1252(&String::from_utf16(&right)?)?;
                msduck_core::bin2::compare_bytes(&left, &right)
            } else {
                msduck_core::bin2::compare(&left, &right)
            };
            let order = match order {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            unsafe {
                result.as_mut_slice_with_len::<i32>(len)[row] = order;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind(), kind()],
            Id::Integer.into(),
        )]
    }
}

struct Length<const BYTES: bool>;
impl<const BYTES: bool> VScalar for Length<BYTES> {
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
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            if data.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let encoded = bytes(&data, row, len)?;
            if encoded.len() % 2 != 0 {
                return Err("invalid Unicode carrier: odd byte length".into());
            }
            let count = if BYTES {
                encoded.len()
            } else {
                let units: Vec<_> = encoded
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                msduck_core::character::len_utf16(&units)
            };
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[row] = i64::try_from(count)?;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind()],
            Id::Bigint.into(),
        )]
    }
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Concat<true>>("__msduck_concat_unicode")?;
    db.register_scalar_function::<Concat<false>>("__msduck_concat_cp1252")?;
    db.register_scalar_function::<Bin2Key>("__msduck_bin2_key")?;
    db.register_scalar_function::<Bin2Compare<false>>("__msduck_bin2_compare")?;
    db.register_scalar_function::<Bin2Compare<true>>("__msduck_bin2_ansi_compare")?;
    db.register_scalar_function::<FirstUnit>("__msduck_carrier_unicode")?;
    db.register_scalar_function::<Cast<true, false, true>>("__msduck_store_carrier_nvarchar")?;
    db.register_scalar_function::<Cast<true, true, true>>("__msduck_store_carrier_nchar")?;
    db.register_scalar_function::<Cast<false, false, true>>("__msduck_store_carrier_varchar")?;
    db.register_scalar_function::<Cast<false, true, true>>("__msduck_store_carrier_char")?;
    db.register_scalar_function::<Length<false>>("__msduck_carrier_len")?;
    db.register_scalar_function::<Length<true>>("__msduck_carrier_datalength")?;
    db.register_scalar_function::<Cast<true, false>>("__msduck_cast_carrier_nvarchar")?;
    db.register_scalar_function::<Cast<true, true>>("__msduck_cast_carrier_nchar")?;
    db.register_scalar_function::<Cast<false, false>>("__msduck_cast_carrier_varchar")?;
    db.register_scalar_function::<Cast<false, true>>("__msduck_cast_carrier_char")?;
    db.register_scalar_function::<Pack>("__msduck_pack_unicode")?;
    db.register_scalar_function::<Pack<true>>("__msduck_unicode_from_le")?;
    db.register_scalar_function::<Slice<false>>("__msduck_left_unicode")?;
    db.register_scalar_function::<Slice<true>>("__msduck_right_unicode")?;
    db.register_scalar_function::<Slice<false, true>>("__msduck_left_unicode_typed")?;
    db.register_scalar_function::<Slice<true, true>>("__msduck_right_unicode_typed")?;
    db.register_scalar_function::<Ansi<false, false>>("__msduck_left_varchar")?;
    db.register_scalar_function::<Ansi<true, false>>("__msduck_right_varchar")?;
    db.register_scalar_function::<Ansi<false, true>>("__msduck_left_binary")?;
    db.register_scalar_function::<Ansi<true, true>>("__msduck_right_binary")?;
    // typeof is a bind-time property. Only the selected branch evaluates the
    // input, including for volatile values; carriers never pass through VARCHAR.
    db.execute_batch("CREATE OR REPLACE MACRO __msduck_carrier_input(v) AS CASE WHEN typeof(v)='STRUCT(__msduck_utf16le BLOB)' THEN CAST(v AS STRUCT(__msduck_utf16le BLOB)) ELSE __msduck_pack_unicode(CAST(v AS VARCHAR)) END")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concat_preserves_reference_overflow_and_raw_units() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let ansi: String = db.query_row("SELECT __msduck_concat_cp1252(__msduck_concat_cp1252(repeat('a',8000),'b',8000),'c',-1)", [], |r| r.get(0)).unwrap();
        assert_eq!(ansi, "a".repeat(8000) + "c");
        let early: String = db.query_row("SELECT __msduck_concat_cp1252(__msduck_concat_cp1252(repeat('a',8000),'b',-1),'c',-1)", [], |r| r.get(0)).unwrap();
        assert_eq!(early, "a".repeat(8000) + "bc");
        let cp: String = db
            .query_row("SELECT __msduck_concat_cp1252('€','x',1)", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cp, "€");
        let raw: Vec<u8> = db.query_row("SELECT (__msduck_concat_unicode(__msduck_pack_unicode(repeat('a',3999)),__msduck_pack_unicode('🦆'),4000)).__msduck_utf16le", [], |r| r.get(0)).unwrap();
        assert_eq!(
            raw,
            "a".repeat(3999)
                .encode_utf16()
                .chain([0xd83e])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let space: Vec<u8> = db.query_row("SELECT (__msduck_concat_unicode(__msduck_concat_unicode(__msduck_pack_unicode('a'),__msduck_pack_unicode(repeat(' ',8000)),4000),__msduck_pack_unicode('b'),4000)).__msduck_utf16le", [], |r| r.get(0)).unwrap();
        assert_eq!(
            space,
            ("a".to_owned() + &" ".repeat(3999))
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        for width in [-2, 4001] {
            assert!(db.query_row("SELECT __msduck_concat_unicode(__msduck_pack_unicode('a'),__msduck_pack_unicode('b'),?)",[width],|r|r.get::<_,duckdb::types::Value>(0)).is_err());
        }
        assert!(db.query_row("SELECT __msduck_concat_unicode(struct_pack(__msduck_utf16le := from_hex('01')),__msduck_pack_unicode('b'),2)",[],|r|r.get::<_,duckdb::types::Value>(0)).is_err());
    }

    #[test]
    fn concat_vectors_propagate_null_and_evaluate_children_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE concat_calls; CREATE TABLE concat_values AS SELECT i, __msduck_concat_unicode(__msduck_pack_unicode(CASE WHEN i%17=0 THEN NULL ELSE CAST(nextval('concat_calls') AS VARCHAR) END), __msduck_pack_unicode('x'),4000) AS u FROM range(6000) t(i)").unwrap();
        let count: i64 = db
            .query_row("SELECT count(u) FROM concat_values", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 6000 - 353);
        let calls: i64 = db
            .query_row("SELECT currval('concat_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, count);
        let selected: i64 = db.query_row("SELECT count(__msduck_concat_unicode(u,__msduck_pack_unicode('y'),-1)) FROM concat_values WHERE i%3=0",[],|r|r.get(0)).unwrap();
        assert_eq!(selected, 2000 - 118);
        let empty: String = db
            .query_row("SELECT __msduck_concat_cp1252('a','b',0)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(empty, "");
        let null: Option<String> = db
            .query_row("SELECT __msduck_concat_cp1252('a',NULL,2)", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(null, None);
    }

    #[test]
    fn bin2_index_keys_enforce_padding_equality_without_losing_units() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE TABLE bin2_values(u STRUCT(__msduck_utf16le BLOB)); CREATE UNIQUE INDEX bin2_idx ON bin2_values(__msduck_bin2_key(u)); INSERT INTO bin2_values VALUES(__msduck_pack_unicode('a')),(__msduck_unicode_from_le(from_hex('3ED8'))),(__msduck_unicode_from_le(from_hex('86DD')))").unwrap();
        assert!(
            db.execute_batch("INSERT INTO bin2_values VALUES(__msduck_pack_unicode('a '))")
                .is_err()
        );
        assert!(
            db.execute_batch("UPDATE bin2_values SET u=__msduck_pack_unicode('a ')")
                .is_err()
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM bin2_values", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        let stored = db
            .prepare("SELECT hex(u.__msduck_utf16le) FROM bin2_values ORDER BY 1")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(stored, ["3ED8", "6100", "86DD"]);
        db.execute_batch(
            "INSERT INTO bin2_values VALUES(__msduck_unicode_from_le(from_hex('61000000')))",
        )
        .unwrap();
        let order:i32=db.query_row("SELECT __msduck_bin2_compare(__msduck_pack_unicode('a'),__msduck_unicode_from_le(from_hex('610020000000')))",[],|r|r.get(0)).unwrap();
        assert_eq!(order, 1);
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../reference/unicode-collation.json")).unwrap();
        for case in reference["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["collation"] == "Latin1_General_100_BIN2")
        {
            let encoded = |value: &serde_json::Value| {
                value
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|u| u16::try_from(u.as_u64().unwrap()).unwrap().to_le_bytes())
                    .collect::<Vec<_>>()
            };
            let actual:i32=db.query_row("SELECT __msduck_bin2_compare(__msduck_unicode_from_le(?),__msduck_unicode_from_le(?))",duckdb::params![encoded(&case["left"]),encoded(&case["right"])],|r|r.get(0)).unwrap();
            assert_eq!(
                i64::from(actual),
                case["reference"]["sets"][0]["rows"][0][0].as_i64().unwrap(),
                "{}",
                case["query"]
            );
        }
        db.execute_batch("CREATE SEQUENCE bin2_calls START 1")
            .unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM (SELECT i,__msduck_bin2_key(__msduck_carrier_input(CASE WHEN nextval('bin2_calls')%17=0 THEN NULL ELSE 'a ' END)) AS k FROM range(6000) t(i)) WHERE k IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE from_hex('0061') END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert_eq!(
            db.query_row("SELECT currval('bin2_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        db.execute_batch("CREATE SEQUENCE ansi_bin2_calls START 1")
            .unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT i,__msduck_bin2_ansi_compare(__msduck_carrier_input(CASE WHEN nextval('ansi_bin2_calls')%17=0 THEN NULL ELSE '€' END),__msduck_carrier_input(chr(160))) AS n FROM range(6000) t(i)) WHERE n IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE -1 END", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert_eq!(
            db.query_row("SELECT currval('ansi_bin2_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        for function in [
            "__msduck_bin2_key(v)",
            "__msduck_bin2_compare(v,v)",
            "__msduck_bin2_ansi_compare(v,v)",
        ] {
            let sql = format!(
                "SELECT {function} FROM (SELECT struct_pack(__msduck_utf16le := from_hex('00')) AS v)"
            );
            assert!(db.prepare(&sql).unwrap().query_arrow([]).is_err());
        }
    }

    #[test]
    fn unicode_reads_raw_units_after_storage_and_evaluates_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let sql = "SELECT CAST(LEFT(N'🦆',1) AS NVARCHAR(4)) AS h,RIGHT(N'🦆',1) AS l INTO dbo.raw_units; SELECT UNICODE(h) AS h,UNICODE(l) AS l,UNICODE(CAST(NULL AS NVARCHAR(4))) AS n,UNICODE(N'') AS e,UNICODE(12) AS d INTO dbo.unit_codes FROM dbo.raw_units";
        let (response, success) = session.batch_response(sql, &Default::default(), false, None);
        assert!(success, "{response:?}");
        let actual = session
            .db
            .query_row("SELECT h,l,n,e,d FROM dbo.unit_codes", [], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, Option<i32>>(2)?,
                    r.get::<_, Option<i32>>(3)?,
                    r.get::<_, i32>(4)?,
                ))
            })
            .unwrap();
        assert_eq!(actual, (55358, 56710, None, None, 49));
        session
            .db
            .execute_batch("CREATE SEQUENCE first_unit_calls START 1")
            .unwrap();
        let wrong:i64=session.db.query_row("SELECT count(*) FROM (SELECT i,__msduck_carrier_unicode(__msduck_carrier_input(CASE WHEN nextval('first_unit_calls')%17=0 THEN NULL ELSE '🦆' END)) AS n FROM range(6000) t(i)) WHERE n IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE 55358 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('first_unit_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        for payload in ["from_hex('00')", "NULL::BLOB"] {
            let sql = format!(
                "SELECT __msduck_carrier_unicode(struct_pack(__msduck_utf16le := {payload}))"
            );
            assert!(session.db.prepare(&sql).unwrap().query_arrow([]).is_err());
        }
    }

    #[test]
    fn assignments_keep_materialized_unicode_layout_and_declaration() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "SELECT 1 AS id,CAST(LEFT(N'🦆',1) AS NVARCHAR(4)) AS u,CAST(LEFT(N'🦆',1) AS NCHAR(4)) AS f INTO dbo.unit_storage",
            "INSERT INTO dbo.unit_storage(id,u,f) VALUES(2,LEFT(N'🦆',1),LEFT(N'🦆',1))",
            "UPDATE dbo.unit_storage SET u=RIGHT(N'🦆',1),f=RIGHT(N'🦆',1) WHERE id=1",
            "UPDATE t SET u=LEFT(N'🦆',1) OUTPUT deleted.u,inserted.u FROM dbo.unit_storage t WHERE id=1",
            "INSERT INTO dbo.unit_storage(id,u,f) VALUES(3,12,34)",
            "INSERT INTO dbo.unit_storage(id,u,f) VALUES(4,NULL,NULL)",
        ] {
            let (response, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(success, "{sql}: {response:?}");
        }
        let values = |session: &crate::engine::Session| {
            session.db.prepare("SELECT id,hex(u.__msduck_utf16le),hex(f.__msduck_utf16le) FROM dbo.unit_storage ORDER BY id").unwrap()
                .query_map([], |r| Ok((r.get::<_,i32>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?)))
                .unwrap().collect::<duckdb::Result<Vec<_>>>().unwrap()
        };
        let before = values(&session);
        assert_eq!(
            before,
            vec![
                (1, Some("3ED8".into()), Some("86DD200020002000".into())),
                (2, Some("3ED8".into()), Some("3ED8200020002000".into())),
                (3, Some("31003200".into()), Some("3300340020002000".into())),
                (4, None, None),
            ]
        );
        for sql in [
            "INSERT INTO dbo.unit_storage(id,u) VALUES(5,N'12345')",
            "UPDATE dbo.unit_storage SET u=N'12345' WHERE id=1",
            "UPDATE t SET u=N'12345' OUTPUT inserted.u FROM dbo.unit_storage t WHERE id=1",
        ] {
            assert!(
                !session
                    .batch_response(sql, &Default::default(), false, None)
                    .1,
                "{sql}"
            );
            assert_eq!(values(&session), before);
        }
    }

    #[test]
    fn storage_preserves_units_padding_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE storage_calls START 1; CREATE TABLE stored AS SELECT i,__msduck_store_carrier_nchar(__msduck_left_unicode(__msduck_pack_unicode(CASE WHEN nextval('storage_calls')%17=0 THEN NULL ELSE '🦆' END),1),3) AS u FROM range(6000) t(i)").unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM stored WHERE u.__msduck_utf16le IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE from_hex('3ED820002000') END", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert_eq!(
            db.query_row("SELECT currval('storage_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        let trimmed: Vec<u8> = db.query_row("SELECT (__msduck_store_carrier_nvarchar(__msduck_unicode_from_le(from_hex('00DC20002000')),2)).__msduck_utf16le", [], |r| r.get(0)).unwrap();
        assert_eq!(trimmed, [0, 0xdc, 32, 0]);
        let ansi: String = db
            .query_row(
                "SELECT __msduck_store_carrier_char(__msduck_unicode_from_le(from_hex('00DC')),2)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ansi, "? ");
        for query in [
            "SELECT __msduck_store_carrier_nvarchar(__msduck_pack_unicode('🦆'),1)",
            "SELECT __msduck_store_carrier_nchar(__msduck_pack_unicode('a b'),2)",
        ] {
            let error = db
                .prepare(query)
                .unwrap()
                .query_arrow([])
                .err()
                .expect("conversion must fail")
                .to_string();
            assert!(error.contains(msduck_core::character::TRUNCATED), "{error}");
        }
        for (payload, message) in [
            ("from_hex('00')", "odd byte length"),
            ("NULL::BLOB", "NULL payload"),
        ] {
            let sql = format!(
                "SELECT __msduck_store_carrier_nvarchar(struct_pack(__msduck_utf16le := {payload}),3)"
            );
            let error = db
                .prepare(&sql)
                .unwrap()
                .query_arrow([])
                .err()
                .expect("conversion must fail")
                .to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn carrier_input_dispatch_binds_scalars_without_stringifying_structs() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();

        for (input, expected) in [
            ("12", "31003200"),
            ("'abc'", "610062006300"),
            ("__msduck_unicode_from_le(from_hex('3ED8'))", "3ED8"),
        ] {
            let sql = format!("SELECT hex((__msduck_carrier_input({input})).__msduck_utf16le)");
            let actual: String = db.query_row(&sql, [], |r| r.get(0)).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn carrier_materializes_surrogates_and_composes_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE TABLE units AS SELECT i,__msduck_left_unicode(__msduck_pack_unicode(CASE WHEN i%3=0 THEN NULL ELSE '🦆xy' END),1) AS u FROM range(6000) t(i)").unwrap();
        let counts: (i64, i64) = db
            .query_row(
                "SELECT count(u),count(*) FILTER(WHERE hex(u.__msduck_utf16le)='3ED8') FROM units",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (4000, 4000));
        let bytes: Vec<u8> = db.query_row("SELECT (__msduck_right_unicode(__msduck_left_unicode(__msduck_pack_unicode('🦆xy'),2),1)).__msduck_utf16le",[],|r|r.get(0)).unwrap();
        assert_eq!(bytes, [0x86, 0xdd]);
        let mut wire = vec![];
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        crate::tds::unicode_value(&mut wire, &crate::tds::Type::Nvarchar(1), Some(&units)).unwrap();
        assert_eq!(wire, [2, 0, 0x86, 0xdd]);
        assert_eq!(db.query_row("SELECT octet_length((__msduck_right_unicode(__msduck_pack_unicode('abc'),0)).__msduck_utf16le)",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn stored_carrier_survives_reopen_and_volatile_input_is_evaluated_once() {
        let path = std::env::temp_dir().join(format!(
            "msduck-unicode-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let db = duckdb::Connection::open(&path).unwrap();
            register(&db).unwrap();
            db.execute_batch("CREATE TABLE saved AS SELECT __msduck_left_unicode(__msduck_pack_unicode('🦆'),1) AS u").unwrap();
            db.execute_batch("CREATE SEQUENCE calls START 1").unwrap();
            let last: i64 = db.query_row("SELECT max(octet_length((__msduck_left_unicode(__msduck_pack_unicode(CAST(nextval('calls') AS VARCHAR)),1)).__msduck_utf16le)) FROM range(6000)", [], |r| r.get(0)).unwrap();
            assert_eq!(last, 2);
            assert_eq!(
                db.query_row("SELECT currval('calls')", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
        {
            let db = duckdb::Connection::open(&path).unwrap();
            register(&db).unwrap();
            let bytes: Vec<u8> = db
                .query_row(
                    "SELECT (__msduck_right_unicode(u,1)).__msduck_utf16le FROM saved",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(bytes, [0x3e, 0xd8]);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn session_rows_encode_carriers_and_reject_invalid_payloads() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let (response, success) = session.batch_response(
            "SELECT __msduck_left_unicode(__msduck_pack_unicode(N'🦆'),1) AS u",
            &Default::default(),
            false,
            None,
        );
        assert!(success, "{response:?}");
        let expected = [
            0xd1, 2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0x3e, 0xd8, 0, 0, 0, 0,
        ];
        assert!(response.windows(expected.len()).any(|v| v == expected));
        let (empty, success) = session.batch_response(
            "SELECT __msduck_pack_unicode(N'x') AS u WHERE 1=0",
            &Default::default(),
            false,
            None,
        );
        assert!(success && empty[0] == 0x81);
        let malformed = duckdb::types::Value::Struct(
            vec![(
                "__msduck_utf16le".into(),
                duckdb::types::Value::Blob(vec![0]),
            )]
            .into(),
        );
        let mut out = vec![0xd1];
        assert!(encode(&mut out, &crate::tds::Type::Text, &malformed).is_err());
        assert_eq!(out, [0xd1]);
    }

    #[test]
    fn sql_lengths_read_materialized_units_and_evaluate_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let (response,success)=session.batch_response("CREATE TABLE dbo.unicode_length_seed(id INT,s NVARCHAR(3)); INSERT INTO dbo.unicode_length_seed SELECT CAST(i AS INT),N'🦆 ' FROM range(6000) t(i); SELECT id,LEFT(CASE WHEN id%17=0 THEN CAST(NULL AS NVARCHAR(3)) ELSE s END,3) AS u INTO dbo.unicode_length_source FROM dbo.unicode_length_seed; SELECT id,LEN(u) AS l,DATALENGTH(u) AS d INTO dbo.unicode_length_output FROM dbo.unicode_length_source",&Default::default(),false,None);
        assert!(success, "{response:?}");
        let wrong:i64=session.db.query_row("SELECT count(*) FROM dbo.unicode_length_output WHERE l IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 2 END OR d IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 6 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        session
            .db
            .execute_batch("CREATE SEQUENCE unicode_length_calls START 1")
            .unwrap();
        let count:i64=session.db.query_row("SELECT count(__msduck_carrier_len(__msduck_pack_unicode(CAST(nextval('unicode_length_calls') AS VARCHAR)))) FROM range(6000)",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('unicode_length_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn sql_variables_rebind_raw_units_without_binary_or_display_conversion() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "DECLARE @s NVARCHAR(3)=LEFT(N'🦆xy',1); SELECT @s",
            "DECLARE @s NVARCHAR(3),@t NVARCHAR(2); SET @s=LEFT(N'🦆xy',1); SET @t=@s; SELECT @t",
            "DECLARE @s NVARCHAR(3); SELECT @s=LEFT(N'🦆xy',1); SELECT @s",
        ] {
            let (response, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(success, "{sql}: {response:?}");
            assert!(response.windows(5).any(|b| b == [0xd1, 2, 0, 0x3e, 0xd8]));
        }
    }

    #[test]
    fn print_emits_raw_units_and_blank_messages_through_session() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for (sql, units) in [
            ("PRINT LEFT(N'🦆xy',1)", vec![0xd83e]),
            ("PRINT NULL", vec![32]),
            ("PRINT ''", vec![32]),
            (
                "DECLARE @s NCHAR(3)=LEFT(N'🦆xy',1); PRINT @s",
                vec![0xd83e, 32, 32],
            ),
        ] {
            let (response, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(success, "{sql}: {response:?}");
            let mut expected = vec![];
            crate::tds::diagnostic_utf16(
                &mut expected,
                crate::tds::DiagnosticKind::Information,
                0,
                1,
                0,
                &units,
            );
            assert!(response.windows(expected.len()).any(|v| v == expected));
        }
    }

    #[test]
    fn invalid_carriers_and_negative_lengths_fail_and_connection_recovers() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for (sql, message) in [
            (
                "SELECT __msduck_left_unicode({'__msduck_utf16le':from_hex('00')},1)",
                "odd byte length",
            ),
            (
                "SELECT __msduck_left_unicode({'__msduck_utf16le':NULL::BLOB},1)",
                "NULL payload",
            ),
            (
                "SELECT __msduck_left_unicode(__msduck_pack_unicode('abc'),-1)",
                "Invalid length parameter passed to the left function.",
            ),
        ] {
            let error = db
                .prepare(sql)
                .unwrap()
                .query([])
                .err()
                .expect("query must fail")
                .to_string();
            assert!(error.contains(message), "{error}");
        }
        assert_eq!(
            db.query_row("SELECT 1", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            1
        );
    }
}
