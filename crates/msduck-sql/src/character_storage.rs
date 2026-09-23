//! Character target validation and deterministic storage AST construction.
pub use msduck_core::character::TRUNCATED;
use msduck_core::character::{CharacterType, Error, Family, INVALID_LENGTH, Length};
use sqlparser::ast::*;
#[derive(Clone, Copy)]
struct Spec {
    unicode: bool,
    fixed: bool,
    width: i32,
}
impl Spec {
    fn domain(self) -> Result<CharacterType, Error> {
        target(self.unicode, self.fixed, self.width)
    }
}
fn spec(kind: &DataType) -> Option<Spec> {
    let (unicode, fixed, width) = match kind {
        DataType::Varchar(n) | DataType::CharacterVarying(n) | DataType::CharVarying(n) => {
            (false, false, *n)
        }
        DataType::Char(n) | DataType::Character(n) => (false, true, *n),
        DataType::Nvarchar(n) => (true, false, *n),
        DataType::Custom(name, args) if name.to_string().eq_ignore_ascii_case("nchar") => {
            let width = match args.as_slice() {
                [] => 1,
                [n] => n.parse::<i32>().unwrap_or(0),
                _ => 0,
            };
            return Some(Spec {
                unicode: true,
                fixed: true,
                width,
            });
        }
        _ => return None,
    };
    Some(Spec {
        unicode,
        fixed,
        width: match width {
            None => 1,
            Some(CharacterLength::Max) => -1,
            Some(CharacterLength::IntegerLength { length, unit: None }) => {
                i32::try_from(length).unwrap_or(0)
            }
            _ => 0,
        },
    })
}
pub fn is_character(kind: &DataType) -> bool {
    spec(kind).is_some()
}
pub fn storage_kind(name: &str) -> Option<DataType> {
    let rest = name.strip_prefix("__MSDUCK_")?;
    let (kind, n) = rest.split_once('(')?;
    let n = n.strip_suffix(')')?.parse::<i32>().ok()?;
    let length = Some(if n == -1 {
        CharacterLength::Max
    } else {
        CharacterLength::IntegerLength {
            length: u64::try_from(n).ok()?,
            unit: None,
        }
    });
    match kind {
        "VARCHAR" => Some(DataType::Varchar(length)),
        "CHAR" => Some(DataType::Char(length)),
        "NVARCHAR" => Some(DataType::Nvarchar(length)),
        "NCHAR" => Some(DataType::Custom(
            ObjectName::from(vec![Ident::new("nchar")]),
            vec![n.to_string()],
        )),
        _ => None,
    }
}
pub fn convert(value: Expr, kind: &DataType) -> Option<Expr> {
    convert_with_layout(value, kind, Layout::Utf8)
}

/// Physical representation supplied by the catalog adapter, independently of
/// the logical character declaration.
#[derive(Clone, Copy)]
pub enum Layout {
    Utf8,
    Utf16,
}

pub fn convert_with_layout(value: Expr, kind: &DataType, layout: Layout) -> Option<Expr> {
    let spec = spec(kind)?;
    if matches!(layout, Layout::Utf16) {
        let function = match (spec.unicode, spec.fixed) {
            (true, false) => "__msduck_store_carrier_nvarchar",
            (true, true) => "__msduck_store_carrier_nchar",
            // A carrier is a Unicode storage representation, not an ANSI one.
            (false, _) => return None,
        };
        return Some(crate::expr::binary_function(
            function,
            crate::expr::unary_function("__msduck_carrier_input", value),
            Expr::Value(Value::Number(spec.width.to_string(), false).into()),
        ));
    }
    let function = match (spec.unicode, spec.fixed) {
        (false, false) => "__msduck_store_varchar",
        (false, true) => "__msduck_store_char",
        (true, false) => "__msduck_store_nvarchar",
        (true, true) => "__msduck_store_nchar",
    };
    Some(crate::expr::binary_function(
        function,
        Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(value),
            data_type: DataType::Text,
            format: None,
        },
        Expr::Value(Value::Number(spec.width.to_string(), false).into()),
    ))
}
pub fn column(column: &mut ColumnDef) -> Result<(), String> {
    let Some(spec) = spec(&column.data_type) else {
        return Ok(());
    };
    validate(spec).map_err(str::to_owned)?;
    for option in &mut column.options {
        if let ColumnOption::Default(value) = &mut option.option {
            *value = convert(value.clone(), &column.data_type).unwrap();
        }
    }
    // CHAR and NCHAR also use VARCHAR storage; the catalog retains the declaration.
    column.data_type = DataType::Varchar(None);
    Ok(())
}
/// Constant installation for ALTER ADD avoids DuckDB's outstanding-update
/// restriction when NOT NULL follows an expression default. The executable
/// default is restored separately for future writes.
pub fn constant_default(value: &Expr, kind: &DataType) -> Result<Option<Expr>, String> {
    if let Expr::Nested(value) = value {
        return constant_default(value, kind);
    }
    let Some(spec) = spec(kind) else {
        return Ok(None);
    };
    let text = match value {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) => text,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    let value = store(text, spec).map_err(|e| e.to_string())?;
    Ok(Some(Expr::Value(Value::SingleQuotedString(value).into())))
}
fn validate(spec: Spec) -> Result<(), &'static str> {
    spec.domain().map(|_| ()).map_err(|_| INVALID_LENGTH)
}
fn store(text: &str, spec: Spec) -> Result<String, Box<dyn std::error::Error>> {
    Ok(spec.domain()?.store(text)?)
}

/// Decode the explicit storage-function target using SQL character rules.
pub fn target(unicode: bool, fixed: bool, width: i32) -> Result<CharacterType, Error> {
    let family = match (unicode, fixed) {
        (false, false) => Family::Varchar,
        (false, true) => Family::Char,
        (true, false) => Family::Nvarchar,
        (true, true) => Family::Nchar,
    };
    let length = if width == -1 {
        Length::Max
    } else {
        Length::Bounded(width.try_into().map_err(|_| Error::InvalidLength)?)
    };
    CharacterType::new(family, length)
}
