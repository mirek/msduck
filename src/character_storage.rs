//! Character target conversion for storage (not truncating expression casts).
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
pub use msduck_sql::character_storage::*;

struct Store<const UNICODE: bool, const FIXED: bool>;
impl<const UNICODE: bool, const FIXED: bool> VScalar for Store<UNICODE, FIXED> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let widths = input.flat_vector(1);
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) || widths.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            // Exact VARCHAR/INTEGER signature; both reads are bounded by this chunk.
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let mut text =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut text).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(text) as usize,
                )
            };
            let value = target(UNICODE, FIXED, width)?.store(std::str::from_utf8(bytes)?)?;
            result.insert(row, value.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Integer.into()],
            Id::Varchar.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Store<false, false>>("__msduck_store_varchar")?;
    db.register_scalar_function::<Store<false, true>>("__msduck_store_char")?;
    db.register_scalar_function::<Store<true, false>>("__msduck_store_nvarchar")?;
    db.register_scalar_function::<Store<true, true>>("__msduck_store_nchar")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_character_targets_and_defaults_survive_restart() {
        let path = std::env::temp_dir().join(format!(
            "msduck-character-store-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            assert!(!session.batch_response("CREATE TABLE dbo.saved_chars(v VARCHAR(3),n NCHAR(3) DEFAULT N'x'); INSERT INTO dbo.saved_chars(v) VALUES('abc'); BEGIN TRAN; ALTER TABLE dbo.saved_chars ALTER COLUMN v VARCHAR(2); ROLLBACK",&Default::default(),false,None).1);
            // The failing shrink keeps the original declaration and row.
            assert!(
                session
                    .batch_response("ROLLBACK", &Default::default(), false, None)
                    .1
            );
        }
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            assert!(
                !session
                    .batch_response(
                        "INSERT INTO dbo.saved_chars(v) VALUES('abcd')",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            assert!(
                session
                    .batch_response(
                        "INSERT INTO dbo.saved_chars(v) VALUES('xy')",
                        &Default::default(),
                        false,
                        None
                    )
                    .1
            );
            let rows = session
                .db
                .prepare("SELECT v,n FROM dbo.saved_chars ORDER BY v")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                rows,
                vec![("abc".into(), "x  ".into()), ("xy".into(), "x  ".into())]
            );
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn vector_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE store_calls").unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM (SELECT __msduck_store_nchar(CASE WHEN nextval('store_calls')%17=0 THEN NULL ELSE '🦆' END,3) v,i FROM range(6000) t(i)) WHERE v IS DISTINCT FROM CASE WHEN (i+1)%17=0 THEN NULL ELSE '🦆 ' END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        let calls: i64 = db
            .query_row("SELECT currval('store_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }
}
