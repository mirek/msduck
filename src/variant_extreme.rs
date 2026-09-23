//! Numeric-first aggregate keys retain the chosen variant's original base type.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
fn kind(reverse: bool) -> LogicalTypeHandle {
    if reverse {
        LogicalTypeHandle::struct_type(&[
            ("__msduck_variant_order", Id::Bigint.into()),
            ("__msduck_variant_tag", Id::UTinyint.into()),
        ])
    } else {
        LogicalTypeHandle::struct_type(&[
            ("__msduck_variant_type", Id::UTinyint.into()),
            ("__msduck_variant_integer", Id::Bigint.into()),
        ])
    }
}
struct Reorder<const RESTORE: bool>;
impl<const RESTORE: bool> VScalar for Reorder<RESTORE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = input.struct_vector(0);
        let tag_index = usize::from(RESTORE);
        let tags = structure.child(tag_index, len);
        let values = structure.child(1 - tag_index, len);
        let mut result = output.struct_vector();
        let mut out_tags = result.child(1 - tag_index, len);
        let mut out_values = result.child(tag_index, len);
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                out_tags.set_null(row);
                out_values.set_null(row);
                continue;
            }
            if tags.row_is_null(row as u64) || values.row_is_null(row as u64) {
                return Err("invalid variant extremum payload".into());
            }
            unsafe {
                let tag = tags.as_slice_with_len::<u8>(len)[row];
                if !matches!(tag, 48 | 52 | 56 | 104 | 127) {
                    return Err("unsupported variant extremum base type".into());
                }
                out_tags.as_mut_slice::<u8>()[row] = tag;
                out_values.as_mut_slice::<i64>()[row] = values.as_slice_with_len::<i64>(len)[row];
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![kind(RESTORE)],
            kind(!RESTORE),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Reorder<false>>("__msduck_variant_extreme_input")?;
    db.register_scalar_function::<Reorder<true>>("__msduck_variant_extreme_output")
}

#[cfg(test)]
mod tests {
    #[test]
    fn extrema_preserve_tags_and_single_evaluation_across_chunks() {
        for (op, expected) in [("min", (52, -3000)), ("max", (127, 2999))] {
            let server = crate::server::Server::open(":memory:").unwrap();
            let db = server.connection().unwrap();
            db.execute_batch("CREATE SEQUENCE extreme_calls").unwrap();
            let sql = format!(
                "SELECT v.__msduck_variant_type,v.__msduck_variant_integer FROM (SELECT __msduck_variant_extreme_output({op}(__msduck_variant_extreme_input(__msduck_pack_integer_variant(CASE WHEN nextval('extreme_calls')%7=0 THEN NULL ELSE __msduck_identity_variant(CASE WHEN i%2=0 THEN 52 ELSE 127 END,i-3000) END)))) AS v FROM range(6000) r(i)) q"
            );
            assert_eq!(
                db.query_row(&sql, [], |r| Ok((r.get::<_, u8>(0)?, r.get::<_, i64>(1)?)))
                    .unwrap(),
                expected
            );
            assert_eq!(
                db.query_row("SELECT currval('extreme_calls')", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
            let sql = format!(
                "SELECT __msduck_variant_extreme_output({op}(__msduck_variant_extreme_input(__msduck_pack_integer_variant(NULL)))) IS NULL FROM range(0)"
            );
            assert!(db.query_row(&sql, [], |r| r.get::<_, bool>(0)).unwrap());
        }
    }
}
