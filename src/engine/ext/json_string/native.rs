//! Native functions behind the json_string lowerings: JSON_MODIFY,
//! STRING_SPLIT, STRING_AGG input text and HASHBYTES.
//!
//! Text arguments arrive either as DuckDB VARCHAR (UTF-8) or as the exact
//! UTF-16 carrier used for NVARCHAR storage. SQL errors leave as json_string
//! markers (`msduck_sql::dialect::ext::json_string::error`), which the
//! runtime diagnostic chain turns back into SQL Server numbers and states.
use super::digest;
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle as Type, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_sql::dialect::ext::json_string::{error as marker, modify};

type Error = Box<dyn std::error::Error>;

/// A text argument: VARCHAR (UTF-8) or an NVARCHAR carrier (UTF-16).
enum Text {
    Ansi(String),
    Unicode(Vec<u16>),
}

impl Text {
    fn units(&self) -> Vec<u16> {
        match self {
            Self::Ansi(text) => text.encode_utf16().collect(),
            Self::Unicode(units) => units.clone(),
        }
    }
}

fn varchar(vector: &FlatVector<'_>, row: usize, len: usize) -> Result<String, Error> {
    Ok(String::from_utf8(crate::unicode_carrier::bytes(
        vector, row, len,
    )?)?)
}

fn utf8(units: &[u16]) -> Result<String, Error> {
    String::from_utf16(units)
        .map_err(|_| "text with isolated surrogate code units is not yet supported".into())
}

/// Read a VARCHAR or carrier argument; None is SQL NULL.
fn text(
    input: &DataChunkHandle,
    column: usize,
    row: usize,
    len: usize,
) -> Result<Option<Text>, Error> {
    let flat = input.flat_vector(column);
    if flat.row_is_null(row as u64) {
        return Ok(None);
    }
    let logical = flat.logical_type();
    if crate::unicode_carrier::is_logical(&logical) {
        let data = input.struct_vector(column).child(0, len);
        return Ok(
            crate::unicode_carrier::vector_units(&flat, &data, row, len)?.map(Text::Unicode),
        );
    }
    match logical.id() {
        Id::Varchar => Ok(Some(Text::Ansi(varchar(&flat, row, len)?))),
        _ => Err("unsupported json_string text argument".into()),
    }
}

/// SQL Server's name for a native argument type, for error 8116.
fn type_name(logical: &Type) -> &'static str {
    match logical.id() {
        Id::Boolean => "bit",
        Id::Tinyint | Id::UTinyint => "tinyint",
        Id::Smallint | Id::USmallint => "smallint",
        Id::Integer | Id::UInteger => "int",
        Id::Bigint | Id::UBigint | Id::Hugeint => "bigint",
        Id::Decimal => "numeric",
        Id::Float => "real",
        Id::Double => "float",
        Id::Date => "date",
        Id::Time | Id::TimeNs => "time",
        Id::Timestamp | Id::TimestampS | Id::TimestampMs | Id::TimestampNs => "datetime2",
        Id::Uuid => "uniqueidentifier",
        Id::Blob => "varbinary",
        _ => "sql_variant",
    }
}

fn decimal_text(coefficient: i128, scale: u8) -> String {
    let digits = coefficient.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let sign = if coefficient < 0 { "-" } else { "" };
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let digits = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = digits.split_at(digits.len() - scale);
    format!("{sign}{whole}.{fraction}")
}

/// SQL Server's JSON spelling of FLOAT and REAL: 16 significant digits in
/// scientific notation with a signed three-digit exponent.
pub(super) fn float_text(value: f64) -> String {
    let text = format!("{value:.15e}");
    let (mantissa, exponent) = text.split_once('e').expect("scientific notation");
    let exponent: i32 = exponent.parse().expect("integer exponent");
    format!(
        "{mantissa}e{}{:03}",
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    )
}

fn quoted(units: &[u16]) -> Vec<u16> {
    let escaped = msduck_core::json_escape::escape_utf16(units, &[106, 115, 111, 110])
        .expect("JSON format is supported");
    let mut out = Vec::with_capacity(escaped.len() + 2);
    out.push(u16::from(b'"'));
    out.extend_from_slice(&escaped);
    out.push(u16::from(b'"'));
    out
}

/// The JSON text JSON_MODIFY writes for its third argument; None is NULL.
fn json_value(
    input: &DataChunkHandle,
    row: usize,
    len: usize,
    json: bool,
) -> Result<Option<Vec<u16>>, Error> {
    let flat = input.flat_vector(2);
    if flat.row_is_null(row as u64) {
        return Ok(None);
    }
    let logical = flat.logical_type();
    macro_rules! read {
        ($ty:ty) => {
            unsafe { flat.as_slice_with_len::<$ty>(len)[row] }
        };
    }
    let text = match logical.id() {
        Id::Varchar | Id::Struct => {
            let value = text(input, 2, row, len)?
                .ok_or("invalid NULL JSON_MODIFY value")?
                .units();
            if json {
                if !msduck_core::json::valid_utf16(&value, 1) {
                    return Err(marker(13609, 1, 16, "JSON text is not properly formatted.").into());
                }
                return Ok(Some(value));
            }
            return Ok(Some(quoted(&value)));
        }
        Id::Boolean => (if read!(u8) != 0 { "true" } else { "false" }).to_owned(),
        Id::Tinyint => read!(i8).to_string(),
        Id::UTinyint => read!(u8).to_string(),
        Id::Smallint => read!(i16).to_string(),
        Id::USmallint => read!(u16).to_string(),
        Id::Integer => read!(i32).to_string(),
        Id::UInteger => read!(u32).to_string(),
        Id::Bigint => read!(i64).to_string(),
        Id::UBigint => read!(u64).to_string(),
        Id::Hugeint => {
            let value = read!(duckdb::ffi::duckdb_hugeint);
            ((i128::from(value.upper) << 64) | i128::from(value.lower)).to_string()
        }
        Id::Decimal => {
            let coefficient = match logical.decimal_width() {
                1..=4 => i128::from(read!(i16)),
                5..=9 => i128::from(read!(i32)),
                10..=18 => i128::from(read!(i64)),
                _ => {
                    let value = read!(duckdb::ffi::duckdb_hugeint);
                    (i128::from(value.upper) << 64) | i128::from(value.lower)
                }
            };
            decimal_text(coefficient, logical.decimal_scale())
        }
        Id::Float => float_text(f64::from(read!(f32))),
        Id::Double => float_text(read!(f64)),
        _ => {
            return Err(marker(
                8116,
                1,
                16,
                &format!(
                    "Argument data type {} is invalid for argument 3 of json_modify function.",
                    type_name(&logical)
                ),
            )
            .into());
        }
    };
    Ok(Some(text.encode_utf16().collect()))
}

/// `__msduck_json_modify(document, path, value, value_is_json)`: VARCHAR.
struct JsonModify;
impl VScalar for JsonModify {
    type State = ();
    // NULL arguments still reach the callback: a NULL value deletes in
    // JSON_MODIFY, and NULL separators and paths are errors.
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let flags = input.flat_vector(3);
        let mut out = output.flat_vector();
        for row in 0..len {
            let Some(document) = text(input, 0, row, len)? else {
                out.set_null(row);
                continue;
            };
            let Some(path) = text(input, 1, row, len)? else {
                return Err(marker(
                    8116,
                    8,
                    16,
                    "Argument data type NULL is invalid for argument 2 of JSON_MODIFY function.",
                )
                .into());
            };
            let path = modify::path(&path.units())?;
            let json = !flags.row_is_null(row as u64)
                && unsafe { flags.as_slice_with_len::<u8>(len)[row] } != 0;
            let value = json_value(input, row, len, json)?;
            let result = modify::modify(&document.units(), &path, value.as_deref())?;
            out.insert(row, utf8(&result)?.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                Id::Any.into(),
                Id::Any.into(),
                Id::Any.into(),
                Id::Boolean.into(),
            ],
            Id::Varchar.into(),
        )]
    }
}

const SEPARATOR: &str = "Procedure expects parameter 'separator' of type 'nchar(1)/nvarchar(1)'.";

/// `__msduck_string_split(source, separator)`: LIST<STRUCT(value VARCHAR,
/// ordinal BIGINT)>, NULL for a NULL source. Empty tokens are kept.
struct Split;
impl VScalar for Split {
    type State = ();
    // NULL arguments still reach the callback: a NULL value deletes in
    // JSON_MODIFY, and NULL separators and paths are errors.
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let mut tokens: Vec<String> = Vec::new();
        let mut entries = Vec::with_capacity(len);
        let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
        for row in 0..len {
            let separator = text(input, 1, row, len)?.map(|t| t.units());
            let Some([separator]) = separator.as_deref() else {
                return Err(marker(214, 11, 16, SEPARATOR).into());
            };
            let Some(source) = text(input, 0, row, len)? else {
                entries.push(None);
                continue;
            };
            let units = source.units();
            remaining = remaining
                .checked_sub(units.len() * 2)
                .ok_or("STRING_SPLIT input exceeds the configured limit")?;
            let start = tokens.len();
            for token in units.split(|unit| unit == separator) {
                tokens.push(utf8(token)?);
            }
            entries.push(Some((start, tokens.len() - start)));
        }
        let mut list = output.list_vector();
        for (row, entry) in entries.iter().enumerate() {
            match entry {
                Some((offset, count)) => list.set_entry(row, *offset, *count),
                None => {
                    list.set_entry(row, tokens.len(), 0);
                    list.set_null(row);
                }
            }
        }
        let child = list.struct_child(tokens.len());
        list.try_set_len(tokens.len())?;
        let values = child.child(0, tokens.len());
        let mut ordinals = child.child(1, tokens.len());
        for (index, token) in tokens.iter().enumerate() {
            values.insert(index, token.as_str());
        }
        for (offset, count) in entries.iter().flatten() {
            let slots = unsafe { ordinals.as_mut_slice_with_len::<i64>(tokens.len()) };
            for (ordinal, slot) in slots[*offset..offset + count].iter_mut().enumerate() {
                *slot = ordinal as i64 + 1;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into(), Id::Any.into()],
            Type::list(&Type::struct_type(&[
                ("value", Id::Varchar.into()),
                ("ordinal", Id::Bigint.into()),
            ])),
        )]
    }
}

/// `__msduck_json_string_text(value)`: VARCHAR text of a VARCHAR, an
/// NVARCHAR carrier or a DATETIME2 value (SQL Server's `yyyy-mm-dd
/// hh:mi:ss[.f]` conversion text).
struct PlainText;
impl VScalar for PlainText {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let flat = input.flat_vector(0);
        let logical = flat.logical_type();
        let datetime2 = (logical.id() == Id::Struct
            && logical.num_children() == 1
            && logical.child(0).id() == Id::Bigint)
            .then(|| {
                logical
                    .child_name(0)
                    .strip_prefix("__msduck_datetime2_")
                    .and_then(|scale| scale.parse::<u8>().ok())
                    .filter(|scale| *scale <= 7)
            })
            .flatten();
        let mut out = output.flat_vector();
        for row in 0..len {
            if let Some(scale) = datetime2 {
                if flat.row_is_null(row as u64) {
                    out.set_null(row);
                    continue;
                }
                let ticks = input.struct_vector(0).child(0, len);
                let ticks = unsafe { ticks.as_slice_with_len::<i64>(len)[row] };
                let text = crate::datetime2::DateTime2::from_ticks(ticks)
                    .and_then(|value| value.format_iso(scale))
                    .map_err(|e| e.to_string())?
                    .replacen('T', " ", 1);
                out.insert(row, text.as_str());
                continue;
            }
            match text(input, 0, row, len)? {
                None => out.set_null(row),
                Some(Text::Ansi(text)) => out.insert(row, text.as_str()),
                Some(Text::Unicode(units)) => out.insert(row, utf8(&units)?.as_str()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            Id::Varchar.into(),
        )]
    }
}

/// `__msduck_string_agg_limit(result, unicode)`: the 8000-byte limit of a
/// bounded STRING_AGG result (error 9829; state 0 VARCHAR, 1 NVARCHAR).
struct AggLimit;
impl VScalar for AggLimit {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let flags = input.flat_vector(1);
        let mut out = output.flat_vector();
        for row in 0..len {
            let Some(text) = text(input, 0, row, len)? else {
                out.set_null(row);
                continue;
            };
            let unicode = unsafe { flags.as_slice_with_len::<u8>(len)[row] } != 0;
            let value = match text {
                Text::Ansi(text) => text,
                Text::Unicode(units) => utf8(&units)?,
            };
            let bytes = if unicode {
                value.encode_utf16().count() * 2
            } else {
                value.chars().count()
            };
            if bytes > 8000 {
                return Err(marker(
                    9829,
                    u8::from(unicode),
                    16,
                    "STRING_AGG aggregation result exceeded the limit of 8000 bytes. Use LOB types to avoid result truncation.",
                )
                .into());
            }
            out.insert(row, value.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into(), Id::Boolean.into()],
            Id::Varchar.into(),
        )]
    }
}

/// Code-page bytes of VARCHAR text (Windows-1252, the default collation);
/// characters outside the code page become '?', as SQL Server's conversion
/// does.
fn code_page(text: &str) -> Vec<u8> {
    msduck_core::encoding::encode_cp1252(text).unwrap_or_else(|_| {
        text.chars()
            .map(|c| {
                msduck_core::encoding::encode_cp1252(c.encode_utf8(&mut [0; 4]))
                    .map(|bytes| bytes[0])
                    .unwrap_or(b'?')
            })
            .collect()
    })
}

/// `__msduck_hashbytes(algorithm, input, encoding)`: BLOB. `encoding` is
/// 'n' when the input is statically NVARCHAR, 'v' for VARCHAR and '' when
/// unknown (decided by the native representation).
struct HashBytes;
impl VScalar for HashBytes {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let source = input.flat_vector(1);
        let logical = source.logical_type();
        let encodings = input.flat_vector(2);
        let mut out = output.flat_vector();
        for row in 0..len {
            let Some(algorithm) = text(input, 0, row, len)? else {
                out.set_null(row);
                continue;
            };
            let algorithm = utf8(&algorithm.units())?;
            let algorithm = digest::resolve_algorithm(&algorithm).map_err(|e| e.0)?;
            if source.row_is_null(row as u64) {
                out.set_null(row);
                continue;
            }
            let bytes = if crate::unicode_carrier::is_logical(&logical) {
                let Some(Text::Unicode(units)) = text(input, 1, row, len)? else {
                    unreachable!("carrier input")
                };
                units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
            } else {
                match logical.id() {
                    Id::Blob => crate::unicode_carrier::bytes(&source, row, len)?,
                    Id::Varchar => {
                        let text = varchar(&source, row, len)?;
                        if varchar(&encodings, row, len)? == "n" {
                            text.encode_utf16().flat_map(u16::to_le_bytes).collect()
                        } else {
                            code_page(&text)
                        }
                    }
                    _ => {
                        return Err(marker(
                            8116,
                            1,
                            16,
                            &format!(
                                "Argument data type {} is invalid for argument 2 of hashbytes function.",
                                type_name(&logical)
                            ),
                        )
                        .into());
                    }
                }
            };
            match algorithm {
                Some(algorithm) => out.insert(row, algorithm.digest(&bytes).as_slice()),
                None => out.set_null(row),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into(), Id::Any.into(), Id::Varchar.into()],
            Id::Blob.into(),
        )]
    }
}

pub(super) fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<JsonModify>("__msduck_json_modify")?;
    db.register_scalar_function::<Split>("__msduck_string_split")?;
    db.register_scalar_function::<PlainText>("__msduck_json_string_text")?;
    db.register_scalar_function::<AggLimit>("__msduck_string_agg_limit")?;
    db.register_scalar_function::<HashBytes>("__msduck_hashbytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_and_decimal_spellings() {
        assert_eq!(float_text(0.1), "1.000000000000000e-001");
        assert_eq!(float_text(1e300), "1.000000000000000e+300");
        assert_eq!(float_text(3.0), "3.000000000000000e+000");
        assert_eq!(float_text(-0.0), "-0.000000000000000e+000");
        assert_eq!(float_text(123.456), "1.234560000000000e+002");
        assert_eq!(float_text(f64::from(1.5f32)), "1.500000000000000e+000");
        assert_eq!(decimal_text(-12340, 3), "-12.340");
        assert_eq!(decimal_text(5, 2), "0.05");
        assert_eq!(decimal_text(1234567890125, 1), "123456789012.5");
        assert_eq!(decimal_text(7, 0), "7");
    }
}
