//! Explicit catalog inputs for deterministic binding; acquisition belongs to adapters.
use crate::{binding_scope::Field, catalog_shape};
use msduck_core::catalog::TypeMetadata;
use sqlparser::ast::DataType;
use std::collections::HashMap;

#[derive(Clone, Default)]
pub struct CatalogSnapshot {
    /// Current database default supplied explicitly by the catalog adapter.
    pub default_collation: Option<String>,
    /// Keys use the original AST name spelling, resolved by the acquiring adapter.
    pub tables: HashMap<String, Vec<Field>>,
    /// Type names use the catalog's canonical spelling.
    pub types: HashMap<String, TypeMetadata>,
}

impl CatalogSnapshot {
    pub fn cast_info(&self, kind: &DataType) -> Option<TypeMetadata> {
        let shape = catalog_shape::cast(kind)?;
        let mut info = self.types.get(shape.name)?.clone();
        info.max_length = shape.length.or(info.max_length);
        info.precision = shape.precision.or(info.precision);
        info.scale = shape.scale.or(info.scale);
        Some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::{CharacterLength, TimezoneInfo};
    #[test]
    fn cast_overrides_preserve_catalog_identity_and_unknown_defaults() {
        let mut snapshot = CatalogSnapshot::default();
        let kind = DataType::Nvarchar(Some(CharacterLength::Max));
        assert!(snapshot.cast_info(&kind).is_none());
        snapshot.types.insert(
            "nvarchar".into(),
            TypeMetadata {
                system_type_id: Some(231),
                user_type_id: Some(231),
                max_length: Some(8000),
                precision: None,
                scale: Some(0),
                collation_name: Some("example_collation".into()),
            },
        );
        let info = snapshot.cast_info(&kind).unwrap();
        assert_eq!(info.max_length, Some(-1));
        assert_eq!(info.system_type_id, Some(231));
        assert_eq!(info.user_type_id, Some(231));
        assert_eq!(info.precision, None);
        assert_eq!(info.collation_name.as_deref(), Some("example_collation"));
        assert_eq!(snapshot.types["nvarchar"].max_length, Some(8000));
        snapshot.types.insert(
            "time".into(),
            TypeMetadata {
                system_type_id: Some(41),
                user_type_id: Some(41),
                ..Default::default()
            },
        );
        let info = snapshot
            .cast_info(&DataType::Time(Some(2), TimezoneInfo::None))
            .unwrap();
        assert_eq!(
            (info.max_length, info.precision, info.scale),
            (Some(3), Some(11), Some(2))
        );
    }
}
