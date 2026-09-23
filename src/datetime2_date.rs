//! Extract local DATE and TIME components from exact temporal payloads.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub fn lower(expr: &mut Expr) {
    let (value, trying) = match expr {
        Expr::Cast {
            expr: value,
            data_type: DataType::Date,
            kind,
            ..
        } => (
            value,
            matches!(kind, CastKind::TryCast | CastKind::SafeCast),
        ),
        Expr::Convert {
            expr: value,
            data_type: Some(DataType::Date),
            is_try,
            styles,
            charset: None,
            ..
        } if styles.is_empty() => (value, *is_try),
        _ => return,
    };
    // RPC/local DATE parameters already carry a concrete DATE value. Retain
    // their binding cast: typeof/ANY dispatch on an untyped placeholder can
    // leave DuckDB's prepared parameter with an unresolved physical type.
    if matches!(value.as_ref(), Expr::Value(v) if matches!(v.value, Value::Placeholder(_))) {
        return;
    }
    *expr = crate::engine::unary_function(
        if trying {
            "__msduck_try_date"
        } else {
            "__msduck_cast_date"
        },
        *value.clone(),
    );
}

pub fn type_condition() -> String {
    format!(
        "typeof(value) IN ({})",
        (0..=7)
            .flat_map(|scale| [
                format!("'{}'", crate::datetime2_cast::storage_type(scale)),
                format!("'{}'", crate::datetimeoffset_cast::storage_type(scale)),
            ])
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Part<false>>("__msduck_datetime2_date")?;
    db.register_scalar_function::<Part<true>>("__msduck_datetime2_time")?;
    for (name, cast) in [("cast", "CAST"), ("try", "TRY_CAST")] {
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}_time(value) AS CASE WHEN {} THEN __msduck_datetime2_time(value) ELSE {cast}(value AS TIME_NS) END", type_condition()))?;
        db.execute_batch(&format!("CREATE OR REPLACE MACRO main.__msduck_{name}_date(value) AS CASE WHEN {} THEN __msduck_datetime2_date(value) ELSE {cast}(value AS DATE) END", type_condition()))?;
    }
    Ok(())
}

struct Part<const TIME: bool>;
impl<const TIME: bool> VScalar for Part<TIME> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let datetime = logical.id() == Id::Struct
            && logical.num_children() == 1
            && logical.child(0).id() == Id::Bigint
            && (0..=7).any(|s| logical.child_name(0) == format!("__msduck_datetime2_{s}"));
        let offset = logical.id() == Id::Struct
            && logical.num_children() == 2
            && logical.child(0).id() == Id::Bigint
            && (0..=7).any(|s| logical.child_name(0) == format!("__msduck_datetimeoffset_{s}"))
            && logical.child_name(1) == "__msduck_offset_minutes"
            && logical.child(1).id() == Id::Smallint;
        if !datetime && !offset {
            return Err("expected exact temporal input".into());
        }
        let structure = input.struct_vector(0);
        let ticks = structure.child(0, len);
        let offsets = offset.then(|| structure.child(1, len));
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) || ticks.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            // Input type and validity checked above; both vectors are flat.
            let value = crate::datetime2::DateTime2::from_ticks(unsafe {
                ticks.as_slice_with_len::<i64>(len)[row]
            })?;
            let value = if let Some(offsets) = &offsets {
                if offsets.row_is_null(row as u64) {
                    return Err("invalid DATETIMEOFFSET offset".into());
                }
                crate::datetimeoffset::DateTimeOffset::from_utc(value, unsafe {
                    offsets.as_slice_with_len::<i16>(len)[row]
                })?
                .local()
            } else {
                value
            };
            if TIME {
                let nanos = (value.ticks() % 864_000_000_000) * 100;
                unsafe {
                    result.as_mut_slice_with_len::<i64>(len)[row] = nanos;
                }
            } else {
                let parts = value.parts();
                let days = crate::scalar::date_days(
                    i32::from(parts.year),
                    i32::from(parts.month),
                    i32::from(parts.day),
                )?;
                unsafe {
                    result.as_mut_slice_with_len::<i32>(len)[row] = days;
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            if TIME { Id::TimeNs } else { Id::Date }.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn offset_components_keep_local_dates_and_ticks_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for scale in 0..=7 {
            db.execute_batch("CREATE OR REPLACE SEQUENCE offset_parts_calls")
                .unwrap();
            let sql = format!(
                "SELECT CAST(__msduck_cast_date(v) AS VARCHAR),epoch_ns(__msduck_cast_time(v)) FROM (SELECT __msduck_datetimeoffset_cast_{scale}(CASE WHEN nextval('offset_parts_calls')%3=0 THEN NULL WHEN i%2=0 THEN '2024-01-01T00:15:30.1234567+14:00' ELSE '2024-01-01T23:30:00.1234567-14:00' END) AS v FROM range(6000) r(i)) s"
            );
            let mut statement = db.prepare(&sql).unwrap();
            let rows = statement
                .query_map([], |r| {
                    Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?))
                })
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows.len(), 6000);
            for (i, (date, nanos)) in rows.into_iter().enumerate() {
                if (i + 1) % 3 == 0 {
                    assert_eq!((date, nanos), (None, None));
                    continue;
                }
                let text = if i % 2 == 0 {
                    "2024-01-01T00:15:30.1234567"
                } else {
                    "2024-01-01T23:30:00.1234567"
                };
                let expected = crate::datetime2::DateTime2::parse_iso(text)
                    .unwrap()
                    .round(scale)
                    .unwrap();
                assert_eq!(date, Some("2024-01-01".to_string()));
                assert_eq!(nanos, Some(expected.ticks() % 864_000_000_000 * 100));
            }
            assert_eq!(
                db.query_row("SELECT currval('offset_parts_calls')", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }

    #[test]
    fn time_components_keep_exact_ticks_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for scale in 0..=7 {
            let source = crate::datetime2::DateTime2::parse_iso("9999-12-31T12:34:56.1234567")
                .unwrap()
                .round(scale)
                .unwrap();
            let expected = source.ticks() % 864_000_000_000 * 100;
            let wrong: i64 = db.query_row(&format!("SELECT count(*) FROM range(6000) r(i) WHERE epoch_ns(__msduck_cast_time(__msduck_datetime2_cast_{scale}(CASE WHEN i%17=0 THEN NULL ELSE '9999-12-31T12:34:56.1234567' END))) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE {expected} END"), [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        db.execute_batch("CREATE SEQUENCE time_evaluations START 1")
            .unwrap();
        let count: i64 = db.query_row("SELECT count(__msduck_cast_time(__msduck_datetime2_cast_7(printf('0001-01-01T12:34:%02d.1234567',nextval('time_evaluations')%60)))) FROM range(6000)", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('time_evaluations')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn dates_across_chunks_keep_nulls_and_evaluate_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for scale in 0..=7 {
            let wrong: i64 = db.query_row(&format!("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_cast_date(__msduck_datetime2_cast_{scale}(CASE WHEN i%17=0 THEN NULL ELSE '2024-02-29T12:34:56.1234567' END)) IS DISTINCT FROM CASE WHEN i%17=0 THEN NULL ELSE DATE '2024-02-29' END"), [], |r| r.get(0)).unwrap();
            assert_eq!(wrong, 0);
        }
        for function in [
            "__msduck_cast_date",
            "__msduck_try_date",
            "__msduck_year",
            "__msduck_month",
            "__msduck_day",
        ] {
            db.execute_batch("CREATE OR REPLACE SEQUENCE evaluations START 1")
                .unwrap();
            let count: i64 = db.query_row(&format!("SELECT count({function}(__msduck_datetime2_cast_7(DATE '2024-01-01' + CAST(nextval('evaluations')%28 AS INTEGER)))) FROM range(6000)"), [], |r| r.get(0)).unwrap();
            assert_eq!(count, 6000);
            assert_eq!(
                db.query_row("SELECT currval('evaluations')", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000,
                "{function}"
            );
        }
    }
}
