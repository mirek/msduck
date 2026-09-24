//! Character set-result types shared by projection and operand binding.
use crate::{catalog_shape, catalog_snapshot::CatalogSnapshot};
use msduck_core::catalog::TypeMetadata;
use sqlparser::ast::{CharacterLength, DataType, Ident, ObjectName};

/// Character + declarations use encoding/family precedence and summed capacity,
/// independently of collation-label precedence and physical text representation.
pub fn concat_info(
    catalog: &CatalogSnapshot,
    left: &TypeMetadata,
    right: &TypeMetadata,
) -> Option<TypeMetadata> {
    use msduck_core::character::{Family, Length};
    let shape = |info: &TypeMetadata| {
        let id = info.system_type_id?;
        if info.user_type_id.is_some_and(|user| user != i32::from(id)) {
            return None;
        }
        let (family, unicode, fixed) = match id {
            167 => (Family::Varchar, false, false),
            175 => (Family::Char, false, true),
            231 => (Family::Nvarchar, true, false),
            239 => (Family::Nchar, true, true),
            _ => return None,
        };
        let length = match info.max_length? {
            -1 if !fixed => Length::Max,
            n @ 0..=8000 if !unicode || n % 2 == 0 => {
                Length::Bounded((n / if unicode { 2 } else { 1 }) as u16)
            }
            _ => return None,
        };
        Some((family, length))
    };
    let (family, length) = msduck_core::concat::shape(shape(left)?, shape(right)?);
    let (name, unicode) = match family {
        Family::Char => ("char", false),
        Family::Varchar => ("varchar", false),
        Family::Nchar => ("nchar", true),
        Family::Nvarchar => ("nvarchar", true),
    };
    let mut info = catalog.types.get(name)?.clone();
    info.max_length = Some(match length {
        Length::Max => -1,
        Length::Bounded(n) => i16::try_from(n * if unicode { 2 } else { 1 }).ok()?,
    });
    Some(info)
}

#[derive(Clone, Copy)]
struct Shape {
    unicode: bool,
    fixed: bool,
    bytes: i16,
}
impl Shape {
    fn new(name: &str, bytes: i16) -> Option<Self> {
        let (unicode, fixed) = match name {
            "varchar" => (false, false),
            "char" => (false, true),
            "nvarchar" => (true, false),
            "nchar" => (true, true),
            _ => return None,
        };
        if !((bytes == -1 && !fixed)
            || ((1..=8000).contains(&bytes) && (!unicode || bytes % 2 == 0)))
        {
            return None;
        }
        Some(Self {
            unicode,
            fixed,
            bytes,
        })
    }
    fn ast(kind: &DataType) -> Option<Self> {
        let shape = catalog_shape::cast(kind)?;
        Self::new(shape.name, shape.length?)
    }
    fn info(info: &TypeMetadata) -> Option<Self> {
        let id = info.system_type_id?;
        // Alias-type precedence needs the declared identity, not just storage.
        if info.user_type_id.is_some_and(|user| user != i32::from(id)) {
            return None;
        }
        Self::new(
            match id {
                167 => "varchar",
                175 => "char",
                231 => "nvarchar",
                239 => "nchar",
                _ => return None,
            },
            info.max_length?,
        )
    }
    fn merge(self, other: Self) -> Option<DataType> {
        // Cross-encoding conversion and collation-label precedence are not
        // represented by this rule. Preserve unknown instead of guessing.
        if self.unicode != other.unicode {
            return None;
        }
        let bytes = if self.bytes == -1 || other.bytes == -1 {
            -1
        } else {
            self.bytes.max(other.bytes)
        };
        let units = if self.unicode && bytes != -1 {
            bytes / 2
        } else {
            bytes
        };
        let length = Some(if units == -1 {
            CharacterLength::Max
        } else {
            CharacterLength::IntegerLength {
                length: units as u64,
                unit: None,
            }
        });
        Some(match (self.unicode, self.fixed && other.fixed) {
            (false, false) => DataType::Varchar(length),
            (false, true) => DataType::Char(length),
            (true, false) => DataType::Nvarchar(length),
            (true, true) => DataType::Custom(
                ObjectName::from(vec![Ident::new("nchar")]),
                vec![units.to_string()],
            ),
        })
    }
}

pub fn set_type(left: &DataType, right: &DataType) -> Option<DataType> {
    Shape::ast(left)?.merge(Shape::ast(right)?)
}

pub fn set_info(
    catalog: &CatalogSnapshot,
    left: &TypeMetadata,
    right: &TypeMetadata,
) -> Option<TypeMetadata> {
    if left.collation_name != right.collation_name {
        return None;
    }
    let kind = Shape::info(left)?.merge(Shape::info(right)?)?;
    let mut info = catalog.cast_info(&kind)?;
    info.collation_name = left.collation_name.clone();
    Some(info)
}

pub fn nchar_cast_width(kind: &DataType) -> Result<Option<u16>, String> {
    let DataType::Custom(name, args) = kind else {
        return Ok(None);
    };
    if !name.to_string().eq_ignore_ascii_case("nchar") {
        return Ok(None);
    }
    match args.as_slice() {
        [] => Ok(Some(30)),
        [length] => length
            .parse::<u16>()
            .ok()
            .filter(|n| (1..=4000).contains(n))
            .map(Some)
            .ok_or_else(|| "NCHAR length must be between 1 and 4000".into()),
        _ => Err("NCHAR requires at most one length".into()),
    }
}

pub fn varchar_cast_width(kind: &DataType) -> Result<u16, String> {
    match kind {
        DataType::Varchar(None) | DataType::Char(None) | DataType::Character(None) => Ok(30),
        DataType::Varchar(Some(CharacterLength::Max)) => Ok(u16::MAX),
        DataType::Varchar(Some(CharacterLength::IntegerLength { length, unit: None }))
        | DataType::Char(Some(CharacterLength::IntegerLength { length, unit: None }))
        | DataType::Character(Some(CharacterLength::IntegerLength { length, unit: None }))
            if (1..=8000).contains(length) =>
        {
            Ok(*length as u16)
        }
        _ => Err("invalid VARCHAR length".into()),
    }
}

pub fn nvarchar_cast_width(kind: &sqlparser::ast::DataType) -> Result<Option<u16>, String> {
    use sqlparser::ast::*;
    match kind {
        DataType::Nvarchar(None) => Ok(Some(30)),
        DataType::Nvarchar(Some(CharacterLength::Max)) => Ok(None),
        DataType::Nvarchar(Some(CharacterLength::IntegerLength { length, unit: None }))
            if (1..=4000).contains(length) =>
        {
            Ok(Some(*length as u16))
        }
        _ => Err("invalid NVARCHAR length".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn kind(sql: &str) -> DataType {
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql(sql)
            .unwrap()
            .parse_data_type()
            .unwrap()
    }
    #[test]
    fn catalog_merge_preserves_collation_and_defers_conflicting_identities() {
        let mut catalog = CatalogSnapshot::default();
        let left = TypeMetadata {
            system_type_id: Some(167),
            user_type_id: Some(167),
            max_length: Some(3),
            collation_name: Some("source_collation".into()),
            ..Default::default()
        };
        catalog.types.insert(
            "varchar".into(),
            TypeMetadata {
                collation_name: Some("different_default".into()),
                ..left.clone()
            },
        );
        let mut right = TypeMetadata {
            max_length: Some(7),
            ..left.clone()
        };
        let merged = set_info(&catalog, &left, &right).unwrap();
        assert_eq!(merged.max_length, Some(7));
        assert_eq!(merged.collation_name, left.collation_name);
        right.collation_name = Some("conflicting".into());
        assert!(set_info(&catalog, &left, &right).is_none());
        right.collation_name = left.collation_name.clone();
        right.user_type_id = Some(5000);
        assert!(set_info(&catalog, &left, &right).is_none());
    }

    #[test]
    fn set_widths_preserve_encoding_fixed_families_and_max() {
        for (a, b, expected) in [
            ("VARCHAR(3)", "VARCHAR(7)", "VARCHAR(7)"),
            ("CHAR(3)", "VARCHAR(7)", "VARCHAR(7)"),
            ("CHAR(3)", "CHAR(7)", "CHAR(7)"),
            ("NCHAR(3)", "NVARCHAR(7)", "NVARCHAR(7)"),
            ("NCHAR(3)", "NCHAR(7)", "nchar(7)"),
            ("VARCHAR(MAX)", "CHAR(7)", "VARCHAR(MAX)"),
            ("NVARCHAR(7)", "NVARCHAR(MAX)", "NVARCHAR(MAX)"),
        ] {
            let expected = catalog_shape::cast(&kind(expected)).unwrap();
            for (a, b) in [(a, b), (b, a)] {
                let actual = set_type(&kind(a), &kind(b)).unwrap();
                let actual = catalog_shape::cast(&actual).unwrap();
                assert_eq!(
                    (actual.name, actual.length),
                    (expected.name, expected.length)
                );
            }
        }
        assert!(set_type(&kind("VARCHAR(3)"), &kind("NVARCHAR(3)")).is_none());
        assert!(set_type(&kind("INT"), &kind("VARCHAR(3)")).is_none());
    }
}
