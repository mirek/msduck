//! Explicit casts from integer variants; implicit assignments stay separate.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
pub use msduck_sql::variant_cast::take;
struct Integer;
impl VScalar for Integer {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        if logical.id() != Id::Struct
            || !matches!(logical.num_children(), 2 | 3)
            || logical.child_name(0) != "__msduck_variant_type"
            || logical.child(0).id() != Id::UTinyint
            || logical.child_name(1) != "__msduck_variant_integer"
            || logical.child(1).id() != Id::Bigint
        {
            return Err("unsupported integer sql_variant conversion".into());
        }
        let structure = input.struct_vector(0);
        let tags = structure.child(0, len);
        let values = structure.child(1, len);
        let mut out = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                out.set_null(row);
                continue;
            }
            if tags.row_is_null(row as u64) || values.row_is_null(row as u64) {
                return Err("unsupported or invalid integer sql_variant payload".into());
            }
            let tag = unsafe { tags.as_slice_with_len::<u8>(len)[row] };
            if !matches!(tag, 48 | 52 | 56 | 104 | 127) {
                return Err("unsupported integer sql_variant base type".into());
            }
            unsafe {
                out.as_mut_slice::<i64>()[row] = values.as_slice_with_len::<i64>(len)[row];
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            Id::Bigint.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Integer>("__msduck_variant_integer")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_explicit_bit(value) AS CASE WHEN starts_with(typeof(value),'STRUCT(__msduck_variant_type ') THEN __msduck_variant_integer(value)<>0 ELSE CAST(value AS BOOLEAN) END;
        CREATE OR REPLACE MACRO main.__msduck_explicit_try_bit(value) AS CASE WHEN starts_with(typeof(value),'STRUCT(__msduck_variant_type ') THEN __msduck_variant_integer(value)<>0 ELSE TRY_CAST(value AS BOOLEAN) END")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_explicit_integer_input(value,target,try_mode) AS CASE WHEN starts_with(typeof(value),'STRUCT(__msduck_variant_type ') THEN __msduck_integer_input(__msduck_variant_integer(value),target,try_mode) ELSE __msduck_integer_input(value,target,try_mode) END")
}

#[cfg(test)]
mod tests {
    #[test]
    fn variant_casts_evaluate_each_input_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE variant_calls; CREATE SEQUENCE ordinary_calls")
            .unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_explicit_integer_input(CASE WHEN nextval('variant_calls')%2=0 THEN __msduck_identity_variant(56,42) ELSE NULL END,'INT',false)='42'",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('variant_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_explicit_integer_input(CASE WHEN nextval('ordinary_calls')%2=0 THEN 42 ELSE NULL END,'INT',false)='42'",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('ordinary_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
