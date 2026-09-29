//! NTILE bucket validation without lossy implicit numeric conversion.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use sqlparser::ast::*;

pub const POSITIVE: &str =
    "The function 'ntile' takes only a positive int or bigint expression as its input.";
pub const TYPE: &str = "Invalid NTILE bucket argument type: ";

/// Only captured invalid constants are rejected here. Parameters, subqueries
/// and other expressions remain runtime inputs; no value is evaluated.
pub fn validate_constants(statement: &Statement) -> Result<(), msduck_core::diagnostic::SqlError> {
    fn null(expr: &Expr) -> bool {
        match expr {
            Expr::Value(value) => matches!(value.value, Value::Null),
            Expr::Nested(expr) => null(expr),
            Expr::Cast {
                expr,
                data_type: DataType::Int(_) | DataType::BigInt(_),
                ..
            } => null(expr),
            _ => false,
        }
    }
    fn integer(expr: &Expr) -> Option<i128> {
        match expr {
            Expr::Value(value) => match &value.value {
                Value::Number(number, _) => number.parse().ok(),
                _ => None,
            },
            Expr::Nested(expr) => integer(expr),
            Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr,
            } => integer(expr)?.checked_neg(),
            Expr::UnaryOp {
                op: UnaryOperator::Plus,
                expr,
            } => integer(expr),
            _ => None,
        }
    }
    let result = visit_expressions(statement, |expr| {
        if let Expr::Function(function) = expr
            && function.name.to_string().eq_ignore_ascii_case("NTILE")
            && msduck_sql::ranking::validate(function).is_ok()
            && let Some(WindowType::WindowSpec(spec)) = &function.over
            && !spec.order_by.is_empty()
            && spec.window_frame.is_none()
            && let FunctionArguments::List(args) = &function.args
            && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice()
            && (null(value) || integer(value).is_some_and(|count| count <= 0))
        {
            return std::ops::ControlFlow::Break(msduck_core::diagnostic::SqlError::syntax(
                4116, 1, POSITIVE,
            ));
        }
        std::ops::ControlFlow::Continue(())
    });
    match result {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(error) => Err(error),
    }
}

/// Recognize only the native validator or canonical binding diagnostic, never
/// an explicit application SqlError or unrelated backend message.
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    use msduck_core::diagnostic::SqlError;
    let message = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    if message == POSITIVE {
        return Some(SqlError::syntax(4116, 1, POSITIVE));
    }
    let prefix = "The reference to column '";
    let suffix = "' is not allowed in an argument to the NTILE function. Only references to columns at an outer scope or standalone expressions and subqueries are allowed here.";
    let column = message.strip_prefix(prefix)?.strip_suffix(suffix)?;
    Some(SqlError::syntax(
        4195,
        1,
        format!(
            "The reference to column \"{column}\" is not allowed in an argument to the NTILE function. Only references to columns at an outer scope or standalone expressions and subqueries are allowed here."
        ),
    ))
}

pub fn lower(expr: &mut Expr) {
    if let Expr::Function(function) = expr
        && function.name.to_string().eq_ignore_ascii_case("NTILE")
        && let FunctionArguments::List(args) = &mut function.args
        && let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_mut_slice()
    {
        *value = crate::engine::unary_function("__msduck_ntile_count", value.clone());
    }
}

pub struct Buckets;
impl VScalar for Buckets {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let kind = source.logical_type().id();
        if !matches!(
            kind,
            LogicalTypeId::UTinyint
                | LogicalTypeId::Smallint
                | LogicalTypeId::Integer
                | LogicalTypeId::Bigint
                | LogicalTypeId::SqlNull
        ) {
            return Err(POSITIVE.into());
        }
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                return Err(POSITIVE.into());
            }
            // ANY retains the actual input type; inspect it before reading the
            // matching physical vector. The result signature is always BIGINT.
            let value = unsafe {
                match kind {
                    LogicalTypeId::UTinyint => {
                        i64::from(source.as_slice_with_len::<u8>(len)[index])
                    }
                    LogicalTypeId::Smallint => {
                        i64::from(source.as_slice_with_len::<i16>(len)[index])
                    }
                    LogicalTypeId::Integer => {
                        i64::from(source.as_slice_with_len::<i32>(len)[index])
                    }
                    LogicalTypeId::Bigint => source.as_slice_with_len::<i64>(len)[index],
                    _ => return Err("invalid non-NULL SQLNULL vector".into()),
                }
            };
            if value <= 0 {
                return Err(POSITIVE.into());
            }
            unsafe {
                result.as_mut_slice_with_len::<i64>(len)[index] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Any.into()],
            LogicalTypeId::Bigint.into(),
        )]
    }

    fn special_null_handling() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bucket_validator_reads_each_integer_layout_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for kind in ["UTINYINT", "SMALLINT", "INTEGER", "BIGINT"] {
            let sql = format!(
                "SELECT COUNT(*) FROM (SELECT CAST(i%200+1 AS {kind}) n FROM range(6000) r(i)) s WHERE __msduck_ntile_count(n) IS DISTINCT FROM CAST(n AS BIGINT)"
            );
            assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            let sql = format!(
                "SELECT SUM(__msduck_ntile_count(CAST(CASE WHEN i=5000 THEN NULL ELSE 2 END AS {kind}))) FROM range(6000) r(i)"
            );
            let error = db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap_err();
            assert!(error.to_string().contains(super::POSITIVE));
        }
        for value in ["0", "-1"] {
            assert!(
                db.query_row(&format!("SELECT __msduck_ntile_count({value})"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_err()
                .to_string()
                .contains(super::POSITIVE)
            );
        }
        for value in ["NULL", "CAST(NULL AS INTEGER)", "CAST(NULL AS BIGINT)"] {
            assert!(
                db.query_row(&format!("SELECT __msduck_ntile_count({value})"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_err()
                .to_string()
                .contains(super::POSITIVE)
            );
        }
        for value in [
            "1.5",
            "'2'",
            "true",
            "CAST(2 AS DECIMAL(10,2))",
            "DATE '2024-01-02'",
        ] {
            assert!(
                db.query_row(&format!("SELECT __msduck_ntile_count({value})"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_err()
                .to_string()
                .contains(super::POSITIVE)
            );
        }
    }
}
