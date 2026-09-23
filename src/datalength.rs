//! SQL storage byte counts; backend UTF-8 byte lengths are not SQL character lengths.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub use msduck_sql::datalength::*;

pub struct Bytes<const MODE: u8>;
impl<const MODE: u8> VScalar for Bytes<MODE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // Exact signatures establish string/blob storage. Keep copied inline
            // storage alive and borrow only initialized, non-null values.
            let mut value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[index] };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                    duckdb::ffi::duckdb_string_t_length(value) as usize,
                )
            };
            let size = match MODE {
                0 => msduck_core::encoding::encode_cp1252(std::str::from_utf8(bytes)?)?.len(),
                1 => std::str::from_utf8(bytes)?.encode_utf16().count() * 2,
                _ => bytes.len(),
            };
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[index] = i64::try_from(size)?;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                if MODE == 2 {
                    LogicalTypeId::Blob
                } else {
                    LogicalTypeId::Varchar
                }
                .into(),
            ],
            LogicalTypeId::Bigint.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn temporal_lengths_keep_nulls_across_chunks_and_evaluate_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("CREATE TABLE dbo.temporal_length_vectors(id INT,t TIME(2),d DATETIME2(3),o DATETIMEOFFSET(5)); INSERT INTO dbo.temporal_length_vectors SELECT CAST(i AS INT),CASE WHEN i%17=0 THEN NULL ELSE '12:34:56.12' END,CASE WHEN i%17=0 THEN NULL ELSE '2024-02-29T12:34:56.123' END,CASE WHEN i%17=0 THEN NULL ELSE '2024-02-29T12:34:56.12345+05:30' END FROM range(6000) r(i); SELECT id,DATALENGTH(t) AS t,DATALENGTH(d) AS d,DATALENGTH(o) AS o INTO dbo.temporal_length_outputs FROM dbo.temporal_length_vectors", &Default::default(), false, None).1);
        let wrong:i64=session.db.query_row("SELECT count(*) FROM dbo.temporal_length_outputs WHERE t IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 3 END OR d IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 7 END OR o IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 10 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        session
            .db
            .execute_batch("CREATE SEQUENCE temporal_length_input")
            .unwrap();
        assert!(session.batch_response("SELECT SUM(DATALENGTH(TIMEFROMPARTS(1,2,CAST(nextval('temporal_length_input')%60 AS INT),0,2))) FROM range(6000)", &Default::default(), false, None).1);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('temporal_length_input')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn numeric_lengths_use_sql_widths_across_chunks_and_evaluate_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("CREATE TABLE dbo.numeric_length_vectors(id INT,d DECIMAL(28,2),m MONEY,r REAL); INSERT INTO dbo.numeric_length_vectors SELECT CAST(i AS INT),CASE WHEN i%17=0 THEN NULL ELSE i END,CASE WHEN i%17=0 THEN NULL ELSE i END,CASE WHEN i%17=0 THEN NULL ELSE i END FROM range(6000) r(i); SELECT id,DATALENGTH(d) AS d,DATALENGTH(m) AS m,DATALENGTH(r) AS r INTO dbo.numeric_length_outputs FROM dbo.numeric_length_vectors", &Default::default(), false, None).1);
        let wrong:i64=session.db.query_row("SELECT count(*) FROM dbo.numeric_length_outputs WHERE d IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 13 END OR m IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 8 END OR r IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 4 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        session
            .db
            .execute_batch("CREATE SEQUENCE numeric_length_input")
            .unwrap();
        assert!(session.batch_response("SELECT SUM(DATALENGTH(CAST(nextval('numeric_length_input') AS DECIMAL(28,2)))) FROM range(6000)", &Default::default(), false, None).1);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('numeric_length_input')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn catalog_lengths_cross_chunks_and_keep_volatile_inputs_single() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let sql = "CREATE TABLE dbo.length_vectors(id INT,v VARCHAR(MAX),n NVARCHAR(MAX)); INSERT INTO dbo.length_vectors SELECT CAST(i AS INT),CASE WHEN i%17=0 THEN NULL ELSE ' a ' END,CASE WHEN i%17=0 THEN NULL ELSE N' 🦆 ' END FROM range(6000) r(i); SELECT id,LEN(UPPER(v)) AS a,DATALENGTH(TRIM(n)) AS b INTO dbo.length_outputs FROM dbo.length_vectors";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        let wrong:i64=session.db.query_row("SELECT count(*) FROM dbo.length_outputs WHERE a IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 2 END OR b IS DISTINCT FROM CASE WHEN id%17=0 THEN NULL ELSE 4 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        session
            .db
            .execute_batch("CREATE SEQUENCE length_input")
            .unwrap();
        assert!(session.batch_response("SELECT SUM(LEN(LOWER(CAST(nextval('length_input') AS VARCHAR(MAX))))) FROM range(6000)", &Default::default(), false, None).1);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('length_input')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn byte_counts_cross_chunks_keep_nulls_and_evaluate_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64=db.query_row("SELECT count(*) FROM range(6000) t(n) WHERE __msduck_datalength_unicode(CASE WHEN n%17=0 THEN NULL ELSE '🦆 ' || repeat('a',CAST(n%50 AS INT)) END) IS DISTINCT FROM CASE WHEN n%17=0 THEN NULL ELSE 6+2*(n%50) END OR __msduck_datalength_ansi('€ ' || repeat('a',CAST(n%50 AS INT))) <> 2+n%50 OR __msduck_datalength_binary(from_hex('0020ff')) <> 3",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE byte_seq").unwrap();
        let count:i64=db.query_row("SELECT count(*) FROM (SELECT __msduck_datalength_unicode(CAST(nextval('byte_seq') AS VARCHAR)) AS n FROM range(6000)) WHERE n > 0",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('byte_seq')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
