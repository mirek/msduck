//! Connection-owned complete projections over explicitly identified native carriers.
use super::{Plan as Storage, is_logical, storage_type, with_native_bytes};
use duckdb::{
    Connection,
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{
    ansi_bytes::{AnsiView, EncodingIdentity, checked_byte_total},
    ansi_conversion::{ProjectedValue, ProjectionLimits, ProjectionTarget, project},
};

/// Explicit logical identity and active per-cell/chunk input/output payload limits.
/// This is not a collation, storage-capacity, SQL CAST or wire-admission plan.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    source: Storage,
    target: ProjectionTarget,
    limits: ProjectionLimits,
    output_chunk_limit: usize,
}

impl Plan {
    pub fn new(
        source: EncodingIdentity,
        target: ProjectionTarget,
        limits: ProjectionLimits,
        input_chunk_limit: usize,
        output_chunk_limit: usize,
    ) -> anyhow::Result<Self> {
        // Validate the semantic projection before any nullable payload is read.
        project(source, None, target, limits)?;
        anyhow::ensure!(
            limits.output_bytes <= output_chunk_limit,
            "ANSI projection output cell limit exceeds chunk limit"
        );
        Ok(Self {
            source: Storage::new(source, limits.input_bytes, input_chunk_limit)?,
            target,
            limits,
            output_chunk_limit,
        })
    }

    /// Register on this connection only, under a caller-owned internal identifier.
    /// Different plans must use distinct names on a connection; no global state.
    pub fn register(self, db: &Connection, name: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            name.starts_with("__msduck_ansi_project_")
                && name.len() <= 128
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid internal ANSI projection function name"
        );
        match self.target {
            ProjectionTarget::Native(_) => {
                db.register_scalar_function_with_state::<Native>(name, &self)?
            }
            ProjectionTarget::SqlUtf16 => {
                db.register_scalar_function_with_state::<Unicode>(name, &self)?
            }
        }
        Ok(())
    }

    pub fn source_encoding(self) -> EncodingIdentity {
        self.source.encoding()
    }

    pub fn target(self) -> ProjectionTarget {
        self.target
    }

    fn payloads(self, input: &mut DataChunkHandle) -> anyhow::Result<Vec<Option<Vec<u8>>>> {
        anyhow::ensure!(
            input.num_columns() == 1,
            "invalid ANSI projection column count"
        );
        let len = input.len();
        {
            let flat = input.flat_vector(0);
            anyhow::ensure!(
                is_logical(&flat.logical_type()) && len <= flat.capacity(),
                "invalid ANSI projection source type/length"
            );
        }
        let source = input.struct_vector(0);
        let tags = source.child(0, len);
        let bytes = source.child(1, len);
        anyhow::ensure!(
            tags.logical_type().id() == Id::UInteger
                && bytes.logical_type().id() == Id::Blob
                && len <= tags.capacity()
                && len <= bytes.capacity(),
            "invalid ANSI projection source children"
        );
        let mut total = 0;
        for row in 0..len {
            if source.try_row_is_null(row as u64)? {
                continue;
            }
            anyhow::ensure!(
                !tags.try_row_is_null(row as u64)? && !bytes.try_row_is_null(row as u64)?,
                "non-NULL ANSI projection source has NULL child"
            );
            // SAFETY: initialized non-NULL UINTEGER child, checked row/capacity.
            self.source
                .check_tag(unsafe { tags.as_mut_ptr::<u32>().add(row).read() })?;
            with_native_bytes(&bytes, row, len, |value| {
                self.source.check_cell(value.len())?;
                total = checked_byte_total(total, value.len(), self.source.batch_limit)?;
                Ok(())
            })?;
        }
        // Complete every fallible projection/allocation before output mutation.
        // The retained payloads are bounded by output_chunk_limit; converting a
        // UTF16 result temporarily retains at most one additional bounded cell.
        let mut result = Vec::new();
        result.try_reserve_exact(len)?;
        let mut total = 0;
        for row in 0..len {
            if source.try_row_is_null(row as u64)? {
                result.push(None);
                continue;
            }
            let payload = with_native_bytes(&bytes, row, len, |value| {
                let view = AnsiView::new(self.source.encoding(), value, self.limits.input_bytes)?;
                let projected =
                    project(self.source.encoding(), Some(view), self.target, self.limits)?
                        .ok_or_else(|| anyhow::anyhow!("non-NULL ANSI projection became NULL"))?;
                let size = match &projected {
                    ProjectedValue::Native(bytes) => bytes.view().bytes().len(),
                    ProjectedValue::SqlUtf16(units) => units
                        .len()
                        .checked_mul(2)
                        .ok_or_else(|| anyhow::anyhow!("ANSI UTF16 projection length overflow"))?,
                };
                total = checked_byte_total(total, size, self.output_chunk_limit)?;
                match projected {
                    ProjectedValue::Native(value) => Ok(value.into_parts().1),
                    ProjectedValue::SqlUtf16(units) => {
                        let mut bytes = Vec::new();
                        bytes.try_reserve_exact(size)?;
                        for unit in units {
                            bytes.extend_from_slice(&unit.to_le_bytes());
                        }
                        Ok(bytes)
                    }
                }
            })?;
            result.push(Some(payload));
        }
        Ok(result)
    }
}

struct Native;
struct Unicode;

fn invoke(
    plan: &Plan,
    input: &mut DataChunkHandle,
    output: &mut dyn WritableVector,
) -> anyhow::Result<()> {
    let len = input.len();
    {
        let flat = output.flat_vector();
        let expected = match plan.target {
            ProjectionTarget::Native(_) => is_logical(&flat.logical_type()),
            ProjectionTarget::SqlUtf16 => crate::unicode_carrier::is_logical(&flat.logical_type()),
        };
        anyhow::ensure!(
            expected && len <= flat.capacity(),
            "invalid ANSI projection output type/length"
        );
    }
    let payloads = plan.payloads(input)?;
    let mut result = output.struct_vector();
    let tag = if let ProjectionTarget::Native(encoding) = plan.target {
        Some(Storage::new(encoding, plan.limits.output_bytes, plan.output_chunk_limit)?.tag)
    } else {
        None
    };
    let mut bytes = result.child(usize::from(tag.is_some()), len);
    anyhow::ensure!(
        bytes.logical_type().id() == Id::Blob && len <= bytes.capacity(),
        "invalid ANSI projection output bytes"
    );
    let mut tags = tag.map(|_| result.child(0, len));
    if let Some(tags) = &tags {
        anyhow::ensure!(
            tags.logical_type().id() == Id::UInteger && len <= tags.capacity(),
            "invalid ANSI projection output tags"
        );
    }
    for (row, payload) in payloads.iter().enumerate() {
        let Some(payload) = payload else {
            result.set_null(row);
            bytes.set_null(row);
            if let Some(tags) = &mut tags {
                tags.set_null(row);
            }
            continue;
        };
        if let (Some(tag), Some(tags)) = (tag, &mut tags) {
            // SAFETY: distinct bounded UINTEGER output child; write this slot only.
            unsafe {
                tags.as_mut_ptr::<u32>().add(row).write(tag);
            }
        }
        bytes.insert(row, payload.as_slice());
    }
    Ok(())
}

macro_rules! projection {
    ($name:ident, $kind:expr) => {
        impl VScalar for $name {
            type State = Plan;
            fn signatures() -> Vec<ScalarFunctionSignature> {
                vec![ScalarFunctionSignature::exact(vec![storage_type()], $kind)]
            }
            fn special_null_handling() -> bool {
                true
            }
            fn invoke(
                plan: &Plan,
                input: &mut DataChunkHandle,
                output: &mut dyn WritableVector,
            ) -> Result<(), Box<dyn std::error::Error>> {
                invoke(plan, input, output).map_err(Into::into)
            }
        }
    };
}
projection!(Native, storage_type());
projection!(Unicode, crate::unicode_carrier::kind());

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::{
        core::{ArrayVector, FlatVector, ListVector, StructVector},
        vtab::arrow::data_chunk_to_arrow,
    };
    struct Output(DataChunkHandle);
    impl WritableVector for Output {
        fn flat_vector(&mut self) -> FlatVector<'_> {
            self.0.flat_vector(0)
        }
        fn struct_vector(&mut self) -> StructVector<'_> {
            self.0.struct_vector(0)
        }
        fn list_vector(&mut self) -> ListVector<'_> {
            self.0.list_vector(0)
        }
        fn array_vector(&mut self) -> ArrayVector<'_> {
            self.0.array_vector(0)
        }
    }
    fn plan() -> Plan {
        Plan::new(
            EncodingIdentity::Cp1251,
            ProjectionTarget::Native(EncodingIdentity::Cp1252),
            ProjectionLimits {
                input_bytes: 8,
                output_bytes: 8,
            },
            16,
            16,
        )
        .unwrap()
    }
    fn fill(chunk: &DataChunkHandle, tags: &[u32], bytes: &[&[u8]]) {
        assert_eq!(tags.len(), bytes.len());
        chunk.set_len(tags.len());
        let structure = chunk.struct_vector(0);
        let data = structure.child(1, tags.len());
        let encodings = structure.child(0, tags.len());
        for (row, (&tag, bytes)) in tags.iter().zip(bytes).enumerate() {
            // SAFETY: allocated UINTEGER child, bounded row, writing each slot once.
            unsafe { encodings.as_mut_ptr::<u32>().add(row).write(tag) };
            data.insert(row, *bytes);
        }
    }
    fn read(chunk: &DataChunkHandle) -> Vec<Option<Vec<u8>>> {
        let b = data_chunk_to_arrow(chunk).unwrap();
        Storage::new(EncodingIdentity::Cp1252, 8, 16)
            .unwrap()
            .read_batch(b.column(0).as_ref())
            .unwrap()
            .into_iter()
            .map(|v| v.map(|v| v.into_parts().1))
            .collect()
    }
    #[test]
    fn zero_rows_still_require_exact_physical_source_and_output_shapes() {
        let p = plan();
        let mut input = DataChunkHandle::new(&[storage_type()]);
        assert!(p.payloads(&mut input).unwrap().is_empty());
        let mut wrong = DataChunkHandle::new(&[Id::Blob.into()]);
        assert!(p.payloads(&mut wrong).is_err());
        let mut output = Output(DataChunkHandle::new(&[Id::Blob.into()]));
        assert!(invoke(&p, &mut input, &mut output).is_err());
        let mut output = Output(DataChunkHandle::new(&[storage_type()]));
        invoke(&p, &mut input, &mut output).unwrap();
    }
    #[test]
    fn late_tag_and_chunk_failures_leave_initialized_output_bytes_unchanged() {
        let mut input = DataChunkHandle::new(&[storage_type()]);
        fill(&input, &[1251, 1252], &[b"a", b"b"]);
        let mut output = Output(DataChunkHandle::new(&[storage_type()]));
        fill(&output.0, &[1252, 1252], &[b"left", b"right"]);
        let before = read(&output.0);
        assert!(invoke(&plan(), &mut input, &mut output).is_err());
        assert_eq!(read(&output.0), before);
        fill(&input, &[1251, 1251], &[b"ab", b"cd"]);
        let p = Plan::new(
            EncodingIdentity::Cp1251,
            ProjectionTarget::Native(EncodingIdentity::Cp1252),
            ProjectionLimits {
                input_bytes: 2,
                output_bytes: 2,
            },
            4,
            2,
        )
        .unwrap();
        assert!(invoke(&p, &mut input, &mut output).is_err());
        assert_eq!(read(&output.0), before);
    }
}
