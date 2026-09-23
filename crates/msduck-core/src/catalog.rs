//! Backend-independent SQL Server catalog type metadata.
//!
//! Catalog identity is kept separately from value representation: alias types
//! can share a system type while retaining their own user type and collation.
//! Nullable properties remain unknown, rather than acquiring guessed defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeMetadata {
    pub system_type_id: Option<u8>,
    pub user_type_id: Option<i32>,
    /// Byte length; -1 is the catalog marker for a MAX declaration.
    pub max_length: Option<i16>,
    pub precision: Option<u8>,
    pub scale: Option<u8>,
    pub collation_name: Option<String>,
}

impl TypeMetadata {
    /// Recover the scalar declaration for rebinding a value. Unknown or
    /// incomplete metadata stays unknown; physical values cannot supply it.
    pub fn logical_type(&self) -> Option<crate::types::Type> {
        use crate::character::{CharacterType, Family, Length};
        use crate::types::{BinaryType, DecimalType, Scale, Type};
        let length = |unicode: bool| {
            let bytes = self.max_length?;
            match bytes {
                -1 => Some(Length::Max),
                1.. if !unicode || bytes % 2 == 0 => {
                    Some(Length::Bounded(bytes as u16 / if unicode { 2 } else { 1 }))
                }
                _ => None,
            }
        };
        Some(match self.system_type_id? {
            104 => Type::Bit,
            48 => Type::TinyInt,
            52 => Type::SmallInt,
            56 => Type::Int,
            127 => Type::BigInt,
            59 => Type::Real,
            62 => Type::Float,
            106 | 108 => Type::Decimal(DecimalType::new(self.precision?, self.scale?).ok()?),
            60 => Type::Money,
            122 => Type::SmallMoney,
            id @ (167 | 175 | 231 | 239) => Type::Character(
                CharacterType::new(
                    match id {
                        167 => Family::Varchar,
                        175 => Family::Char,
                        231 => Family::Nvarchar,
                        _ => Family::Nchar,
                    },
                    length(matches!(id, 231 | 239))?,
                )
                .ok()?,
            ),
            id @ (165 | 173) => Type::Binary(BinaryType::new(id == 173, length(false)?).ok()?),
            40 => Type::Date,
            61 => Type::DateTime,
            58 => Type::SmallDateTime,
            41 => Type::Time(Scale::new(self.scale?).ok()?),
            42 => Type::DateTime2(Scale::new(self.scale?).ok()?),
            43 => Type::DateTimeOffset(Scale::new(self.scale?).ok()?),
            36 => Type::UniqueIdentifier,
            35 => Type::Text,
            99 => Type::Ntext,
            34 => Type::Image,
            241 => Type::Xml,
            98 => Type::Variant,
            _ => return None,
        })
    }

    pub fn time_scale(&self) -> Option<u8> {
        (self.system_type_id == Some(41))
            .then_some(self.scale)
            .flatten()
            .filter(|scale| *scale <= 7)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character::{CharacterType, Family, Length},
        types::{DecimalType, Type},
    };

    #[test]
    fn rebinding_retains_logical_families_and_rejects_incomplete_shapes() {
        let mut info = TypeMetadata {
            system_type_id: Some(231),
            max_length: Some(8),
            ..Default::default()
        };
        assert_eq!(
            info.logical_type(),
            Some(Type::Character(
                CharacterType::new(Family::Nvarchar, Length::Bounded(4)).unwrap()
            ))
        );
        info.max_length = Some(-1);
        assert_eq!(
            info.logical_type(),
            Some(Type::Character(
                CharacterType::new(Family::Nvarchar, Length::Max).unwrap()
            ))
        );
        for width in [None, Some(0), Some(3), Some(-2)] {
            info.max_length = width;
            assert_eq!(info.logical_type(), None);
        }
        info.system_type_id = Some(60);
        assert_eq!(info.logical_type(), Some(Type::Money));
        info.system_type_id = Some(106);
        assert_eq!(info.logical_type(), None);
        info.precision = Some(19);
        info.scale = Some(4);
        assert_eq!(
            info.logical_type(),
            Some(Type::Decimal(DecimalType::new(19, 4).unwrap()))
        );
        info.scale = Some(20);
        assert_eq!(info.logical_type(), None);
        info.system_type_id = Some(42);
        info.scale = Some(8);
        assert_eq!(info.logical_type(), None);
        info.system_type_id = None;
        assert_eq!(info.logical_type(), None);
    }
}
