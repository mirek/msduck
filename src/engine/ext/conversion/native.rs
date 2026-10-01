//! DuckDB scalar functions for styled conversions, FORMAT and explicit
//! collations. Each reads its first argument by the backend type msduck uses
//! for the SQL Server type (see `Input`).
use super::{binary, dotnet, temporal};
use crate::{datetime2::DateTime2, datetimeoffset::DateTimeOffset};
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

type Error = Box<dyn std::error::Error>;

const NANOS_PER_DAY: i128 = 86_400_000_000_000;

/// The backend representation of an argument.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Null,
    Varchar,
    /// The UTF-16 carrier STRUCT of Unicode values.
    Carrier,
    Blob,
    Boolean,
    /// Integers: SQL type name and two's complement width.
    Integer(&'static str, u8),
    Decimal(u8, u8),
    Double,
    Float,
    Date,
    /// TIMESTAMP family (datetime, smalldatetime): nanoseconds per unit.
    Timestamp(i128),
    /// TIME and TIME_NS: nanoseconds per unit.
    Time(i128),
    DateTime2(u8),
    DateTimeOffset(u8),
    Other(&'static str),
}

impl Kind {
    fn sql_name(self) -> &'static str {
        match self {
            Self::Null => "NULL",
            Self::Varchar => "varchar",
            Self::Carrier => "nvarchar",
            Self::Blob => "varbinary",
            Self::Boolean => "bit",
            Self::Integer(name, _) => name,
            Self::Decimal(..) => "numeric",
            Self::Double => "float",
            Self::Float => "real",
            Self::Date => "date",
            Self::Timestamp(_) => "datetime",
            Self::Time(_) => "time",
            Self::DateTime2(_) => "datetime2",
            Self::DateTimeOffset(_) => "datetimeoffset",
            Self::Other(name) => name,
        }
    }
}

/// A value read from an argument vector.
#[derive(Clone, Debug, PartialEq)]
enum Datum {
    Text(String, Option<Vec<u16>>),
    Binary(Vec<u8>),
    Boolean(bool),
    Integer(i128, u8),
    Decimal(i128, u8),
    Double(f64),
    Float(f32),
    Temporal(temporal::Value, temporal::Source),
}

struct Input<'a> {
    kind: Kind,
    vector: FlatVector<'a>,
    first: Option<FlatVector<'a>>,
    second: Option<FlatVector<'a>>,
    len: usize,
}

impl<'a> Input<'a> {
    fn new(chunk: &'a DataChunkHandle, index: usize) -> Self {
        let len = chunk.len();
        let vector = chunk.flat_vector(index);
        let logical = vector.logical_type();
        let kind = match logical.id() {
            Id::SqlNull => Kind::Null,
            Id::Varchar => Kind::Varchar,
            Id::Blob => Kind::Blob,
            Id::Boolean => Kind::Boolean,
            Id::Tinyint => Kind::Integer("tinyint", 8),
            Id::UTinyint => Kind::Integer("tinyint", 8),
            Id::Smallint => Kind::Integer("smallint", 16),
            Id::Integer => Kind::Integer("int", 32),
            Id::Bigint => Kind::Integer("bigint", 64),
            Id::Decimal => Kind::Decimal(logical.decimal_width(), logical.decimal_scale()),
            Id::Double => Kind::Double,
            Id::Float => Kind::Float,
            Id::Date => Kind::Date,
            Id::Timestamp => Kind::Timestamp(1_000),
            Id::TimestampMs => Kind::Timestamp(1_000_000),
            Id::TimestampS => Kind::Timestamp(1_000_000_000),
            Id::TimestampNs => Kind::Timestamp(1),
            Id::Time => Kind::Time(1_000),
            Id::TimeNs => Kind::Time(1),
            Id::Uuid => Kind::Other("uniqueidentifier"),
            Id::Struct if crate::unicode_carrier::is_logical(&logical) => Kind::Carrier,
            Id::Struct => {
                let names: Vec<String> = (0..logical.num_children())
                    .map(|i| logical.child_name(i))
                    .collect();
                match names.as_slice() {
                    [name] if logical.child(0).id() == Id::Bigint => {
                        match msduck_sql::datetime2_cast::field_scale(name) {
                            Some(scale) => Kind::DateTime2(scale),
                            None => Kind::Other("sql_variant"),
                        }
                    }
                    [name, offset]
                        if offset == msduck_sql::datetimeoffset_cast::OFFSET
                            && logical.child(0).id() == Id::Bigint =>
                    {
                        match msduck_sql::datetimeoffset_cast::field_scale(name) {
                            Some(scale) => Kind::DateTimeOffset(scale),
                            None => Kind::Other("sql_variant"),
                        }
                    }
                    _ => Kind::Other("sql_variant"),
                }
            }
            _ => Kind::Other("sql_variant"),
        };
        let (first, second) = match kind {
            Kind::Carrier | Kind::DateTime2(_) => {
                (Some(chunk.struct_vector(index).child(0, len)), None)
            }
            Kind::DateTimeOffset(_) => {
                let structure = chunk.struct_vector(index);
                (Some(structure.child(0, len)), Some(structure.child(1, len)))
            }
            _ => (None, None),
        };
        Self {
            kind,
            vector,
            first,
            second,
            len,
        }
    }

    fn is_null(&self, row: usize) -> bool {
        self.kind == Kind::Null
            || self.vector.row_is_null(row as u64)
            || self
                .first
                .as_ref()
                .is_some_and(|v| v.row_is_null(row as u64))
            || self
                .second
                .as_ref()
                .is_some_and(|v| v.row_is_null(row as u64))
    }

    /// Read a non-NULL row.
    fn read(&self, row: usize) -> Result<Datum, Error> {
        let len = self.len;
        macro_rules! slice {
            ($vector:expr, $ty:ty) => {
                // The kind was derived from this vector's logical type, which
                // fixes its physical element type; `row` is below the chunk size.
                unsafe { $vector.as_slice_with_len::<$ty>(len)[row] }
            };
        }
        Ok(match self.kind {
            Kind::Null => return Err("NULL has no value".into()),
            Kind::Varchar => {
                let bytes = crate::unicode_carrier::bytes(&self.vector, row, len)?;
                Datum::Text(String::from_utf8(bytes)?, None)
            }
            Kind::Carrier => {
                let bytes = crate::unicode_carrier::bytes(self.first.as_ref().unwrap(), row, len)?;
                let units: Vec<u16> = bytes
                    .chunks(2)
                    .map(|pair| u16::from_le_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
                    .collect();
                Datum::Text(String::from_utf16_lossy(&units), Some(units))
            }
            Kind::Blob => Datum::Binary(crate::unicode_carrier::bytes(&self.vector, row, len)?),
            Kind::Boolean => Datum::Boolean(slice!(self.vector, u8) != 0),
            Kind::Integer(_, bits) => Datum::Integer(
                match self.vector.logical_type().id() {
                    Id::Tinyint => i128::from(slice!(self.vector, i8)),
                    Id::UTinyint => i128::from(slice!(self.vector, u8)),
                    Id::Smallint => i128::from(slice!(self.vector, i16)),
                    Id::Integer => i128::from(slice!(self.vector, i32)),
                    _ => i128::from(slice!(self.vector, i64)),
                },
                bits,
            ),
            Kind::Decimal(width, scale) => Datum::Decimal(
                match width {
                    1..=4 => i128::from(slice!(self.vector, i16)),
                    5..=9 => i128::from(slice!(self.vector, i32)),
                    10..=18 => i128::from(slice!(self.vector, i64)),
                    _ => {
                        let value = slice!(self.vector, duckdb::ffi::duckdb_hugeint);
                        (i128::from(value.upper) << 64) | i128::from(value.lower)
                    }
                },
                scale,
            ),
            Kind::Double => Datum::Double(slice!(self.vector, f64)),
            Kind::Float => Datum::Float(slice!(self.vector, f32)),
            Kind::Date => {
                let days = slice!(self.vector, i32);
                Datum::Temporal(
                    temporal::Value {
                        local: DateTime2::from_unix_nanos(i128::from(days) * NANOS_PER_DAY)?,
                        offset: None,
                    },
                    temporal::Source::Date,
                )
            }
            Kind::Timestamp(factor) => {
                let value = slice!(self.vector, i64);
                let local = DateTime2::from_unix_nanos(i128::from(value) * factor)?;
                // datetime and smalldatetime share this representation; a
                // smalldatetime has no seconds, so both format alike.
                Datum::Temporal(
                    temporal::Value {
                        local,
                        offset: None,
                    },
                    temporal::Source::DateTime,
                )
            }
            Kind::Time(factor) => {
                let nanos = i128::from(slice!(self.vector, i64)) * factor;
                let base = DateTime2::from_parts(crate::datetime2::Parts {
                    year: 1900,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                    fraction: 0,
                })?;
                Datum::Temporal(
                    temporal::Value {
                        local: DateTime2::from_ticks(base.ticks() + ((nanos + 50) / 100) as i64)?,
                        offset: None,
                    },
                    temporal::Source::Time(7),
                )
            }
            Kind::DateTime2(scale) => Datum::Temporal(
                temporal::Value {
                    local: DateTime2::from_ticks(slice!(self.first.as_ref().unwrap(), i64))?,
                    offset: None,
                },
                temporal::Source::DateTime2(scale),
            ),
            Kind::DateTimeOffset(scale) => {
                let utc = DateTime2::from_ticks(slice!(self.first.as_ref().unwrap(), i64))?;
                let offset = slice!(self.second.as_ref().unwrap(), i16);
                let value = DateTimeOffset::from_utc(utc, offset)?;
                Datum::Temporal(
                    temporal::Value {
                        local: value.local(),
                        offset: Some(offset),
                    },
                    temporal::Source::DateTimeOffset(scale),
                )
            }
            Kind::Other(name) => {
                return Err(format!("unsupported {name} value in a styled conversion").into());
            }
        })
    }
}

fn integer(chunk: &DataChunkHandle, index: usize, row: usize) -> Option<i32> {
    let vector = chunk.flat_vector(index);
    if vector.row_is_null(row as u64) {
        return None;
    }
    // INTEGER arguments by signature.
    Some(unsafe { vector.as_slice_with_len::<i32>(chunk.len())[row] })
}

fn flag(chunk: &DataChunkHandle, index: usize, row: usize) -> bool {
    let vector = chunk.flat_vector(index);
    // BOOLEAN arguments by signature.
    !vector.row_is_null(row as u64)
        && unsafe { vector.as_slice_with_len::<u8>(chunk.len())[row] } != 0
}

/// Format a float with SQL Server's float/real character styles.
fn float_text(value: f64, style: i32) -> Option<String> {
    let scientific = |digits: usize| {
        let text = format!("{:.*e}", digits - 1, value);
        let (mantissa, exponent) = text.split_once('e').expect("scientific notation");
        let exponent: i32 = exponent.parse().expect("exponent");
        format!(
            "{mantissa}e{}{:03}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
    };
    Some(match style {
        0 => {
            if value == 0.0 {
                return Some("0".into());
            }
            // Six significant digits, scientific outside 1e-4..1e6.
            let rounded: f64 = format!("{value:.5e}").parse().ok()?;
            let exponent = rounded.abs().log10().floor() as i32;
            if !(-4..6).contains(&exponent) {
                let text = scientific(6);
                let (mantissa, exp) = text.split_once('e')?;
                let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
                format!("{mantissa}e{exp}")
            } else {
                let decimals = (5 - exponent).max(0) as usize;
                let text = format!("{rounded:.decimals$}");
                if text.contains('.') {
                    text.trim_end_matches('0').trim_end_matches('.').to_owned()
                } else {
                    text
                }
            }
        }
        1 => scientific(8),
        2 => scientific(16),
        3 => scientific(17),
        _ => return None,
    })
}

fn temporal_failure(
    error: temporal::FormatError,
    source: temporal::Source,
    style: i32,
    target: i32,
) -> Error {
    let target = ["varchar", "char", "nvarchar", "nchar"][target.clamp(0, 3) as usize];
    match error {
        temporal::FormatError::InvalidStyle => format!(
            "{style} is not a valid style number when converting from {} to a character string.",
            source.name()
        )
        .into(),
        temporal::FormatError::NotApplicable => {
            format!("Error converting data type {} to {target}.", source.name()).into()
        }
        temporal::FormatError::Unsupported => {
            format!("unsupported style {style} (Hijri calendar) in CONVERT").into()
        }
    }
}

/// `__msduck_conversion_text(value, style, target, try)`.
struct Text;
impl VScalar for Text {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let source = Input::new(input, 0);
        let mut result = output.flat_vector();
        for row in 0..len {
            let (Some(style), Some(target)) = (integer(input, 1, row), integer(input, 2, row))
            else {
                result.set_null(row);
                continue;
            };
            if source.is_null(row) {
                result.set_null(row);
                continue;
            }
            let trying = flag(input, 3, row);
            let width = integer(input, 4, row).unwrap_or(-1);
            let scale = integer(input, 5, row).unwrap_or(-1);
            let text: Result<String, Error> = match source.read(row)? {
                Datum::Temporal(value, kind) => {
                    let kind = match kind {
                        temporal::Source::Time(_) if (0..=7).contains(&scale) => {
                            temporal::Source::Time(scale as u8)
                        }
                        kind => kind,
                    };
                    temporal::format(value, kind, style)
                        .map_err(|error| temporal_failure(error, kind, style, target))
                }
                Datum::Binary(mut bytes) => {
                    // Hexadecimal styles keep whole bytes within the width.
                    if width >= 0 {
                        let room = match style {
                            1 => (width - 2).max(0) / 2,
                            2 => width / 2,
                            _ => i32::MAX,
                        };
                        bytes.truncate(room as usize);
                    }
                    binary::to_text(&bytes, style, target >= 2).map_err(|_| {
                    format!(
                        "The style {style} is not supported for conversions from varbinary to {}.",
                        ["varchar", "char", "nvarchar", "nchar"][target.clamp(0, 3) as usize]
                    )
                    .into()
                    })
                }
                Datum::Double(value) => float_text(value, style)
                    .ok_or_else(|| format!("{style} is not a valid style number when converting from float to a character string.").into()),
                Datum::Float(value) => float_text(f64::from(value), style)
                    .ok_or_else(|| format!("{style} is not a valid style number when converting from real to a character string.").into()),
                Datum::Text(text, _) => Ok(text),
                Datum::Boolean(value) => Ok(if value { "1" } else { "0" }.into()),
                Datum::Integer(value, _) => Ok(value.to_string()),
                Datum::Decimal(coefficient, scale) => Ok(decimal_text(coefficient, scale)),
            };
            match text {
                Ok(text) => result.insert(row, text.as_str()),
                Err(_) if trying => result.set_null(row),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                Id::Any.into(),
                Id::Integer.into(),
                Id::Integer.into(),
                Id::Boolean.into(),
                Id::Integer.into(),
                Id::Integer.into(),
            ],
            Id::Varchar.into(),
        )]
    }
}

fn decimal_text(coefficient: i128, scale: u8) -> String {
    let digits = coefficient.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let text = if scale == 0 {
        digits
    } else if digits.len() > scale {
        format!(
            "{}.{}",
            &digits[..digits.len() - scale],
            &digits[digits.len() - scale..]
        )
    } else {
        format!("0.{}{digits}", "0".repeat(scale - digits.len()))
    };
    if coefficient < 0 {
        format!("-{text}")
    } else {
        text
    }
}

/// `__msduck_conversion_binary(value, style, width, fixed, try)`.
struct Binary;
impl VScalar for Binary {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let source = Input::new(input, 0);
        let mut result = output.flat_vector();
        for row in 0..len {
            let (Some(style), Some(width)) = (integer(input, 1, row), integer(input, 2, row))
            else {
                result.set_null(row);
                continue;
            };
            if source.is_null(row) {
                result.set_null(row);
                continue;
            }
            let fixed = flag(input, 3, row);
            let trying = flag(input, 4, row);
            let target = if fixed { "binary" } else { "varbinary" };
            let bytes: Result<Vec<u8>, Error> = match source.read(row)? {
                Datum::Binary(bytes) => Ok(bytes),
                Datum::Text(text, units) => {
                    let name = if units.is_some() {
                        "nvarchar"
                    } else {
                        "varchar"
                    };
                    binary::from_text(&text, style, units.as_deref()).map_err(|error| match error {
                        binary::Error::Style => format!(
                            "The style {style} is not supported for conversions from {name} to {target}."
                        )
                        .into(),
                        binary::Error::Syntax => {
                            format!("Error converting data type {name} to {target}.").into()
                        }
                    })
                }
                Datum::Integer(value, bits) => {
                    let bytes = value.to_be_bytes();
                    Ok(bytes[16 - usize::from(bits / 8)..].to_vec())
                }
                other => {
                    Err(format!("unsupported styled conversion of {other:?} to {target}").into())
                }
            };
            match bytes {
                Ok(bytes) => {
                    let bytes = binary::fit(bytes, width, fixed);
                    result.insert(row, bytes.as_slice());
                }
                Err(_) if trying => result.set_null(row),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                Id::Any.into(),
                Id::Integer.into(),
                Id::Integer.into(),
                Id::Boolean.into(),
                Id::Boolean.into(),
            ],
            Id::Blob.into(),
        )]
    }
}

/// `__msduck_conversion_temporal(value, style, target, try)`: ISO 8601 text
/// for the built-in conversion to the target date/time type.
struct Temporal;
impl VScalar for Temporal {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let source = Input::new(input, 0);
        let mut result = output.flat_vector();
        for row in 0..len {
            let (Some(style), Some(code)) = (integer(input, 1, row), integer(input, 2, row)) else {
                result.set_null(row);
                continue;
            };
            if source.is_null(row) {
                result.set_null(row);
                continue;
            }
            let trying = flag(input, 3, row);
            let target = temporal::Target::from_code(code).ok_or("invalid date/time target")?;
            let value: Result<temporal::Value, Error> = match source.read(row)? {
                Datum::Text(text, _) => temporal::parse(&text, style, target).map_err(|error| match error {
                    temporal::ParseError::Syntax => crate::datetime2_cast::CONVERSION.into(),
                    temporal::ParseError::Range => format!(
                        "The conversion of a varchar data type to a {} data type resulted in an out-of-range value.",
                        if target == temporal::Target::SmallDateTime { "smalldatetime" } else { "datetime" }
                    )
                    .into(),
                }),
                // The style does not apply to date/time sources.
                Datum::Temporal(value, kind) => Ok(match (kind, target) {
                    (temporal::Source::DateTimeOffset(_), temporal::Target::DateTimeOffset) => value,
                    (_, temporal::Target::DateTimeOffset) => temporal::Value {
                        local: value.local,
                        offset: Some(0),
                    },
                    _ => temporal::Value {
                        local: value.local,
                        offset: None,
                    },
                }),
                other => Err(format!("unsupported styled conversion of {other:?} to a date/time type").into()),
            };
            match value.map(|value| temporal::iso(value, target)) {
                Ok(Some(text)) => result.insert(row, text.as_str()),
                Ok(None) | Err(_) if trying => result.set_null(row),
                Ok(None) => return Err(crate::datetime2_cast::CONVERSION.into()),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                Id::Any.into(),
                Id::Integer.into(),
                Id::Integer.into(),
                Id::Boolean.into(),
            ],
            Id::Varchar.into(),
        )]
    }
}

/// Collation operands: the text of character values (`MODE` 0), without
/// the trailing spaces comparisons ignore (1), or that key in upper case for
/// case-insensitive name lookups (2).
struct Collation<const MODE: u8>;
impl<const MODE: u8> VScalar for Collation<MODE> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let source = Input::new(input, 0);
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.is_null(row) {
                result.set_null(row);
                continue;
            }
            match source.kind {
                Kind::Varchar | Kind::Carrier => {}
                kind => {
                    return Err(format!(
                        "Expression type {} is invalid for COLLATE clause.",
                        kind.sql_name()
                    )
                    .into());
                }
            }
            let Datum::Text(text, _) = source.read(row)? else {
                unreachable!("character input");
            };
            match MODE {
                0 => result.insert(row, text.as_str()),
                1 => result.insert(row, text.trim_end_matches(' ')),
                _ => result.insert(row, text.trim_end_matches(' ').to_uppercase().as_str()),
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

fn text_argument(
    chunk: &DataChunkHandle,
    index: usize,
    row: usize,
) -> Result<Option<String>, Error> {
    let input = Input::new(chunk, index);
    if input.is_null(row) {
        return Ok(None);
    }
    match input.read(row)? {
        Datum::Text(text, _) => Ok(Some(text)),
        _ => Err(format!(
            "Argument data type {} is invalid for argument {} of format function.",
            input.kind.sql_name(),
            index + 1
        )
        .into()),
    }
}

/// `__msduck_format(value, format, culture)`.
struct Format;
impl VScalar for Format {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        let len = input.len();
        let source = Input::new(input, 0);
        if matches!(
            source.kind,
            Kind::Varchar
                | Kind::Carrier
                | Kind::Blob
                | Kind::Boolean
                | Kind::Null
                | Kind::Other(_)
        ) {
            return Err(format!(
                "Argument data type {} is invalid for argument 1 of format function.",
                source.kind.sql_name()
            )
            .into());
        }
        let mut result = output.flat_vector();
        for row in 0..len {
            let culture_name = text_argument(input, 2, row)?;
            let culture = match culture_name.as_deref().map(dotnet::culture) {
                Some(Ok(culture)) => culture,
                Some(Err(dotnet::CultureError::Unsupported)) => {
                    return Err(format!(
                        "unsupported FORMAT culture {}",
                        culture_name.unwrap_or_default()
                    )
                    .into());
                }
                _ => {
                    return Err(format!(
                        "The culture parameter '{}' provided in the function call is not supported.",
                        culture_name.unwrap_or_else(|| "NULL".into())
                    )
                    .into());
                }
            };
            if source.is_null(row) {
                result.set_null(row);
                continue;
            }
            // A NULL format formats as if no format were given.
            let format = text_argument(input, 1, row)?.unwrap_or_default();
            let text = match source.read(row)? {
                Datum::Integer(value, bits) => {
                    dotnet::number(dotnet::Number::Integer(value, bits), &format, culture)
                }
                Datum::Decimal(coefficient, scale) => dotnet::number(
                    dotnet::Number::Decimal(coefficient, scale),
                    &format,
                    culture,
                ),
                Datum::Double(value) => {
                    dotnet::number(dotnet::Number::Double(value), &format, culture)
                }
                Datum::Float(value) => {
                    dotnet::number(dotnet::Number::Single(value), &format, culture)
                }
                Datum::Temporal(value, temporal::Source::Time(_)) => {
                    let ticks = value.local.ticks() % (86_400 * 10_000_000);
                    dotnet::span(ticks, &format, culture)
                }
                Datum::Temporal(value, kind) => {
                    let local = match kind {
                        // DATETIME reaches .NET rounded to milliseconds.
                        temporal::Source::DateTime => {
                            let ticks = value.local.ticks();
                            DateTime2::from_ticks((ticks + 5_000) / 10_000 * 10_000)?
                        }
                        _ => value.local,
                    };
                    dotnet::moment(
                        dotnet::Moment {
                            local,
                            offset: value.offset,
                        },
                        &format,
                        culture,
                    )
                }
                _ => None,
            };
            match text {
                Some(text) => result.insert(row, text.as_str()),
                None => result.set_null(row),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into(), Id::Any.into(), Id::Any.into()],
            Id::Varchar.into(),
        )]
    }
}

pub(super) fn register(db: &duckdb::Connection) -> anyhow::Result<()> {
    db.register_scalar_function::<Text>("__msduck_conversion_text")?;
    db.register_scalar_function::<Binary>("__msduck_conversion_binary")?;
    db.register_scalar_function::<Temporal>("__msduck_conversion_temporal")?;
    db.register_scalar_function::<Collation<0>>("__msduck_collation_text")?;
    db.register_scalar_function::<Collation<0>>("__msduck_collation_binary")?;
    db.register_scalar_function::<Collation<1>>("__msduck_collation_key")?;
    db.register_scalar_function::<Collation<2>>("__msduck_collation_upper")?;
    db.register_scalar_function::<Format>("__msduck_format")?;
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_conversion_variant(tag, value, text) AS
        CASE WHEN tag IS NULL THEN NULL ELSE struct_pack(
            __msduck_variant_type := CAST(tag AS UTINYINT),
            __msduck_variant_integer := CAST(value AS BIGINT),
            __msduck_variant_sysname := CAST(text AS VARCHAR)) END",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_styles_match_sql_server() {
        assert_eq!(float_text(1.5, 0).unwrap(), "1.5");
        assert_eq!(float_text(123456789.5, 0).unwrap(), "1.23457e+008");
        assert_eq!(float_text(1.5, 1).unwrap(), "1.5000000e+000");
        assert_eq!(float_text(1.5, 2).unwrap(), "1.500000000000000e+000");
        assert_eq!(float_text(1.5, 3).unwrap(), "1.5000000000000000e+000");
        assert_eq!(float_text(0.0, 0).unwrap(), "0");
        assert_eq!(float_text(-0.000012345, 0).unwrap(), "-1.2345e-005");
        assert_eq!(float_text(1.5, 4), None);
    }

    #[test]
    fn decimal_text_keeps_scale() {
        assert_eq!(decimal_text(125, 1), "12.5");
        assert_eq!(decimal_text(-5, 2), "-0.05");
        assert_eq!(decimal_text(42, 0), "42");
    }
}
