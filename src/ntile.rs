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
            return Err(format!("{TYPE}{kind:?}").into());
        }
        let mut result = output.flat_vector();
        for index in 0..len {
            if source.row_is_null(index as u64) {
                result.set_null(index);
                continue;
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
}

#[cfg(test)]
mod tests {
    #[test]
    fn bucket_validator_reads_each_integer_layout_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for kind in ["UTINYINT", "SMALLINT", "INTEGER", "BIGINT"] {
            let sql = format!(
                "SELECT COUNT(*) FROM (SELECT CAST(CASE WHEN i%17=0 THEN NULL ELSE i%200+1 END AS {kind}) n FROM range(6000) r(i)) s WHERE __msduck_ntile_count(n) IS DISTINCT FROM CAST(n AS BIGINT)"
            );
            assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 0);
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
        for value in ["1.5", "'2'", "true"] {
            assert!(
                db.query_row(&format!("SELECT __msduck_ntile_count({value})"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_err()
                .to_string()
                .contains(super::TYPE)
            );
        }
    }
}
