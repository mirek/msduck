//! Explicitly planned native ANSI storage, independent of SQL decoding/capacity.
use duckdb::{
    Connection,
    arrow::{
        array::{Array, BinaryArray, LargeBinaryArray, StructArray, UInt32Array},
        datatypes::DataType,
    },
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle, LogicalTypeId as Id},
    types::Value,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::ansi_bytes::{AnsiBytes, AnsiView, EncodingIdentity, checked_byte_total};

const TAG: &str = "__msduck_ansi_encoding";
const PAYLOAD: &str = "__msduck_ansi_bytes";

/// Caller-approved logical encoding and explicit payload resource budgets.
/// A matching physical STRUCT alone does not establish logical SQL VARCHAR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    encoding: EncodingIdentity,
    tag: u32,
    cell_limit: usize,
    batch_limit: usize,
}

impl Plan {
    /// Validate the declaration before processing even NULL/empty operands.
    pub fn new(
        encoding: EncodingIdentity,
        cell_limit: usize,
        batch_limit: usize,
    ) -> anyhow::Result<Self> {
        let tag = match encoding {
            EncodingIdentity::Cp1252 => 1252,
            EncodingIdentity::Cp1251 => 1251,
            EncodingIdentity::Utf8 => 65001,
            EncodingIdentity::Opaque(_) => {
                anyhow::bail!("unsupported native ANSI storage encoding")
            }
        };
        anyhow::ensure!(
            cell_limit <= batch_limit,
            "ANSI cell limit exceeds batch limit"
        );
        Ok(Self {
            encoding,
            tag,
            cell_limit,
            batch_limit,
        })
    }

    pub fn encoding(self) -> EncodingIdentity {
        self.encoding
    }

    /// These names are fixed identifiers, never interpolated caller SQL.
    pub fn pack_function(self) -> &'static str {
        match self.encoding {
            EncodingIdentity::Cp1252 => "__msduck_ansi_pack_1252",
            EncodingIdentity::Cp1251 => "__msduck_ansi_pack_1251",
            EncodingIdentity::Utf8 => "__msduck_ansi_pack_65001",
            EncodingIdentity::Opaque(_) => unreachable!("validated plan"),
        }
    }

    pub fn unpack_function(self) -> &'static str {
        match self.encoding {
            EncodingIdentity::Cp1252 => "__msduck_ansi_unpack_1252",
            EncodingIdentity::Cp1251 => "__msduck_ansi_unpack_1251",
            EncodingIdentity::Utf8 => "__msduck_ansi_unpack_65001",
            EncodingIdentity::Opaque(_) => unreachable!("validated plan"),
        }
    }

    /// Register explicit BLOB -> tagged storage -> BLOB adapters on this connection.
    /// Registration does not enable SQL collation or common Value conversion.
    pub fn register(self, connection: &Connection) -> anyhow::Result<()> {
        connection.register_scalar_function_with_state::<Pack>(self.pack_function(), &self)?;
        connection.register_scalar_function_with_state::<Unpack>(self.unpack_function(), &self)?;
        Ok(())
    }

    pub fn storage_type(self) -> LogicalTypeHandle {
        storage_type()
    }

    /// Bind a binary operand to the matching pack function; it is not yet VARCHAR.
    pub fn bind(self, value: Option<AnsiView<'_>>) -> anyhow::Result<Value> {
        let Some(value) = value else {
            return Ok(Value::Null);
        };
        anyhow::ensure!(
            value.encoding() == self.encoding,
            "ANSI source encoding disagrees with storage plan"
        );
        self.check_cell(value.bytes().len())?;
        Ok(Value::Blob(copy_bytes(value.bytes())?))
    }

    pub fn read_value(self, value: &Value) -> anyhow::Result<Option<AnsiBytes>> {
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        let Value::Struct(fields) = value else {
            anyhow::bail!("invalid ANSI carrier value")
        };
        anyhow::ensure!(fields.keys().count() == 2, "invalid ANSI carrier fields");
        let Some(Value::UInt(tag)) = fields.get(&TAG.to_owned()) else {
            anyhow::bail!("invalid ANSI carrier encoding field")
        };
        let Some(Value::Blob(bytes)) = fields.get(&PAYLOAD.to_owned()) else {
            anyhow::bail!("invalid ANSI carrier payload field")
        };
        self.check_tag(*tag)?;
        self.owned(bytes).map(Some)
    }

    /// Physical validation is mandatory even for NULL rows and zero-row arrays.
    /// The caller must independently establish this column's logical ANSI plan.
    pub fn validate_array(self, array: &dyn Array) -> anyhow::Result<()> {
        anyhow::ensure!(
            is_arrow(array.data_type()),
            "invalid ANSI carrier Arrow type"
        );
        let values = array
            .as_any()
            .downcast_ref::<StructArray>()
            .ok_or_else(|| anyhow::anyhow!("invalid ANSI carrier Arrow struct"))?;
        anyhow::ensure!(
            values.columns().len() == 2 && values.columns().iter().all(|c| c.len() == values.len()),
            "invalid ANSI carrier child lengths"
        );
        Ok(())
    }

    pub fn read_array(self, array: &dyn Array, row: usize) -> anyhow::Result<Option<AnsiBytes>> {
        self.validate_array(array)?;
        anyhow::ensure!(row < array.len(), "ANSI carrier row out of bounds");
        self.arrow_bytes(array, row)?
            .map(|b| self.owned(b))
            .transpose()
    }

    /// Preflight the complete batch before allocating/copying any payload.
    pub fn read_batch(self, array: &dyn Array) -> anyhow::Result<Vec<Option<AnsiBytes>>> {
        self.validate_array(array)?;
        let mut total = 0;
        for row in 0..array.len() {
            if let Some(bytes) = self.arrow_bytes(array, row)? {
                self.check_cell(bytes.len())?;
                total = checked_byte_total(total, bytes.len(), self.batch_limit)?;
            }
        }
        let mut values = Vec::new();
        values.try_reserve_exact(array.len())?;
        for row in 0..array.len() {
            values.push(
                self.arrow_bytes(array, row)?
                    .map(|b| self.owned(b))
                    .transpose()?,
            );
        }
        Ok(values)
    }

    fn arrow_bytes(self, array: &dyn Array, row: usize) -> anyhow::Result<Option<&[u8]>> {
        if array.is_null(row) {
            return Ok(None);
        }
        let values = array
            .as_any()
            .downcast_ref::<StructArray>()
            .ok_or_else(|| anyhow::anyhow!("invalid ANSI carrier struct"))?;
        let tags = values
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid ANSI carrier encoding array"))?;
        let bytes = values.column(1);
        anyhow::ensure!(
            !tags.is_null(row) && !bytes.is_null(row),
            "non-NULL ANSI carrier has NULL child"
        );
        self.check_tag(tags.value(row))?;
        if let Some(bytes) = bytes.as_any().downcast_ref::<BinaryArray>() {
            return Ok(Some(bytes.value(row)));
        }
        if let Some(bytes) = bytes.as_any().downcast_ref::<LargeBinaryArray>() {
            return Ok(Some(bytes.value(row)));
        }
        anyhow::bail!("invalid ANSI carrier byte array")
    }

    fn check_tag(self, tag: u32) -> anyhow::Result<()> {
        anyhow::ensure!(
            tag == self.tag,
            "ANSI stored encoding {tag} disagrees with plan {}",
            self.tag
        );
        Ok(())
    }
    fn check_cell(self, len: usize) -> anyhow::Result<()> {
        checked_byte_total(0, len, self.cell_limit)?;
        Ok(())
    }
    fn owned(self, bytes: &[u8]) -> anyhow::Result<AnsiBytes> {
        self.check_cell(bytes.len())?;
        Ok(AnsiBytes::from_vec(
            self.encoding,
            copy_bytes(bytes)?,
            self.cell_limit,
        )?)
    }
}

fn copy_bytes(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut result = Vec::new();
    result.try_reserve_exact(bytes.len())?;
    result.extend_from_slice(bytes);
    Ok(result)
}

fn storage_type() -> LogicalTypeHandle {
    LogicalTypeHandle::struct_type(&[(TAG, Id::UInteger.into()), (PAYLOAD, Id::Blob.into())])
}

fn is_logical(kind: &LogicalTypeHandle) -> bool {
    kind.id() == Id::Struct
        && kind.num_children() == 2
        && kind.child_name(0) == TAG
        && kind.child(0).id() == Id::UInteger
        && kind.child_name(1) == PAYLOAD
        && kind.child(1).id() == Id::Blob
}

fn is_arrow(kind: &DataType) -> bool {
    matches!(kind, DataType::Struct(fields) if fields.len() == 2
        && fields[0].name() == TAG && fields[0].data_type() == &DataType::UInt32
        && fields[1].name() == PAYLOAD
        && matches!(fields[1].data_type(), DataType::Binary | DataType::LargeBinary))
}

// The string_t copy keeps inline storage alive while its borrowed bytes are used.
// Long storage belongs to DuckDB and remains live for this callback. Never copy
// native bytes before the caller has checked per-cell and full-chunk budgets.
fn with_native_bytes<T>(
    vector: &FlatVector<'_>,
    row: usize,
    len: usize,
    f: impl FnOnce(&[u8]) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    anyhow::ensure!(
        vector.logical_type().id() == Id::Blob && row < len && len <= vector.capacity(),
        "invalid ANSI native byte vector bounds/type"
    );
    anyhow::ensure!(
        !vector.try_row_is_null(row as u64)?,
        "NULL ANSI native byte vector row"
    );
    // SAFETY: validated BLOB physical storage, initialized non-NULL row, bounded
    // length, callback-owned input only; no writable alias or retained pointer.
    let mut value = unsafe {
        vector
            .as_mut_ptr::<duckdb::ffi::duckdb_string_t>()
            .add(row)
            .read()
    };
    let size = unsafe { duckdb::ffi::duckdb_string_t_length(value) as usize };
    if size == 0 {
        return f(&[]);
    }
    let pointer = unsafe { duckdb::ffi::duckdb_string_t_data(&mut value) }.cast::<u8>();
    anyhow::ensure!(
        !pointer.is_null() && size <= isize::MAX as usize,
        "invalid ANSI native byte pointer/length"
    );
    // SAFETY: DuckDB's live string_t owns size initialized bytes; inline storage
    // is in value, which stays alive through f. Nothing escapes this closure.
    f(unsafe { std::slice::from_raw_parts(pointer, size) })
}

struct Pack;
impl VScalar for Pack {
    type State = Plan;
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Blob.into()],
            storage_type(),
        )]
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        plan: &Plan,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        (|| -> anyhow::Result<()> {
            anyhow::ensure!(input.num_columns() == 1, "invalid ANSI pack column count");
            let len = input.len();
            let source = input.flat_vector(0);
            anyhow::ensure!(
                source.logical_type().id() == Id::Blob && len <= source.capacity(),
                "invalid ANSI pack input type/length"
            );
            // Check through a flat wrapper before requesting STRUCT children.
            {
                let flat = output.flat_vector();
                anyhow::ensure!(
                    is_logical(&flat.logical_type()) && len <= flat.capacity(),
                    "invalid ANSI pack output type/length"
                );
            }
            let mut total = 0;
            for row in 0..len {
                if source.try_row_is_null(row as u64)? {
                    continue;
                }
                with_native_bytes(&source, row, len, |bytes| {
                    plan.check_cell(bytes.len())?;
                    total = checked_byte_total(total, bytes.len(), plan.batch_limit)?;
                    Ok(())
                })?;
            }
            let mut result = output.struct_vector();
            let mut tags = result.child(0, len);
            let mut payload = result.child(1, len);
            anyhow::ensure!(
                tags.logical_type().id() == Id::UInteger && payload.logical_type().id() == Id::Blob,
                "invalid ANSI pack children"
            );
            for row in 0..len {
                if source.try_row_is_null(row as u64)? {
                    result.set_null(row);
                    tags.set_null(row);
                    payload.set_null(row);
                    continue;
                }
                // SAFETY: distinct output child, validated UINTEGER and bounded
                // row. Write only this slot without viewing uninitialized slots.
                unsafe {
                    tags.as_mut_ptr::<u32>().add(row).write(plan.tag);
                }
                with_native_bytes(&source, row, len, |bytes| {
                    payload.insert(row, bytes);
                    Ok(())
                })?;
            }
            Ok(())
        })()
        .map_err(Into::into)
    }
}

struct Unpack;
impl VScalar for Unpack {
    type State = Plan;
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![storage_type()],
            Id::Blob.into(),
        )]
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        plan: &Plan,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        (|| -> anyhow::Result<()> {
            anyhow::ensure!(input.num_columns() == 1, "invalid ANSI unpack column count");
            let len = input.len();
            {
                let flat = input.flat_vector(0);
                anyhow::ensure!(
                    is_logical(&flat.logical_type()) && len <= flat.capacity(),
                    "invalid ANSI unpack type/length"
                );
            }
            let source = input.struct_vector(0);
            let tags = source.child(0, len);
            let payload = source.child(1, len);
            anyhow::ensure!(
                tags.logical_type().id() == Id::UInteger && payload.logical_type().id() == Id::Blob,
                "invalid ANSI unpack children"
            );
            let mut result = output.flat_vector();
            anyhow::ensure!(
                result.logical_type().id() == Id::Blob && len <= result.capacity(),
                "invalid ANSI unpack output"
            );
            let mut total = 0;
            for row in 0..len {
                if source.try_row_is_null(row as u64)? {
                    continue;
                }
                anyhow::ensure!(
                    !tags.try_row_is_null(row as u64)? && !payload.try_row_is_null(row as u64)?,
                    "non-NULL ANSI carrier has NULL child"
                );
                // SAFETY: validated UINTEGER child and non-NULL initialized row;
                // only a read-only wrapper, bounded by the callback's row count.
                plan.check_tag(unsafe { tags.as_mut_ptr::<u32>().add(row).read() })?;
                with_native_bytes(&payload, row, len, |bytes| {
                    plan.check_cell(bytes.len())?;
                    total = checked_byte_total(total, bytes.len(), plan.batch_limit)?;
                    Ok(())
                })?;
            }
            for row in 0..len {
                if source.try_row_is_null(row as u64)? {
                    result.set_null(row);
                    continue;
                }
                with_native_bytes(&payload, row, len, |bytes| {
                    result.insert(row, bytes);
                    Ok(())
                })?;
            }
            Ok(())
        })()
        .map_err(Into::into)
    }
}
