//! FIRST_VALUE/LAST_VALUE signatures and NULL-treatment syntax translation.
use crate::parameter::Parameter;
use sqlparser::ast::*;
use std::collections::HashMap;

pub use msduck_sql::expression_metadata::temporal::window_argument as argument;

pub fn lower(expr: &mut Expr, parameters: &HashMap<String, Parameter>) -> Result<(), String> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let name = function.name.to_string().to_ascii_lowercase();
    if !matches!(name.as_str(), "first_value" | "last_value" | "lag" | "lead") {
        return Ok(());
    }
    let offset = matches!(name.as_str(), "lag" | "lead");
    if argument(function).is_none() {
        if offset {
            return Err(format!(
                "The function '{name}' takes between 1 and 3 arguments."
            ));
        }
        return Err(format!("The {name} function requires 1 argument(s)."));
    }
    let value = argument(function).unwrap();
    let offset_scale = crate::datetimeoffset_compare::scale(value, parameters);
    let datetime_scale = crate::datetime2_compare::scale(value, parameters);
    let time_scale = crate::time_results::scale(value).or_else(|| {
        if let Expr::Identifier(id) = value
            && let Some(parameter) = parameters.get(&id.value.to_lowercase())
            && let DataType::Time(scale, TimezoneInfo::None) = parameter.ast_type()
        {
            return u8::try_from(scale.unwrap_or(7)).ok().filter(|s| *s <= 7);
        }
        None
    });
    let value_type = crate::case_types::integer_rank(value, parameters)
        .map(|rank| match rank {
            0 => DataType::TinyInt(None),
            1 => DataType::SmallInt(None),
            2 => DataType::Int(None),
            _ => DataType::BigInt(None),
        })
        .or_else(|| crate::case_types::is_bit(value, parameters).then_some(DataType::Bit(None)));
    let FunctionArguments::List(args) = &mut function.args else {
        unreachable!();
    };
    if !matches!(function.parameters, FunctionArguments::None)
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || function.filter.is_some()
        || !function.within_group.is_empty()
    {
        return Err("unsupported value window modifiers".into());
    }
    let Some(over) = &function.over else {
        return Err(format!("The function '{name}' must have an OVER clause."));
    };
    if let WindowType::WindowSpec(spec) = over
        && spec.order_by.is_empty()
        && spec.window_name.is_none()
    {
        return Err(format!(
            "The function '{name}' must have an OVER clause with ORDER BY."
        ));
    }
    if offset {
        if matches!(over, WindowType::WindowSpec(spec) if spec.window_frame.is_some()) {
            return Err(format!(
                "The function '{name}' may not have a window frame."
            ));
        }
        if args
            .args
            .iter()
            .any(|arg| !matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_))))
        {
            return Err("unsupported LAG/LEAD argument".into());
        }
        if let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get_mut(1) {
            *value = crate::engine::unary_function(
                "__msduck_lag_offset",
                Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(value.clone()),
                    data_type: DataType::BigInt(None),
                    format: None,
                },
            );
        }
        if let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get_mut(2) {
            if let Some(scale) = offset_scale {
                *value = crate::datetimeoffset_cast::convert(value.clone(), scale);
            } else if let Some(scale) = datetime_scale {
                *value = crate::datetime2_cast::convert(value.clone(), scale);
            }
        }
        if let Some(scale) = time_scale
            && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get_mut(2)
        {
            *value = crate::assignment::convert(
                value.clone(),
                &DataType::Time(Some(u64::from(scale)), TimezoneInfo::None),
                false,
            );
        }
        if let Some(kind) = value_type
            && let Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(value))) = args.args.get_mut(2)
        {
            *value = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(value.clone()),
                data_type: kind,
                format: None,
            };
        }
    }
    // SQL Server places this after ')'; DuckDB expects it inside the arguments.
    if let Some(treatment) = function.null_treatment.take() {
        args.clauses
            .push(FunctionArgumentClause::IgnoreOrRespectNulls(treatment));
    }
    Ok(())
}

pub const NEGATIVE_OFFSET: &str =
    "Offset parameter for Lag and Lead functions cannot be a negative value.";
pub struct Offset;
impl duckdb::vscalar::VScalar for Offset {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut duckdb::core::DataChunkHandle,
        output: &mut dyn duckdb::vtab::arrow::WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // The exact BIGINT signature fixes both physical vector layouts.
            let value = unsafe { source.as_slice_with_len::<i64>(len)[index] };
            if value < 0 {
                return Err(NEGATIVE_OFFSET.into());
            }
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[index] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<duckdb::vscalar::ScalarFunctionSignature> {
        use duckdb::core::LogicalTypeId;
        vec![duckdb::vscalar::ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Bigint.into()],
            LogicalTypeId::Bigint.into(),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_temporal_defaults_preserve_scale_offset_and_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for scale in 0..=7 {
            for offset in [false, true] {
                let kind = if offset {
                    "DATETIMEOFFSET"
                } else {
                    "DATETIME2"
                };
                let suffix = if offset { "-00:30" } else { "" };
                session
                    .db
                    .execute_batch("CREATE OR REPLACE SEQUENCE temporal_fallback START 1")
                    .unwrap();
                let table = format!("dbo.fallback_{}_{}", offset, scale);
                let sql = format!(
                    "SELECT LAG(CAST(NULL AS {kind}({scale})),6001,printf('2024-01-01T00:00:%02d.1234567{suffix}',nextval('temporal_fallback')%60)) OVER(ORDER BY i) v INTO {table} FROM range(6000) r(i)"
                );
                assert!(
                    session
                        .batch_response(&sql, &Default::default(), false, None)
                        .1,
                    "{kind}({scale})"
                );
                let quantum = 10i64.pow(7 - scale);
                let fraction = ((1234567 + quantum / 2) / quantum) * quantum;
                let tag = if offset {
                    "datetimeoffset"
                } else {
                    "datetime2"
                };
                let wrong:i64=session.db.query_row(&format!("SELECT count(*) FROM {table} WHERE v.__msduck_{tag}_{scale}%10000000 <> {fraction}"),[],|r|r.get(0)).unwrap();
                assert_eq!(wrong, 0);
                if offset {
                    assert_eq!(session.db.query_row(&format!("SELECT count(*) FROM {table} WHERE v.__msduck_offset_minutes<>-30"),[],|r|r.get::<_,i64>(0)).unwrap(),0);
                }
                assert_eq!(
                    session
                        .db
                        .query_row("SELECT currval('temporal_fallback')", [], |r| r
                            .get::<_, i64>(0))
                        .unwrap(),
                    6000
                );
            }
        }
    }

    #[test]
    fn offsets_preserve_nulls_across_chunks_and_evaluate_once() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong: i64 = db.query_row("SELECT COUNT(*) FROM (SELECT CASE WHEN i%17=0 THEN NULL ELSE i END n FROM range(6000) r(i)) s WHERE __msduck_lag_offset(n) IS DISTINCT FROM n", [], |r| r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE offset_calls START 1")
            .unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(__msduck_lag_offset(nextval('offset_calls'))) FROM range(6000)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 6000);
        let calls: i64 = db
            .query_row("SELECT currval('offset_calls')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
        assert!(
            db.query_row("SELECT __msduck_lag_offset(-1::BIGINT)", [], |r| r
                .get::<_, i64>(0))
                .unwrap_err()
                .to_string()
                .contains(super::NEGATIVE_OFFSET)
        );
    }
}
