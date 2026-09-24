//! Materialize integer sql_variant values without losing the base type.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub use msduck_sql::variant_pack::*;

struct Pack;
impl VScalar for Pack {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let kind = logical.id();
        let tagged = kind == Id::Struct
            && matches!(logical.num_children(), 2 | 3)
            && logical.child_name(0) == "__msduck_variant_type"
            && logical.child(0).id() == Id::UTinyint
            && logical.child_name(1) == "__msduck_variant_integer"
            && logical.child(1).id() == Id::Bigint;
        if !tagged
            && !matches!(
                kind,
                Id::UTinyint | Id::Smallint | Id::Integer | Id::Bigint | Id::Boolean | Id::SqlNull
            )
        {
            return Err("unsupported sql_variant input base type".into());
        }
        let structure = tagged.then(|| input.struct_vector(0));
        let source_tags = structure.as_ref().map(|s| s.child(0, len));
        let source_values = structure.as_ref().map(|s| s.child(1, len));
        let mut result = output.struct_vector();
        let mut tags = result.child(0, len);
        let mut values = result.child(1, len);
        for row in 0..len {
            if source.row_is_null(row as u64) {
                result.set_null(row);
                tags.set_null(row);
                values.set_null(row);
                continue;
            }
            let (tag, value) = unsafe {
                if let (Some(tags), Some(values)) = (&source_tags, &source_values) {
                    if tags.row_is_null(row as u64) || values.row_is_null(row as u64) {
                        return Err("unsupported or invalid integer sql_variant payload".into());
                    }
                    let tag = tags.as_slice_with_len::<u8>(len)[row];
                    if !matches!(tag, 48 | 52 | 56 | 104 | 127) {
                        return Err("unsupported sql_variant input base type".into());
                    }
                    (tag, values.as_slice_with_len::<i64>(len)[row])
                } else {
                    match kind {
                        Id::Boolean => (
                            104,
                            i64::from(source.as_slice_with_len::<u8>(len)[row] != 0),
                        ),
                        Id::UTinyint => (48, i64::from(source.as_slice_with_len::<u8>(len)[row])),
                        Id::Smallint => (52, i64::from(source.as_slice_with_len::<i16>(len)[row])),
                        Id::Integer => (56, i64::from(source.as_slice_with_len::<i32>(len)[row])),
                        Id::Bigint => (127, source.as_slice_with_len::<i64>(len)[row]),
                        _ => return Err("invalid non-NULL sql_variant source".into()),
                    }
                }
            };
            unsafe {
                tags.as_mut_slice::<u8>()[row] = tag;
                values.as_mut_slice::<i64>()[row] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            LogicalTypeHandle::struct_type(&[
                ("__msduck_variant_type", Id::UTinyint.into()),
                ("__msduck_variant_integer", Id::Bigint.into()),
            ]),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Pack>("__msduck_pack_integer_variant")
}

#[cfg(test)]
mod tests {
    #[test]
    fn materialized_variants_survive_restart() {
        let path = std::env::temp_dir().join(format!(
            "msduck-packed-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for first in [true, false] {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            if first {
                for sql in [
                    "SELECT CAST(CAST(9223372036854775807 AS BIGINT) AS SQL_VARIANT) AS v INTO packed",
                    "CREATE VIEW packed_view AS SELECT v FROM packed",
                    "CREATE TABLE declared_variant(v SQL_VARIANT DEFAULT CAST(7 AS SMALLINT),b SQL_VARIANT DEFAULT CAST(1 AS BIT))",
                ] {
                    assert!(
                        session
                            .batch_response(sql, &Default::default(), false, None)
                            .1,
                        "{sql}"
                    );
                }
            }
            assert_eq!(
                session
                    .db
                    .query_row(
                        "SELECT v.__msduck_variant_integer FROM dbo.packed_view",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                i64::MAX
            );
            assert!(
                session
                    .batch_response(
                        "INSERT INTO declared_variant DEFAULT VALUES",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            assert_eq!(session.db.query_row("SELECT min(v.__msduck_variant_type),max(v.__msduck_variant_integer),count(*) FROM dbo.declared_variant",[],|r|Ok((r.get::<_,u8>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap(),(52,7,if first {1} else {2}));
            assert_eq!(session.db.query_row("SELECT min(b.__msduck_variant_type),min(b.__msduck_variant_integer) FROM dbo.declared_variant",[],|r|Ok((r.get::<_,u8>(0)?,r.get::<_,i64>(1)?))).unwrap(),(104,1));
            assert!(
                session
                    .batch_response(
                        "SELECT b,CAST(b AS BIT) FROM declared_variant",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            assert_eq!(session.db.query_row("SELECT user_type_id FROM sys.columns WHERE object_id=__msduck_object_id('packed',NULL)",[],|r|r.get::<_,i32>(0)).unwrap(),98);
            assert!(session.batch_response("SELECT v,CAST(v AS BIGINT),SQL_VARIANT_PROPERTY(v,'BaseType') FROM packed_view",&Default::default(),false,None).1);
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn packing_retains_nulls_and_evaluates_volatile_input_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE bit_calls").unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_variant_integer(__msduck_pack_integer_variant(CASE WHEN nextval('bit_calls')%2=0 THEN true ELSE NULL END))=1",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('bit_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        db.execute_batch("CREATE SEQUENCE pack_calls").unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_variant_integer(__msduck_pack_integer_variant(CASE WHEN nextval('pack_calls')%2=0 THEN 42 ELSE NULL END))=42",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('pack_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
