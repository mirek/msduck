//! Width-preserving binary bitwise operations, resolved from bound input types.
use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

const TYPES: [LogicalTypeId; 5] = [
    LogicalTypeId::Boolean,
    LogicalTypeId::UTinyint,
    LogicalTypeId::Smallint,
    LogicalTypeId::Integer,
    LogicalTypeId::Bigint,
];

pub struct Binary<const OP: u8>;
impl<const OP: u8> VScalar for Binary<OP> {
    type State = ();
    fn invoke(
        _: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let left = input.flat_vector(0);
        let right = input.flat_vector(1);
        let left_kind = left.logical_type().id();
        let right_kind = right.logical_type().id();
        let rank = |kind| {
            TYPES
                .iter()
                .position(|v| *v == kind)
                .ok_or("unsupported bitwise operand")
        };
        let result_kind = TYPES[rank(left_kind)?.max(rank(right_kind)?).max(1)];
        let mut result = output.flat_vector();
        for index in 0..len {
            if left.row_is_null(index as u64) || right.row_is_null(index as u64) {
                result.set_null(index);
                continue;
            }
            // Exact overloads fix the vector storage types. Reads and writes
            // are restricted to initialized non-null slots in this chunk.
            macro_rules! read {
                ($vector:ident, $kind:expr) => {
                    unsafe {
                        match $kind {
                            LogicalTypeId::Boolean => {
                                i64::from($vector.as_slice_with_len::<bool>(len)[index])
                            }
                            LogicalTypeId::UTinyint => {
                                i64::from($vector.as_slice_with_len::<u8>(len)[index])
                            }
                            LogicalTypeId::Smallint => {
                                i64::from($vector.as_slice_with_len::<i16>(len)[index])
                            }
                            LogicalTypeId::Integer => {
                                i64::from($vector.as_slice_with_len::<i32>(len)[index])
                            }
                            LogicalTypeId::Bigint => $vector.as_slice_with_len::<i64>(len)[index],
                            _ => unreachable!(),
                        }
                    }
                };
            }
            let a = read!(left, left_kind);
            let b = read!(right, right_kind);
            let value = match OP {
                0 => a & b,
                1 => a | b,
                _ => a ^ b,
            };
            unsafe {
                match result_kind {
                    LogicalTypeId::UTinyint => {
                        result.as_mut_slice_with_len::<u8>(len)[index] = value as u8
                    }
                    LogicalTypeId::Smallint => {
                        result.as_mut_slice_with_len::<i16>(len)[index] = value as i16
                    }
                    LogicalTypeId::Integer => {
                        result.as_mut_slice_with_len::<i32>(len)[index] = value as i32
                    }
                    LogicalTypeId::Bigint => {
                        result.as_mut_slice_with_len::<i64>(len)[index] = value
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let mut signatures = Vec::new();
        for (a, left) in TYPES.iter().enumerate() {
            for (b, right) in TYPES.iter().enumerate() {
                signatures.push(ScalarFunctionSignature::exact(
                    vec![
                        LogicalTypeHandle::from(*left),
                        LogicalTypeHandle::from(*right),
                    ],
                    LogicalTypeHandle::from(TYPES[a.max(b).max(1)]),
                ));
            }
        }
        signatures
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn overload_matrix_preserves_values_and_nulls_across_chunks() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for left in ["BOOLEAN", "UTINYINT", "SMALLINT", "INTEGER", "BIGINT"] {
            for right in ["BOOLEAN", "UTINYINT", "SMALLINT", "INTEGER", "BIGINT"] {
                let sql = format!(
                    "SELECT COUNT(*) FROM (SELECT CAST(CASE WHEN n%17=0 THEN NULL ELSE n%127 END AS {left}) a, CAST(CASE WHEN n%13=0 THEN NULL ELSE n%97 END AS {right}) b FROM range(5000) r(n)) s WHERE CAST(__msduck_bitand(a,b) AS BIGINT) IS DISTINCT FROM (CAST(a AS BIGINT)&CAST(b AS BIGINT)) OR CAST(__msduck_bitor(a,b) AS BIGINT) IS DISTINCT FROM (CAST(a AS BIGINT)|CAST(b AS BIGINT)) OR CAST(__msduck_bitxor(a,b) AS BIGINT) IS DISTINCT FROM xor(CAST(a AS BIGINT),CAST(b AS BIGINT))"
                );
                let wrong: i64 = db.query_row(&sql, [], |row| row.get(0)).unwrap();
                assert_eq!(wrong, 0, "{left}, {right}");
            }
        }
    }
}
