//! Normalize numeric values before DuckDB performs the target integer range check.
use duckdb::{
    core::Inserter,
    core::{DataChunkHandle, FlatVector, LogicalTypeId},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

pub struct IntegerText;

pub(crate) use msduck_core::diagnostic::TEXT_INT_OVERFLOW;

// Compare decimal digits without an intermediate machine-integer limit. Long
// strings of leading zeroes must not be mistaken for an overflow.
fn text_overflows_int(value: &str) -> bool {
    let negative = value.starts_with('-');
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let digits = digits.trim_start_matches('0');
    let bound = if negative { "2147483648" } else { "2147483647" };
    digits.len() > bound.len() || (digits.len() == bound.len() && digits > bound)
}

fn text_at(
    vector: &FlatVector<'_>,
    index: usize,
    len: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    // Text arguments are VARCHAR; callers check validity first.
    // The copy retains inline bytes, and heap data stays owned by the chunk.
    let mut value = unsafe { vector.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[index] };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
            duckdb::ffi::duckdb_string_t_length(value) as usize,
        )
    };
    Ok(std::str::from_utf8(bytes)?.to_owned())
}

impl VScalar for IntegerText {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let values = input.flat_vector(0);
        let kinds = input.flat_vector(1);
        let targets = input.flat_vector(2);
        let try_modes = input.flat_vector(3);
        let mut out = output.flat_vector();
        for index in 0..len {
            if values.row_is_null(index as u64)
                || kinds.row_is_null(index as u64)
                || targets.row_is_null(index as u64)
                || try_modes.row_is_null(index as u64)
            {
                out.set_null(index);
                continue;
            }
            let value = text_at(&values, index, len)?;
            let kind = text_at(&kinds, index, len)?;
            let normalized = normalize(&value, &kind);
            let target = text_at(&targets, index, len)?;
            // BOOLEAN has one-byte physical storage. The outer TRY_CAST handles
            // failure using the normalized text, without throwing from this UDF.
            let is_try = unsafe { try_modes.as_slice_with_len::<u8>(len)[index] != 0 };
            if !is_try
                && matches!(kind.as_str(), "VARCHAR" | "STRING_LITERAL")
                && matches!(target.as_str(), "INT" | "INTEGER")
                && text_overflows_int(&normalized)
            {
                return Err(TEXT_INT_OVERFLOW.into());
            }
            if !is_try
                && numeric_type(&kind)
                && let Some((minimum, maximum, name)) = integer_range(&target)
                && !normalized
                    .parse::<i128>()
                    .is_ok_and(|value| value >= minimum && value <= maximum)
            {
                return Err(format!(
                    "Arithmetic overflow error converting expression to data type {name}."
                )
                .into());
            }
            out.insert(index, normalized.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                LogicalTypeId::Varchar.into(),
                LogicalTypeId::Varchar.into(),
                LogicalTypeId::Varchar.into(),
                LogicalTypeId::Boolean.into(),
            ],
            LogicalTypeId::Varchar.into(),
        )]
    }
}

fn numeric_type(kind: &str) -> bool {
    kind.starts_with("DECIMAL(")
        || matches!(
            kind,
            "TINYINT"
                | "SMALLINT"
                | "INTEGER"
                | "BIGINT"
                | "HUGEINT"
                | "UTINYINT"
                | "USMALLINT"
                | "UINTEGER"
                | "UBIGINT"
                | "UHUGEINT"
                | "FLOAT"
                | "DOUBLE"
        )
}

fn integer_range(target: &str) -> Option<(i128, i128, &'static str)> {
    match target {
        "TINYINT" | "UTINYINT" => Some((0, 255, "tinyint")),
        "SMALLINT" => Some((i16::MIN as i128, i16::MAX as i128, "smallint")),
        "INT" | "INTEGER" => Some((i32::MIN as i128, i32::MAX as i128, "int")),
        "BIGINT" => Some((i64::MIN as i128, i64::MAX as i128, "bigint")),
        _ => None,
    }
}

fn normalize(value: &str, kind: &str) -> String {
    if kind.starts_with("DECIMAL(") {
        // Backend decimal formatting is fixed point. Never pass through f64:
        // 38-digit inputs and BIGINT boundaries must retain every integral digit.
        let integral = value.split('.').next().unwrap_or(value);
        // DECIMAL(p,p) can format without a leading zero (".9", "-.9").
        // Truncating either sign's sub-unit fraction yields integer zero.
        if matches!(integral, "" | "+" | "-") {
            "0".to_owned()
        } else {
            integral.to_owned()
        }
    } else if matches!(kind, "FLOAT" | "DOUBLE") {
        let number = if kind == "FLOAT" {
            // FLOAT text round-trips to f32, not to the same f64 value. Widen
            // only after parsing at its original precision (e.g. 2^31).
            value.parse::<f32>().ok().map(f64::from)
        } else {
            value.parse::<f64>().ok()
        };
        match number {
            Some(number) if number.is_finite() => format!("{:.0}", number.trunc()),
            _ => value.to_owned(),
        }
    } else if kind == "BOOLEAN" {
        if value == "true" { "1" } else { "0" }.to_owned()
    } else if matches!(kind, "VARCHAR" | "STRING_LITERAL") {
        let trimmed = value.trim_matches(' ');
        if trimmed.is_empty() {
            return "0".to_owned();
        }
        let digits = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            trimmed.to_owned()
        } else {
            // CAST raises the conversion error; TRY_CAST instead returns NULL.
            // DuckDB otherwise accepts and rounds strings such as '1.9'.
            format!("invalid integer value: {value}")
        }
    } else {
        value.to_owned()
    }
}

pub(crate) fn storage_integer_type(kind: &str) -> Option<sqlparser::ast::DataType> {
    match kind {
        "UTINYINT" => Some(sqlparser::ast::DataType::UTinyInt),
        "SMALLINT" => Some(sqlparser::ast::DataType::SmallInt(None)),
        "INTEGER" => Some(sqlparser::ast::DataType::Int(None)),
        "BIGINT" => Some(sqlparser::ast::DataType::BigInt(None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn text_int_bounds_ignore_leading_zeroes() {
        for value in [
            "2147483647",
            "-2147483648",
            "+2147483647",
            "0",
            "-0",
            "bad",
            "2147483648.0",
        ] {
            assert!(!super::text_overflows_int(value), "{value}");
        }
        for value in [
            "2147483648",
            "-2147483649",
            "+2147483648",
            "999999999999999999999999999999999999999999999",
        ] {
            assert!(super::text_overflows_int(value), "{value}");
        }
        assert!(!super::text_overflows_int(&format!(
            "{}2147483647",
            "0".repeat(5000)
        )));
        assert!(super::text_overflows_int(&format!(
            "-{}2147483649",
            "0".repeat(5000)
        )));
        assert_eq!(
            crate::engine::error_number(&format!(
                "Invalid Input Error: {}",
                super::TEXT_INT_OVERFLOW
            )),
            248
        );
        assert_ne!(
            crate::engine::error_number(&format!(
                "Conversion Error: Could not convert string '{}' to INT32",
                super::TEXT_INT_OVERFLOW
            )),
            248
        );
    }
}
