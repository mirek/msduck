//! Parser adapters for logical scalar declarations. No database access.
use anyhow::{Result, bail};
use msduck_core::{
    character::{CharacterType, Family, Length},
    types::{BinaryType, DecimalType, Scale, Type},
};
use sqlparser::ast::*;

/// Resolve a declaration's defaults. CAST defaults are handled by CAST lowering.
pub fn declaration(kind: &DataType) -> Result<Type> {
    let character = |family, length: &Option<CharacterLength>| -> Result<Type> {
        let length = match length {
            None => Length::Bounded(1),
            Some(CharacterLength::Max) => Length::Max,
            Some(CharacterLength::IntegerLength { length, unit: None }) => {
                Length::Bounded(u16::try_from(*length)?)
            }
            _ => bail!("unsupported character length unit"),
        };
        Ok(Type::Character(CharacterType::new(family, length)?))
    };
    if let Some(scale) = crate::temporal_scale::datetime2(kind).map_err(anyhow::Error::msg)? {
        return Ok(Type::DateTime2(Scale::new(scale)?));
    }
    if let Some(scale) = crate::temporal_scale::datetimeoffset(kind).map_err(anyhow::Error::msg)? {
        return Ok(Type::DateTimeOffset(Scale::new(scale)?));
    }
    Ok(match kind {
        DataType::Bit(_) | DataType::Boolean => Type::Bit,
        DataType::TinyInt(_) | DataType::UTinyInt => Type::TinyInt,
        DataType::SmallInt(_) => Type::SmallInt,
        DataType::Int(_) | DataType::Integer(_) => Type::Int,
        DataType::BigInt(_) => Type::BigInt,
        DataType::Real => Type::Real,
        DataType::Double(_) | DataType::DoublePrecision => Type::Float,
        DataType::Float(info) => match info {
            ExactNumberInfo::None | ExactNumberInfo::Precision(25..=53) => Type::Float,
            ExactNumberInfo::Precision(1..=24) => Type::Real,
            _ => bail!("FLOAT precision must be between 1 and 53"),
        },
        DataType::Decimal(info) | DataType::Numeric(info) | DataType::Dec(info) => {
            let (precision, scale) = match info {
                ExactNumberInfo::None => (18, 0),
                ExactNumberInfo::Precision(p) => (*p, 0),
                ExactNumberInfo::PrecisionAndScale(p, s) => (*p, *s),
            };
            Type::Decimal(DecimalType::new(
                u8::try_from(precision)?,
                u8::try_from(scale)?,
            )?)
        }
        DataType::Varchar(n) | DataType::CharacterVarying(n) | DataType::CharVarying(n) => {
            character(Family::Varchar, n)?
        }
        DataType::Char(n) | DataType::Character(n) => character(Family::Char, n)?,
        DataType::Nvarchar(n) => character(Family::Nvarchar, n)?,
        DataType::Binary(n) => Type::Binary(BinaryType::new(
            true,
            Length::Bounded(u16::try_from(n.unwrap_or(1))?),
        )?),
        DataType::Varbinary(n) => Type::Binary(BinaryType::new(
            false,
            match n {
                None => Length::Bounded(1),
                Some(BinaryLength::Max) => Length::Max,
                Some(BinaryLength::IntegerLength { length }) => {
                    Length::Bounded(u16::try_from(*length)?)
                }
            },
        )?),
        DataType::Date => Type::Date,
        DataType::Datetime(_) => Type::DateTime,
        DataType::Time(s, TimezoneInfo::None) => {
            let scale = s.unwrap_or(7);
            anyhow::ensure!(scale <= 7, "invalid time scale");
            Type::Time(Scale::new(scale as u8)?)
        }
        DataType::Uuid => Type::UniqueIdentifier,
        DataType::Text => Type::Text,
        DataType::Custom(name, args) => match name.to_string().to_ascii_lowercase().as_str() {
            "money" if args.is_empty() => Type::Money,
            "smallmoney" if args.is_empty() => Type::SmallMoney,
            "smalldatetime" if args.is_empty() => Type::SmallDateTime,
            "uniqueidentifier" if args.is_empty() => Type::UniqueIdentifier,
            "nchar" => {
                let n = match args.as_slice() {
                    [] => 1,
                    [n] => n.parse()?,
                    _ => bail!("NCHAR requires at most one length"),
                };
                Type::Character(CharacterType::new(Family::Nchar, Length::Bounded(n))?)
            }
            "ntext" if args.is_empty() => Type::Ntext,
            "image" if args.is_empty() => Type::Image,
            "xml" if args.is_empty() => Type::Xml,
            "sql_variant" if args.is_empty() => Type::Variant,
            _ => bail!("unsupported scalar declaration {kind}"),
        },
        _ => bail!("unsupported scalar declaration {kind}"),
    })
}

/// Reconstruct a logical AST declaration for existing compiler transforms.
pub fn ast(kind: Type) -> DataType {
    let custom =
        |name: &str, args| DataType::Custom(ObjectName::from(vec![Ident::new(name)]), args);
    match kind {
        Type::Bit => DataType::Bit(None),
        Type::TinyInt => DataType::TinyInt(None),
        Type::SmallInt => DataType::SmallInt(None),
        Type::Int => DataType::Int(None),
        Type::BigInt => DataType::BigInt(None),
        Type::Real => DataType::Real,
        Type::Float => DataType::Double(ExactNumberInfo::None),
        Type::Decimal(d) => DataType::Decimal(ExactNumberInfo::PrecisionAndScale(
            u64::from(d.precision()),
            i64::from(d.scale()),
        )),
        Type::Character(c) => {
            let n = Some(match c.length() {
                Length::Max => CharacterLength::Max,
                Length::Bounded(n) => CharacterLength::IntegerLength {
                    length: u64::from(n),
                    unit: None,
                },
            });
            match c.family() {
                Family::Char => DataType::Char(n),
                Family::Varchar => DataType::Varchar(n),
                Family::Nvarchar => DataType::Nvarchar(n),
                Family::Nchar => {
                    let Length::Bounded(n) = c.length() else {
                        unreachable!("validated fixed length")
                    };
                    custom("nchar", vec![n.to_string()])
                }
            }
        }
        Type::Binary(b) => match b.length() {
            Length::Bounded(n) if b.fixed() => DataType::Binary(Some(u64::from(n))),
            Length::Bounded(n) => DataType::Varbinary(Some(BinaryLength::IntegerLength {
                length: u64::from(n),
            })),
            Length::Max => DataType::Varbinary(Some(BinaryLength::Max)),
        },
        Type::Money => custom("money", vec![]),
        Type::SmallMoney => custom("smallmoney", vec![]),
        Type::Date => DataType::Date,
        Type::DateTime => DataType::Datetime(None),
        Type::SmallDateTime => custom("smalldatetime", vec![]),
        Type::Time(s) => DataType::Time(Some(u64::from(s.get())), TimezoneInfo::None),
        Type::DateTime2(s) => custom("datetime2", vec![s.get().to_string()]),
        Type::DateTimeOffset(s) => custom("datetimeoffset", vec![s.get().to_string()]),
        Type::UniqueIdentifier => DataType::Uuid,
        Type::Text => DataType::Text,
        Type::Ntext => custom("ntext", vec![]),
        Type::Image => custom("image", vec![]),
        Type::Xml => custom("xml", vec![]),
        Type::Variant => custom("sql_variant", vec![]),
    }
}

pub fn integral_type(kind: &DataType) -> bool {
    matches!(
        kind,
        DataType::TinyInt(_)
            | DataType::UTinyInt
            | DataType::SmallInt(_)
            | DataType::Int(_)
            | DataType::Integer(_)
            | DataType::BigInt(_)
    )
}
