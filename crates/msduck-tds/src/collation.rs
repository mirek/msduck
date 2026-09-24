//! TDS collation descriptors, independent of SQL comparison implementation.
use anyhow::{Result, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Collation([u8; 5]);

impl Default for Collation {
    fn default() -> Self {
        Self(crate::COLLATION)
    }
}

impl Collation {
    pub fn new(lcid: u32, flags: u8, version: u8, sort_id: u8) -> Result<Self> {
        ensure!(lcid <= 0xfffff, "collation LCID exceeds 20 bits");
        ensure!(version <= 15, "collation version exceeds 4 bits");
        let info = lcid | (u32::from(flags) << 20) | (u32::from(version) << 28);
        let [a, b, c, d] = info.to_le_bytes();
        Ok(Self([a, b, c, d, sort_id]))
    }

    pub const fn from_bytes(bytes: [u8; 5]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 5] {
        self.0
    }

    /// Only mappings captured from SQL Server are recognized. Unknown names
    /// must not be inferred from suffixes: locale, version and code page matter.
    pub fn for_name(name: &str) -> Option<Self> {
        let (flags, version, sort_id) = match name.to_ascii_lowercase().as_str() {
            "sql_latin1_general_cp1_ci_as" => (13, 0, 52),
            "latin1_general_100_ci_as" => (13, 2, 0),
            "latin1_general_100_cs_as" => (12, 2, 0),
            "latin1_general_100_ci_ai" => (15, 2, 0),
            "latin1_general_100_cs_ai" => (14, 2, 0),
            "latin1_general_100_bin2" => (32, 2, 0),
            _ => return None,
        };
        Self::new(1033, flags, version, sort_id).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_bounds_preserve_all_wire_bits() {
        assert_eq!(
            Collation::new(1033, 13, 0, 52).unwrap(),
            Collation::default()
        );
        assert_eq!(
            Collation::new(0xfffff, 255, 15, 255).unwrap().bytes(),
            [255; 5]
        );
        assert!(Collation::new(0x100000, 0, 0, 0).is_err());
        assert!(Collation::new(0, 0, 16, 0).is_err());
        assert_eq!(
            Collation::from_bytes([1, 2, 3, 4, 5]).bytes(),
            [1, 2, 3, 4, 5]
        );
        for name in [
            "unknown",
            "Japanese_CI_AS",
            "Latin1_General_100_CI_AS_SC_UTF8",
        ] {
            assert!(Collation::for_name(name).is_none());
        }
    }

    #[test]
    fn character_metadata_matches_live_collation_descriptors() {
        use crate::{Column, Type, metadata};
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../../../reference/collation-wire.json")).unwrap();
        let cases = reference["results"].as_array().unwrap();
        assert_eq!(cases.len(), 6);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let collation = Collation::for_name(name).unwrap();
            assert_eq!(Collation::for_name(&name.to_uppercase()), Some(collation));
            for column in case["reference"]["sets"][0]["columns"].as_array().unwrap() {
                let expected = &column["collation"];
                assert_eq!(
                    collation,
                    Collation::new(
                        expected["lcid"].as_u64().unwrap() as u32,
                        expected["flags"].as_u64().unwrap() as u8,
                        expected["version"].as_u64().unwrap() as u8,
                        expected["sortId"].as_u64().unwrap() as u8,
                    )
                    .unwrap(),
                    "{name}"
                );
            }
            for kind in [
                Type::Text,
                Type::Nvarchar(4),
                Type::Nchar(4),
                Type::Varchar(4),
                Type::Varchar(u16::MAX),
                Type::Char(4),
            ] {
                let mut out = vec![];
                metadata(
                    &mut out,
                    &[Column {
                        name: "s".into(),
                        kind,
                        properties: Default::default(),
                        collation: Some(collation),
                    }],
                )
                .unwrap();
                assert_eq!(&out[12..17], &collation.bytes(), "{name}");
            }
        }
    }
}
