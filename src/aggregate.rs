//! Lower known integer SUM/AVG calls to bounded native aggregates.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub use msduck_sql::aggregate::{error_number, mark};

// Retained for stored views created before native bounded aggregation.

pub struct SumResult<const BIG: bool>;
impl<const BIG: bool> VScalar for SumResult<BIG> {
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
            // HUGEINT uses DuckDB's two-word signed 128-bit representation.
            let value =
                unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[index] };
            let value = (i128::from(value.upper) << 64) | i128::from(value.lower);
            if BIG {
                let value = i64::try_from(value).map_err(
                    |_| "Arithmetic overflow error converting expression to data type bigint.",
                )?;
                unsafe {
                    result.as_mut_slice_with_len::<i64>(len)[index] = value;
                }
            } else {
                let value = i32::try_from(value).map_err(
                    |_| "Arithmetic overflow error converting expression to data type int.",
                )?;
                unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[index] = value;
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Hugeint.into()],
            if BIG {
                LogicalTypeId::Bigint.into()
            } else {
                LogicalTypeId::Integer.into()
            },
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn offset_distinct_count_evaluates_each_nullable_input_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE offset_count_calls")
            .unwrap();
        let sql = "SELECT COUNT(DISTINCT CAST(CASE WHEN nextval('offset_count_calls')%5=0 THEN NULL WHEN i%2=0 THEN '2024-01-01T12:00:00.1234567+05:30' ELSE '2024-01-01T01:30:00.1234567-05:00' END AS DATETIMEOFFSET(7))) AS n INTO dbo.offset_count FROM range(6000) r(i)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT n FROM dbo.offset_count", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('offset_count_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn variant_distinct_count_evaluates_each_nullable_input_once() {
        struct Mark;
        impl sqlparser::ast::VisitorMut for Mark {
            type Break = String;
            fn pre_visit_expr(
                &mut self,
                expr: &mut sqlparser::ast::Expr,
            ) -> std::ops::ControlFlow<String> {
                match super::mark(expr, &Default::default()) {
                    Ok(()) => std::ops::ControlFlow::Continue(()),
                    Err(error) => std::ops::ControlFlow::Break(error),
                }
            }
        }
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE count_calls").unwrap();
        let mut statement = sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::GenericDialect {}, "SELECT count(DISTINCT __msduck_pack_integer_variant(CASE WHEN nextval('count_calls')%5=0 THEN NULL ELSE __msduck_identity_variant(CASE WHEN i%2=0 THEN 48 ELSE 127 END,i%3+1) END)) FROM range(6000) r(i)").unwrap().remove(0);
        assert!(sqlparser::ast::VisitMut::visit(&mut statement, &mut Mark).is_continue());
        assert_eq!(
            db.query_row(&statement.to_string(), [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row("SELECT currval('count_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn final_sum_widths_nulls_and_signed_hugeints() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT count(*) FROM (SELECT CAST(CASE WHEN n%17=0 THEN NULL ELSE n-3000 END AS HUGEINT) n FROM range(6000) r(n)) WHERE __msduck_sum_int_result(n) IS DISTINCT FROM CAST(n AS INT) OR __msduck_sum_big_result(n*3000000000000000) IS DISTINCT FROM CAST(n*3000000000000000 AS BIGINT)", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        for (name, value) in [
            ("int", "2147483648"),
            ("int", "-2147483649"),
            ("big", "9223372036854775808"),
            ("big", "-9223372036854775809"),
        ] {
            let error = db
                .query_row(
                    &format!("SELECT __msduck_sum_{name}_result(CAST('{value}' AS HUGEINT))"),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_err();
            assert!(error.to_string().contains("Arithmetic overflow"));
        }
    }
}
