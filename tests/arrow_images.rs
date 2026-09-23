use duckdb::{
    Connection,
    arrow::{
        compute::concat_batches,
        datatypes::{DataType, TimeUnit},
        record_batch::RecordBatch,
    },
};

#[test]
fn image_materialization_preserves_exact_times_nested_values_nulls_and_extensions() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("SET arrow_lossless_conversion=true;
        CREATE TABLE source AS SELECT i AS id,
        CASE WHEN i%7=0 THEN NULL WHEN i%2=0 THEN CAST('23:59:59.9999999' AS TIME_NS) ELSE CAST('00:00:00.0000001' AS TIME_NS) END AS nanos,
        CASE WHEN i%3=0 THEN NULL ELSE CAST('12:34:56.123456' AS TIME) END AS micros,
        CASE WHEN i%5=0 THEN NULL ELSE {'raw':from_hex('3ed8'),'time':CAST('12:34:56.1234567' AS TIME_NS)} END AS nested,
        CAST(i/10000.0 AS DECIMAL(19,4)) AS money,
        CASE WHEN i%3=0 THEN NULL ELSE i%2=0 END AS flag,
        CASE WHEN i%2=0 THEN NULL ELSE CAST('01234567-89ab-cdef-0123-456789abcdef' AS UUID) END AS guid
        FROM range(6001) r(i);
        CREATE TEMP TABLE images AS SELECT * FROM source WHERE false").unwrap();
    let batches: Vec<RecordBatch> = db
        .prepare("SELECT * FROM source ORDER BY id")
        .unwrap()
        .query_arrow([])
        .unwrap()
        .collect();
    let schema = batches[0].schema();
    assert_eq!(
        schema.field(1).data_type(),
        &DataType::Time64(TimeUnit::Nanosecond)
    );
    assert_eq!(
        schema.field(2).data_type(),
        &DataType::Time64(TimeUnit::Microsecond)
    );
    let expected = concat_batches(&schema, &batches).unwrap();
    {
        let mut appender = db.appender("images").unwrap();
        // A batch larger than DuckDB's native vector capacity must be split
        // without changing validity bits, units or nested extension metadata.
        appender.append_record_batch(expected.clone()).unwrap();
        appender.flush().unwrap();
    }
    let restored: Vec<RecordBatch> = db
        .prepare("SELECT * FROM images ORDER BY id")
        .unwrap()
        .query_arrow([])
        .unwrap()
        .collect();
    let actual = concat_batches(&restored[0].schema(), &restored).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual.num_rows(), 6001);
    let types = db
        .prepare("SELECT nanos,micros FROM images")
        .unwrap()
        .prepared_columns()
        .unwrap();
    assert_eq!(types[0].1.id(), duckdb::core::LogicalTypeId::TimeNs);
    assert_eq!(types[1].1.id(), duckdb::core::LogicalTypeId::Time);
}
