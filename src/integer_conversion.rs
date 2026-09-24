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

/// Exact Unicode input adapter. Numeric and ANSI operands keep their existing
/// path; the dispatch is resolved from the physical type at binding time.
/// ANY avoids binding an impossible STRUCT cast in an unselected variant branch.
/// The callback validates the exact carrier type before touching child vectors.
struct UnicodeInteger;
impl VScalar for UnicodeInteger {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        if !crate::unicode_carrier::is_logical(&source.logical_type()) {
            return Err("invalid Unicode integer input type".into());
        }

        let structure = input.struct_vector(0);
        let payload = structure.child(0, len);
        let targets = input.flat_vector(1);
        let tries = input.flat_vector(2);
        let mut out = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64)
                || targets.row_is_null(row as u64)
                || tries.row_is_null(row as u64)
            {
                out.set_null(row);
                continue;
            }
            if payload.row_is_null(row as u64) {
                return Err("invalid Unicode carrier: NULL payload".into());
            }
            let bytes = crate::unicode_carrier::bytes(&payload, row, len)?;
            if bytes.len() % 2 != 0 {
                return Err("invalid Unicode carrier byte length".into());
            }
            let units: Vec<_> = bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let target = text_at(&targets, row, len)?;
            let is_try = unsafe { tries.as_slice_with_len::<u8>(len)[row] != 0 };
            match unicode_integer(&units, &target) {
                Ok(value) => out.insert(row, value.as_str()),
                Err(error) if is_try && error.number != 8152 => out.set_null(row),
                Err(_) => {
                    // DuckDB scalar errors are UTF8 strings. Carry original units
                    // through that boundary; recovery recomputes the diagnostic.
                    let mut message = format!("msduck_unicode_integer:{target}:");
                    use std::fmt::Write;
                    if units.len() > 4000 {
                        message.push_str("oversize");
                    } else {
                        for unit in units {
                            write!(message, "{unit:04x}")?;
                        }
                    }
                    return Err(message.into());
                }
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                LogicalTypeId::Any.into(),
                LogicalTypeId::Varchar.into(),
                LogicalTypeId::Boolean.into(),
            ],
            LogicalTypeId::Varchar.into(),
        )]
    }
}

fn unicode_integer(
    units: &[u16],
    target: &str,
) -> Result<String, msduck_core::diagnostic::SqlError> {
    use msduck_core::diagnostic::SqlError;
    if units.len() > 4000 {
        return Err(SqlError::new(
            8152,
            10,
            "String or binary data would be truncated.",
        ));
    }
    let (minimum, maximum, name) = integer_range(target)
        .ok_or_else(|| SqlError::new(206, 1, "unsupported Unicode integer target"))?;
    let trimmed = trim_spaces(units);
    let (negative, digits) = match trimmed.first() {
        Some(45) => (true, &trimmed[1..]),
        Some(43) => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digits = trim_spaces(digits);
    let syntax = digits.iter().any(|u| !(48..=57).contains(u));
    let value = (!syntax)
        .then(|| {
            digits.iter().try_fold(0i128, |v, &u| {
                v.checked_mul(10)?.checked_add(i128::from(u - 48))
            })
        })
        .flatten()
        .and_then(|v| if negative { v.checked_neg() } else { Some(v) });
    if let Some(value) = value.filter(|v| *v >= minimum && *v <= maximum) {
        return Ok(value.to_string());
    }
    if name == "bigint" {
        return Err(if syntax {
            SqlError::new(8114, 5, "Error converting data type nvarchar to bigint.")
        } else {
            SqlError::new(
                8115,
                2,
                "Arithmetic overflow error converting expression to data type bigint.",
            )
        });
    }
    let (number, state, prefix, suffix) = if syntax {
        (
            245,
            1,
            "Conversion failed when converting the nvarchar value '",
            format!("' to data type {name}."),
        )
    } else if name == "int" {
        (
            248,
            1,
            "The conversion of the nvarchar value '",
            "' overflowed an int column.".into(),
        )
    } else {
        let width = if name == "tinyint" { 1 } else { 2 };
        (
            244,
            width,
            "The conversion of the nvarchar value '",
            format!("' overflowed an INT{width} column. Use a larger integer column."),
        )
    };
    let message = prefix
        .encode_utf16()
        .chain(units.iter().copied().map(|u| if u == 0 { 46 } else { u }))
        .chain(suffix.encode_utf16())
        .collect();
    Err(SqlError::from_utf16(number, state, 16, message))
}

fn trim_spaces(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|u| *u != 32).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| *u != 32)
        .map_or(start, |i| i + 1);
    &units[start..end]
}

pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let message = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    let (target, encoded) = message
        .strip_prefix("msduck_unicode_integer:")?
        .split_once(':')?;
    integer_range(target)?;
    if encoded == "oversize" {
        return unicode_integer(&vec![32; 4001], target).err();
    }
    if encoded.len() > 16000
        || !encoded.len().is_multiple_of(4)
        || !encoded.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let units = encoded
        .as_bytes()
        .chunks_exact(4)
        .map(|b| u16::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    unicode_integer(&units, target).err()
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<IntegerText>("__msduck_integer_text")?;
    db.register_scalar_function::<UnicodeInteger>("__msduck_integer_unicode")?;
    db.execute_batch("CREATE OR REPLACE MACRO main.__msduck_integer_input(value, target := '', try_mode := false) AS CASE WHEN typeof(value) = 'STRUCT(__msduck_utf16le BLOB)' THEN __msduck_integer_unicode(value,target,try_mode) ELSE __msduck_integer_text(CAST(value AS VARCHAR),typeof(value),target,try_mode) END")
}

#[cfg(test)]
mod tests {
    #[test]
    fn unicode_diagnostics_retain_units_and_reject_embedded_or_valid_values() {
        let error =
            super::diagnostic("Invalid Input Error: msduck_unicode_integer:INT:d83e").unwrap();
        assert_eq!((error.number, error.state, error.severity), (245, 1, 16));
        let expected: Vec<_> = "Conversion failed when converting the nvarchar value '"
            .encode_utf16()
            .chain([0xd83e])
            .chain("' to data type int.".encode_utf16())
            .collect();
        assert_eq!(error.message_utf16, Some(expected));
        for invalid in [
            "wrapper: msduck_unicode_integer:INT:d83e",
            "msduck_unicode_integer:INT:0037",
            "msduck_unicode_integer:INT:d83",
            "msduck_unicode_integer:INT:zzzz",
            "msduck_unicode_integer:TEXT:d83e",
            "msduck_unicode_integer:INT:d83e trailing",
        ] {
            assert!(super::diagnostic(invalid).is_none(), "{invalid}");
        }
    }

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
