//! Declaration overrides for catalog type metadata. No catalog access.
use sqlparser::ast::*;

#[derive(Debug, PartialEq)]
pub struct Shape {
    pub name: &'static str,
    pub length: Option<i16>,
    pub precision: Option<u8>,
    pub scale: Option<u8>,
}
pub fn declaration(kind: &DataType) -> Option<Shape> {
    let mut result = Shape {
        name: "",
        length: None,
        precision: None,
        scale: None,
    };
    let chars = |length: &Option<CharacterLength>, unicode: bool| -> Option<i16> {
        match length {
            Some(CharacterLength::Max) => Some(-1),
            Some(CharacterLength::IntegerLength { length, .. }) => {
                i16::try_from(length.checked_mul(if unicode { 2 } else { 1 })?).ok()
            }
            None => Some(if unicode { 2 } else { 1 }),
        }
    };
    result.name = match kind {
        DataType::TinyInt(_) => "tinyint",
        DataType::SmallInt(_) => "smallint",
        DataType::Int(_) | DataType::Integer(_) => "int",
        DataType::BigInt(_) => "bigint",
        DataType::Bit(_) => "bit",
        DataType::Real => "real",
        DataType::Float(info) => {
            if matches!(info, ExactNumberInfo::Precision(1..=24)) {
                "real"
            } else {
                "float"
            }
        }
        DataType::Double(_) | DataType::DoublePrecision => "float",
        DataType::Date => "date",
        DataType::Datetime(_) => "datetime",
        DataType::Uuid => "uniqueidentifier",
        DataType::Text => "text",
        DataType::Varchar(length)
        | DataType::CharacterVarying(length)
        | DataType::CharVarying(length) => {
            result.length = Some(chars(length, false)?);
            "varchar"
        }
        DataType::Nvarchar(length) => {
            result.length = Some(chars(length, true)?);
            "nvarchar"
        }
        DataType::Char(length) | DataType::Character(length) => {
            result.length = Some(chars(length, false)?);
            "char"
        }
        DataType::Binary(length) => {
            result.length = Some(i16::try_from(length.unwrap_or(1)).ok()?);
            "binary"
        }
        DataType::Varbinary(length) => {
            result.length = Some(match length {
                Some(BinaryLength::Max) => -1,
                Some(BinaryLength::IntegerLength { length }) => i16::try_from(*length).ok()?,
                None => 1,
            });
            "varbinary"
        }
        DataType::Decimal(info) | DataType::Numeric(info) | DataType::Dec(info) => {
            let (p, s) = match info {
                ExactNumberInfo::None => (18, 0),
                ExactNumberInfo::Precision(p) => (*p, 0),
                ExactNumberInfo::PrecisionAndScale(p, s) => (*p, *s),
            };
            result.precision = Some(u8::try_from(p).ok()?);
            result.scale = Some(u8::try_from(s).ok()?);
            result.length = Some(match p {
                1..=9 => 5,
                10..=19 => 9,
                20..=28 => 13,
                29..=38 => 17,
                _ => return None,
            });
            if matches!(kind, DataType::Numeric(_)) {
                "numeric"
            } else {
                "decimal"
            }
        }
        DataType::Time(scale, TimezoneInfo::None) => {
            let s = u8::try_from(scale.unwrap_or(7)).ok()?;
            if s > 7 {
                return None;
            }
            result.scale = Some(s);
            result.precision = Some(if s == 0 { 8 } else { 9 + s });
            result.length = Some(time_length(s));
            "time"
        }
        DataType::Custom(name, args) => match name.to_string().to_ascii_lowercase().as_str() {
            "datetime2" | "datetimeoffset" => {
                let s = match args.as_slice() {
                    [] => 7,
                    [s] => s.parse::<u8>().ok()?,
                    _ => return None,
                };
                if s > 7 {
                    return None;
                }
                let offset = name.to_string().eq_ignore_ascii_case("datetimeoffset");
                result.scale = Some(s);
                result.precision =
                    Some((if s == 0 { 19 } else { 20 + s }) + if offset { 7 } else { 0 });
                result.length = Some(time_length(s) + if offset { 5 } else { 3 });
                if offset {
                    "datetimeoffset"
                } else {
                    "datetime2"
                }
            }
            "nchar" => {
                let n = match args.as_slice() {
                    [] => 1,
                    [n] => n.parse::<u64>().ok()?,
                    _ => return None,
                };
                result.length = Some(i16::try_from(n.checked_mul(2)?).ok()?);
                "nchar"
            }
            "money" => "money",
            "smallmoney" => "smallmoney",
            "smalldatetime" => "smalldatetime",
            "uniqueidentifier" => "uniqueidentifier",
            "sysname" => "sysname",
            "ntext" => "ntext",
            "image" => "image",
            "xml" => "xml",
            "sql_variant" => "sql_variant",
            "timestamp" | "rowversion" => "timestamp",
            _ => return None,
        },
        _ => return None,
    };
    Some(result)
}
fn time_length(scale: u8) -> i16 {
    match scale {
        0..=2 => 3,
        3..=4 => 4,
        _ => 5,
    }
}

pub fn cast(kind: &DataType) -> Option<Shape> {
    let mut s = declaration(kind)?;
    // CAST/CONVERT omitted character lengths default to 30, unlike DDL's 1.
    if matches!(
        kind,
        DataType::Varchar(None)
            | DataType::Char(None)
            | DataType::Character(None)
            | DataType::CharVarying(None)
            | DataType::CharacterVarying(None)
            | DataType::Binary(None)
            | DataType::Varbinary(None)
    ) {
        s.length = Some(30)
    }
    if matches!(kind, DataType::Nvarchar(None))
        || matches!(kind,DataType::Custom(n,a) if n.to_string().eq_ignore_ascii_case("nchar") && a.is_empty())
    {
        s.length = Some(60)
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declaration_widths_cover_decimal_and_temporal_boundaries() {
        for (p, length) in [
            (1, 5),
            (9, 5),
            (10, 9),
            (19, 9),
            (20, 13),
            (28, 13),
            (29, 17),
            (38, 17),
        ] {
            let s =
                declaration(&DataType::Decimal(ExactNumberInfo::PrecisionAndScale(p, 1))).unwrap();
            assert_eq!(
                (s.length, s.precision, s.scale),
                (Some(length), Some(p as u8), Some(1))
            );
        }
        for scale in 0..=7 {
            let kind = DataType::Custom(
                ObjectName::from(vec![Ident::new("datetime2")]),
                vec![scale.to_string()],
            );
            let s = declaration(&kind).unwrap();
            assert_eq!(
                s.length,
                Some(match scale {
                    0..=2 => 6,
                    3..=4 => 7,
                    _ => 8,
                })
            );
            assert_eq!(s.precision, Some(if scale == 0 { 19 } else { 20 + scale }));
        }
        assert_eq!(
            declaration(&DataType::Nvarchar(None)).unwrap().length,
            Some(2)
        );
        assert_eq!(
            declaration(&DataType::Nvarchar(Some(CharacterLength::Max)))
                .unwrap()
                .length,
            Some(-1)
        );
        assert_eq!(
            declaration(&DataType::Binary(None)).unwrap().length,
            Some(1)
        );
    }
}

#[cfg(test)]
mod cast_tests {
    use super::*;
    #[test]
    fn omitted_cast_lengths_differ_from_declarations_but_explicit_lengths_do_not() {
        for (kind, ddl, conversion) in [
            (DataType::Varchar(None), 1, 30),
            (DataType::Nvarchar(None), 2, 60),
            (DataType::Binary(None), 1, 30),
            (DataType::Varbinary(None), 1, 30),
            (
                DataType::Custom(ObjectName::from(vec![Ident::new("nchar")]), vec![]),
                2,
                60,
            ),
        ] {
            assert_eq!(declaration(&kind).unwrap().length, Some(ddl));
            assert_eq!(cast(&kind).unwrap().length, Some(conversion));
        }
        for kind in [
            DataType::Nvarchar(Some(CharacterLength::Max)),
            DataType::Binary(Some(17)),
            DataType::Time(Some(2), TimezoneInfo::None),
        ] {
            assert_eq!(declaration(&kind), cast(&kind));
        }
    }
}
