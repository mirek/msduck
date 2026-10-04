//! Connection-owned projections over explicitly identified native carriers.
use super::{Plan as Storage, is_logical, storage_type, with_native_bytes};
use duckdb::{
    Connection,
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::{
    ansi_bytes::{AnsiView, EncodingIdentity, checked_byte_total},
    ansi_conversion::{
        ProjectedValue, ProjectionLimits, ProjectionTarget, capacity, project, stored_utf8,
    },
};

/// BulkLoad declaration facts, supplied after root wire/catalog admission.
/// Encoding and source form must never be inferred from a row's bytes or size.
#[derive(Clone, Copy, Debug)]
pub struct CapacityDeclaration {
    pub source: EncodingIdentity,
    pub source_form: capacity::SourceForm,
    pub target: ProjectionTarget,
    pub family: capacity::Family,
    pub capacity: capacity::Capacity,
}

/// Explicit logical identity and active per-cell/chunk input/output payload limits.
/// This is not a collation, SQL CAST or wire-admission plan.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    source: Storage,
    target: ProjectionTarget,
    limits: ProjectionLimits,
    output_chunk_limit: usize,
    decode: Decode,
}

#[derive(Clone, Copy, Debug)]
enum Decode {
    Strict,
    StoredUtf8,
    BulkCapacity(capacity::Plan),
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
        Self::validated_storage(
            source,
            target,
            limits,
            input_chunk_limit,
            output_chunk_limit,
        )
    }

    // Callers validate their own semantic domain before constructing storage.
    // Capacity conversion admits UTF8-to-codepage plans outside strict project.
    fn validated_storage(
        source: EncodingIdentity,
        target: ProjectionTarget,
        limits: ProjectionLimits,
        input_chunk_limit: usize,
        output_chunk_limit: usize,
    ) -> anyhow::Result<Self> {
        if target == ProjectionTarget::SqlUtf16 {
            anyhow::ensure!(
                limits.output_bytes <= crate::unicode_carrier::CELL_LIMIT
                    && output_chunk_limit <= crate::unicode_carrier::CHUNK_LIMIT,
                "ANSI projection exceeds the existing UTF16 carrier limits"
            );
        }
        anyhow::ensure!(
            limits.output_bytes <= output_chunk_limit,
            "ANSI projection output cell limit exceeds chunk limit"
        );
        Ok(Self {
            source: Storage::new(source, limits.input_bytes, input_chunk_limit)?,
            target,
            limits,
            output_chunk_limit,
            decode: Decode::Strict,
        })
    }

    /// SQL UTF16 projection of stored UTF8 bytes, including measured EOF fitting
    /// and malformed repair. This does not admit a wire value or apply capacity.
    /// Source and target identity are fixed declaration facts, even for NULL.
    pub fn stored_utf8_to_sql_utf16(
        limits: ProjectionLimits,
        input_chunk_limit: usize,
        output_chunk_limit: usize,
    ) -> anyhow::Result<Self> {
        let mut plan = Self::new(
            EncodingIdentity::Utf8,
            ProjectionTarget::SqlUtf16,
            limits,
            input_chunk_limit,
            output_chunk_limit,
        )?;
        plan.decode = Decode::StoredUtf8;
        Ok(plan)
    }

    /// Apply measured BulkLoad capacity rules to an already admitted source.
    /// This callback does not establish wire admission, SQL error tokens or
    /// whole-load transaction behavior. Source form and target capacity stay explicit.
    pub fn bulk_capacity(
        declaration: CapacityDeclaration,
        limits: ProjectionLimits,
        input_chunk_limit: usize,
        output_chunk_limit: usize,
    ) -> anyhow::Result<Self> {
        let capacity = capacity::Plan::new(
            declaration.source,
            declaration.source_form,
            declaration.target,
            declaration.family,
            declaration.capacity,
        )?;
        let mut plan = Self::validated_storage(
            declaration.source,
            declaration.target,
            limits,
            input_chunk_limit,
            output_chunk_limit,
        )?;
        plan.decode = Decode::BulkCapacity(capacity);
        Ok(plan)
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
                let projected = match self.decode {
                    Decode::Strict => {
                        project(self.source.encoding(), Some(view), self.target, self.limits)?
                    }
                    Decode::StoredUtf8 => {
                        stored_utf8::to_sql_utf16(self.source.encoding(), Some(view), self.limits)?
                            .map(ProjectedValue::SqlUtf16)
                    }
                    Decode::BulkCapacity(plan) => plan.apply(Some(view), self.limits)?,
                }
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
    fn unicode_values(chunk: &DataChunkHandle) -> Vec<duckdb::types::Value> {
        let batch = data_chunk_to_arrow(chunk).unwrap();
        (0..batch.num_rows())
            .map(|row| crate::unicode_carrier::read(batch.column(0).as_ref(), row).unwrap())
            .collect()
    }
    #[test]
    fn stored_boundary_and_resource_failures_preserve_initialized_unicode_output() {
        use msduck_core::ansi_bytes::ByteError;
        use msduck_core::ansi_conversion::stored_utf8::StoredUtf8Error;
        let stored = |input, output, input_chunk, output_chunk| {
            Plan::stored_utf8_to_sql_utf16(
                ProjectionLimits {
                    input_bytes: input,
                    output_bytes: output,
                },
                input_chunk,
                output_chunk,
            )
            .unwrap()
        };
        let mut input = DataChunkHandle::new(&[storage_type()]);
        let mut output = Output(DataChunkHandle::new(&[crate::unicode_carrier::kind()]));
        output.0.set_len(2);
        let structure = output.0.struct_vector(0);
        let payload = structure.child(0, 2);
        payload.insert(0, b"L\0".as_slice());
        payload.insert(1, b"R\0".as_slice());
        let before = unicode_values(&output.0);
        fill(&input, &[65001, 65001], &[b"A", &[0x80]]);
        let error = invoke(&stored(8, 8, 16, 16), &mut input, &mut output).unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoredUtf8Error>(),
            Some(&StoredUtf8Error::InvalidBoundary)
        );
        assert_eq!(unicode_values(&output.0), before);
        fill(&input, &[65001, 65001], &[b"A", b"B"]);
        let error = invoke(&stored(1, 2, 2, 3), &mut input, &mut output).unwrap_err();
        assert_eq!(
            error.downcast_ref::<ByteError>(),
            Some(&ByteError::Limit {
                requested: 4,
                maximum: 3
            })
        );
        assert_eq!(unicode_values(&output.0), before);
        fill(&input, &[65001, 65001], &[b"A", &[0xc2, 0x80]]);
        let error = invoke(&stored(1, 2, 2, 4), &mut input, &mut output).unwrap_err();
        assert_eq!(
            error.downcast_ref::<ByteError>(),
            Some(&ByteError::Limit {
                requested: 2,
                maximum: 1
            })
        );
        assert_eq!(unicode_values(&output.0), before);
        fill(&input, &[65001, 1252], &[b"A", b"B"]);
        assert!(invoke(&stored(8, 8, 16, 16), &mut input, &mut output).is_err());
        assert_eq!(unicode_values(&output.0), before);
        fill(&input, &[65001, 65001], &[b"A", b"B"]);
        input.struct_vector(0).child(1, 2).set_null(1);
        assert!(invoke(&stored(8, 8, 16, 16), &mut input, &mut output).is_err());
        assert_eq!(unicode_values(&output.0), before);
    }
    #[test]
    fn stored_zero_rows_require_exact_source_and_unicode_output_shapes() {
        let p = Plan::stored_utf8_to_sql_utf16(
            ProjectionLimits {
                input_bytes: 0,
                output_bytes: 0,
            },
            0,
            0,
        )
        .unwrap();
        let mut input = DataChunkHandle::new(&[storage_type()]);
        let mut output = Output(DataChunkHandle::new(&[crate::unicode_carrier::kind()]));
        invoke(&p, &mut input, &mut output).unwrap();
        let mut wrong_input = DataChunkHandle::new(&[Id::Blob.into()]);
        assert!(invoke(&p, &mut wrong_input, &mut output).is_err());
        let mut wrong_output = Output(DataChunkHandle::new(&[storage_type()]));
        assert!(invoke(&p, &mut input, &mut wrong_output).is_err());
    }

    fn capacity_plan(target: ProjectionTarget, output: usize, chunk: usize) -> Plan {
        Plan::bulk_capacity(
            CapacityDeclaration {
                source: EncodingIdentity::Cp1252,
                source_form: capacity::SourceForm::Bounded,
                target,
                family: capacity::Family::Fixed,
                capacity: capacity::Capacity::Bounded(2),
            },
            ProjectionLimits {
                input_bytes: 8,
                output_bytes: output,
            },
            16,
            chunk,
        )
        .unwrap()
    }
    #[test]
    fn capacity_late_failures_preserve_initialized_native_and_unicode_outputs() {
        use capacity::CapacityError;
        use msduck_core::ansi_bytes::ByteError;
        use msduck_core::ansi_conversion::{ProjectionError, Resource};
        for target in [
            ProjectionTarget::Native(EncodingIdentity::Cp1252),
            ProjectionTarget::SqlUtf16,
        ] {
            let (kind, cell) = if target == ProjectionTarget::SqlUtf16 {
                (crate::unicode_carrier::kind(), 4)
            } else {
                (storage_type(), 2)
            };
            let mut input = DataChunkHandle::new(&[storage_type()]);
            let mut output = Output(DataChunkHandle::new(&[kind]));
            if target == ProjectionTarget::SqlUtf16 {
                output.0.set_len(2);
                let structure = output.0.struct_vector(0);
                let payload = structure.child(0, 2);
                payload.insert(0, b"L\0".as_slice());
                payload.insert(1, b"R\0".as_slice());
            } else {
                fill(&output.0, &[1252, 1252], &[b"left", b"right"]);
            }
            let before = data_chunk_to_arrow(&output.0).unwrap();
            fill(&input, &[1252, 1252], &[b"A", b"ABC"]);
            let error = invoke(
                &capacity_plan(target, cell, cell * 2),
                &mut input,
                &mut output,
            )
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<CapacityError>(),
                Some(&CapacityError::Truncation {
                    source_bytes: 3,
                    target_capacity: 2
                })
            );
            assert_eq!(data_chunk_to_arrow(&output.0).unwrap(), before);
            fill(&input, &[1252, 1252], &[b"A", b"B"]);
            let error = invoke(
                &capacity_plan(target, cell - 1, cell * 2),
                &mut input,
                &mut output,
            )
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<CapacityError>(),
                Some(&CapacityError::Projection(ProjectionError::Limit {
                    resource: Resource::Output,
                    requested: cell,
                    maximum: cell - 1
                }))
            );
            assert_eq!(data_chunk_to_arrow(&output.0).unwrap(), before);
            let error = invoke(
                &capacity_plan(target, cell, cell * 2 - 1),
                &mut input,
                &mut output,
            )
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<ByteError>(),
                Some(&ByteError::Limit {
                    requested: cell * 2,
                    maximum: cell * 2 - 1
                })
            );
            assert_eq!(data_chunk_to_arrow(&output.0).unwrap(), before);
            fill(&input, &[1252, 1251], &[b"A", b"B"]);
            assert!(
                invoke(
                    &capacity_plan(target, cell, cell * 2),
                    &mut input,
                    &mut output
                )
                .is_err()
            );
            assert_eq!(data_chunk_to_arrow(&output.0).unwrap(), before);
            fill(&input, &[1252, 1252], &[b"A", b"B"]);
            input.struct_vector(0).child(1, 2).set_null(1);
            assert!(
                invoke(
                    &capacity_plan(target, cell, cell * 2),
                    &mut input,
                    &mut output
                )
                .is_err()
            );
            assert_eq!(data_chunk_to_arrow(&output.0).unwrap(), before);
        }
    }
    #[test]
    fn capacity_zero_rows_still_validate_exact_declared_physical_shapes() {
        for target in [
            ProjectionTarget::Native(EncodingIdentity::Cp1252),
            ProjectionTarget::SqlUtf16,
        ] {
            let p = capacity_plan(target, 4, 8);
            let mut input = DataChunkHandle::new(&[storage_type()]);
            let mut output = Output(DataChunkHandle::new(&[
                if target == ProjectionTarget::SqlUtf16 {
                    crate::unicode_carrier::kind()
                } else {
                    storage_type()
                },
            ]));
            invoke(&p, &mut input, &mut output).unwrap();
            let mut wrong = DataChunkHandle::new(&[Id::Blob.into()]);
            assert!(invoke(&p, &mut wrong, &mut output).is_err());
            let mut wrong = Output(DataChunkHandle::new(&[Id::Blob.into()]));
            assert!(invoke(&p, &mut input, &mut wrong).is_err());
        }
    }
}
