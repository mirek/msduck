//! Finite TRANSLATE matching certificates from owner-retained SQL Server
//! observations. These are operation keys, not general collation weights.
use std::collections::BTreeSet;

use msduck_core::character::Family;

use crate::concat_ws::{Collation, Encoding, Function, Plan};

pub const ASCII_EVIDENCE_SHA256: &str =
    "d284c6a7c4d0ff6062492db2de91f6b8bedcd8f9620395ed10c02f0f2184c579";
pub const FINITE_EVIDENCE_SHA256: &str =
    "be7056a2fbe88e681fcec688131f3b58c3d7bc5786112709419c1dc22e4b35ee";
pub const MAX_OPERAND_UNITS: usize = 8 * 1024 * 1024;
pub const MAX_CERTIFICATE_CHARACTERS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Varchar,
    Nvarchar,
}

/// Actual catalog/wire properties are explicit inputs, never inferred from a
/// name. Unicode payloads are UTF16 even when the collation's ANSI codepage is
/// UTF8. The caller must retain the original domain through conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Properties {
    pub collation: Collation,
    pub lcid: u32,
    pub flags: u8,
    pub version: u8,
    pub sort_id: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownContext,
    ContradictoryContext,
    UnknownCharacterBoundary,
    UnknownRelationship,
    InconsistentRelationships,
    OperandLimit,
    CertificateLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    domain: Domain,
    profile: usize,
    properties: Properties,
}

// Names identify evidence records. They are not parsed for linguistic rules.
const PROFILES: &[(&str, u8, u8, u8, bool, bool, Encoding)] = &[
    (
        "SQL_Latin1_General_CP1_CI_AS",
        13,
        0,
        52,
        false,
        false,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_CI_AS",
        13,
        2,
        0,
        false,
        false,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_CS_AS",
        12,
        2,
        0,
        false,
        true,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_CI_AI",
        15,
        2,
        0,
        false,
        false,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_CS_AI",
        14,
        2,
        0,
        false,
        true,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_BIN2",
        32,
        2,
        0,
        false,
        true,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_CI_AS_SC",
        13,
        2,
        0,
        true,
        false,
        Encoding::Cp1252,
    ),
    (
        "Latin1_General_100_CI_AS_SC_UTF8",
        77,
        2,
        0,
        true,
        false,
        Encoding::Utf8,
    ),
];

impl Context {
    /// Validate before NULL shortcuts. A certificate cannot be made from a
    /// caller-provided boolean relationship graph or an unverified profile.
    pub fn new(domain: Domain, properties: Properties) -> Result<Self, Error> {
        let profile = PROFILES
            .iter()
            .position(|p| p.0.eq_ignore_ascii_case(&properties.collation.name))
            .ok_or(Error::UnknownContext)?;
        let (_, flags, version, sort_id, supplementary, case_sensitive, encoding) =
            PROFILES[profile];
        if properties.lcid != 1033
            || properties.flags != flags
            || properties.version != version
            || properties.sort_id != sort_id
            || properties.collation.supplementary != supplementary
            || properties.collation.case_sensitive != case_sensitive
            || properties.collation.encoding != encoding
        {
            return Err(Error::ContradictoryContext);
        }
        Ok(Self {
            domain,
            profile,
            properties,
        })
    }

    pub fn domain(&self) -> Domain {
        self.domain
    }

    /// Check that independently compiled result properties agree with the
    /// matching context. This never mutates metadata or admits a source domain
    /// conversion; original operands remain the binder's responsibility.
    pub fn validate_plan(&self, plan: &Plan) -> Result<(), Error> {
        if plan.function() != Function::Translate
            || !matches!(
                (self.domain, plan.declaration.family()),
                (Domain::Varchar, Family::Varchar) | (Domain::Nvarchar, Family::Nvarchar)
            )
            || !plan
                .collation
                .name()
                .is_some_and(|name| name.eq_ignore_ascii_case(&self.properties.collation.name))
            || plan.supplementary != self.properties.collation.supplementary
            || plan.encoding != self.properties.collation.encoding
            || (plan.flags & 2 != 0) != self.properties.collation.case_sensitive
        {
            return Err(Error::ContradictoryContext);
        }
        Ok(())
    }

    /// Borrowed, already SQL-converted UTF16 operands. Native byte decoding and
    /// source construction errors belong to the adapter, before this boundary.
    /// Repeated MAX values are streamed and deduplicated before pair checks.
    pub fn certificate(&self, input: &[u16], mapping: &[u16]) -> Result<Certificate, Error> {
        if input.len() > MAX_OPERAND_UNITS || mapping.len() > MAX_OPERAND_UNITS {
            return Err(Error::OperandLimit);
        }
        let mut characters = BTreeSet::new();
        for value in [input, mapping] {
            let mut offset = 0;
            while offset < value.len() {
                let (character, size) = self.character(&value[offset..])?;
                if !characters.contains(&character)
                    && characters.len() == MAX_CERTIFICATE_CHARACTERS
                {
                    return Err(Error::CertificateLimit);
                }
                characters.insert(character);
                offset += size;
            }
        }
        let characters: Vec<u32> = characters.into_iter().collect();
        let mut keys = Vec::with_capacity(characters.len());
        for (i, &character) in characters.iter().enumerate() {
            let mut key = i as u16;
            for (j, &other) in characters.iter().enumerate() {
                let matches = self
                    .relationship(character, other)
                    .ok_or(Error::UnknownRelationship)?;
                if i == j && !matches {
                    return Err(Error::InconsistentRelationships);
                }
                if matches && j < i {
                    key = keys[j];
                    break;
                }
            }
            keys.push(key);
        }
        // Check every ordered pair, including relationships after the first
        // equivalent character. Missing negatives cannot become distinct keys.
        for (i, &a) in characters.iter().enumerate() {
            for (j, &b) in characters.iter().enumerate() {
                let matches = self.relationship(a, b).ok_or(Error::UnknownRelationship)?;
                if matches != (keys[i] == keys[j]) {
                    return Err(Error::InconsistentRelationships);
                }
            }
        }
        Ok(Certificate {
            context: self.clone(),
            characters,
            keys,
        })
    }

    fn character(&self, units: &[u16]) -> Result<(u32, usize), Error> {
        let first = *units.first().ok_or(Error::UnknownCharacterBoundary)?;
        if (0xd800..=0xdfff).contains(&first) {
            if self.properties.collation.supplementary
                && (0xd800..=0xdbff).contains(&first)
                && let Some(&low) = units.get(1)
                && (0xdc00..=0xdfff).contains(&low)
            {
                return Ok((
                    0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(low) - 0xdc00,
                    2,
                ));
            }
            return Err(Error::UnknownCharacterBoundary);
        }
        Ok((u32::from(first), 1))
    }

    fn relationship(&self, a: u32, b: u32) -> Option<bool> {
        if a < 128 && b < 128 {
            // Two measured ASCII partitions; every pair is replayed against
            // both actual sentinel controls in every retained profile/run.
            let classes = match self.profile {
                0 | 1 | 3 | 6 | 7 => &ASCII_PAIRED,
                _ => &ASCII_SINGLETONS,
            };
            return Some(classes[a as usize] == classes[b as usize]);
        }
        let bit = 1u16 << (self.profile * 2 + usize::from(self.domain == Domain::Nvarchar));
        let index = OBSERVED
            .binary_search_by_key(&(a, b), |p| (p.0, p.1))
            .ok()?;
        let (_, _, known, equal) = OBSERVED[index];
        (known & bit != 0).then_some(equal & bit != 0)
    }
}

/// Immutable operation-local keys. Lookup allocates nothing and never extends
/// its alphabet. Keys from different certificates must never be compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    context: Context,
    characters: Vec<u32>,
    keys: Vec<u16>,
}
impl Certificate {
    pub fn key(&self, character: &[u16]) -> Option<u16> {
        let (scalar, size) = self.context.character(character).ok()?;
        if size != character.len() {
            return None;
        }
        let index = self.characters.binary_search(&scalar).ok()?;
        Some(self.keys[index])
    }
    pub fn character_count(&self) -> usize {
        self.characters.len()
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
}

const ASCII_SINGLETONS: [u8; 128] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
    98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
    117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
];
const ASCII_PAIRED: [u8; 128] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 65,
    66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89,
    90, 123, 124, 125, 126, 127,
];
// Stable single-scalar relationships from all four #841 runs.
const OBSERVED: &[(u32, u32, u16, u16)] = &[
    (0x20, 0xa0, 0xffff, 0x0000),
    (0x20, 0x2000, 0xeaaa, 0x0000),
    (0x20, 0x200b, 0xeaaa, 0x0000),
    (0x20, 0xfeff, 0xeaaa, 0x0280),
    (0x27, 0x2019, 0xffff, 0x0000),
    (0x2d, 0xad, 0xffff, 0x0000),
    (0x2d, 0x2010, 0xeaaa, 0x0000),
    (0x2e, 0xff0e, 0xeaaa, 0xe2aa),
    (0x30, 0xff10, 0xeaaa, 0xe2aa),
    (0x3f, 0xb4, 0x1555, 0x0000),
    (0x41, 0xff21, 0xeaaa, 0xe2aa),
    (0x49, 0x131, 0xeaaa, 0x0080),
    (0x61, 0xe0, 0xffff, 0x03c0),
    (0x61, 0xe1, 0xffff, 0x03c0),
    (0x61, 0xe4, 0xffff, 0x03c0),
    (0x61, 0xe5, 0xffff, 0x03c0),
    (0x61, 0xe6, 0xffff, 0x0000),
    (0x63, 0xe7, 0xffff, 0x03c0),
    (0x65, 0xe9, 0xffff, 0x03c0),
    (0x69, 0x130, 0xeaaa, 0x0080),
    (0x6e, 0xf1, 0xffff, 0x03c0),
    (0x6f, 0xf6, 0xffff, 0x03c0),
    (0x6f, 0x153, 0xffff, 0x0000),
    (0x73, 0xdf, 0xffff, 0x0000),
    (0x75, 0xfc, 0xffff, 0x03c0),
    (0x79, 0xff, 0xffff, 0x03c0),
    (0x7f, 0x81, 0xffff, 0x0000),
    (0x81, 0x7f, 0xffff, 0x0000),
    (0x81, 0x81, 0xffff, 0xffff),
    (0xa0, 0x20, 0xffff, 0x0000),
    (0xa0, 0xa0, 0xffff, 0xffff),
    (0xad, 0x2d, 0xffff, 0x0000),
    (0xad, 0xad, 0xffff, 0xffff),
    (0xb4, 0x3f, 0x1555, 0x0000),
    (0xb4, 0xb4, 0x1555, 0x1555),
    (0xb5, 0xb5, 0xffff, 0xffff),
    (0xb5, 0x3bc, 0xeaaa, 0x0000),
    (0xc6, 0xc6, 0xffff, 0xffff),
    (0xc6, 0xe6, 0xffff, 0xf0cf),
    (0xc9, 0xc9, 0xffff, 0xffff),
    (0xc9, 0xe9, 0xffff, 0xf0cf),
    (0xdf, 0x73, 0xffff, 0x0000),
    (0xdf, 0xdf, 0xffff, 0xffff),
    (0xe0, 0x61, 0xffff, 0x03c0),
    (0xe0, 0xe0, 0xffff, 0xffff),
    (0xe1, 0x61, 0xffff, 0x03c0),
    (0xe1, 0xe1, 0xffff, 0xffff),
    (0xe4, 0x61, 0xffff, 0x03c0),
    (0xe4, 0xe4, 0xffff, 0xffff),
    (0xe5, 0x61, 0xffff, 0x03c0),
    (0xe5, 0xe5, 0xffff, 0xffff),
    (0xe6, 0x61, 0xffff, 0x0000),
    (0xe6, 0xc6, 0xffff, 0xf0cf),
    (0xe6, 0xe6, 0xffff, 0xffff),
    (0xe7, 0x63, 0xffff, 0x03c0),
    (0xe7, 0xe7, 0xffff, 0xffff),
    (0xe9, 0x65, 0xffff, 0x03c0),
    (0xe9, 0xc9, 0xffff, 0xf0cf),
    (0xe9, 0xe9, 0xffff, 0xffff),
    (0xf1, 0x6e, 0xffff, 0x03c0),
    (0xf1, 0xf1, 0xffff, 0xffff),
    (0xf6, 0x6f, 0xffff, 0x03c0),
    (0xf6, 0xf6, 0xffff, 0xffff),
    (0xfc, 0x75, 0xffff, 0x03c0),
    (0xfc, 0xfc, 0xffff, 0xffff),
    (0xff, 0x79, 0xffff, 0x03c0),
    (0xff, 0xff, 0xffff, 0xffff),
    (0x130, 0x69, 0xeaaa, 0x0080),
    (0x130, 0x130, 0xeaaa, 0xeaaa),
    (0x131, 0x49, 0xeaaa, 0x0080),
    (0x131, 0x131, 0xeaaa, 0xeaaa),
    (0x152, 0x152, 0xffff, 0xffff),
    (0x152, 0x153, 0xffff, 0xf0ce),
    (0x153, 0x6f, 0xffff, 0x0000),
    (0x153, 0x152, 0xffff, 0xf0ce),
    (0x153, 0x153, 0xffff, 0xffff),
    (0x301, 0x301, 0xeaaa, 0xeaaa),
    (0x301, 0x341, 0xeaaa, 0xe2aa),
    (0x341, 0x301, 0xeaaa, 0xe2aa),
    (0x341, 0x341, 0xeaaa, 0xeaaa),
    (0x3a3, 0x3a3, 0xeaaa, 0xeaaa),
    (0x3a3, 0x3c3, 0xeaaa, 0xe08a),
    (0x3bc, 0xb5, 0xeaaa, 0x0000),
    (0x3bc, 0x3bc, 0xeaaa, 0xeaaa),
    (0x3c2, 0x3c2, 0xeaaa, 0xeaaa),
    (0x3c2, 0x3c3, 0xeaaa, 0xe08a),
    (0x3c3, 0x3a3, 0xeaaa, 0xe08a),
    (0x3c3, 0x3c2, 0xeaaa, 0xe08a),
    (0x3c3, 0x3c3, 0xeaaa, 0xeaaa),
    (0x2000, 0x20, 0xeaaa, 0x0000),
    (0x2000, 0x2000, 0xeaaa, 0xeaaa),
    (0x200b, 0x20, 0xeaaa, 0x0000),
    (0x200b, 0x200b, 0xeaaa, 0xeaaa),
    (0x2010, 0x2d, 0xeaaa, 0x0000),
    (0x2010, 0x2010, 0xeaaa, 0xeaaa),
    (0x2019, 0x27, 0xffff, 0x0000),
    (0x2019, 0x2019, 0xffff, 0xffff),
    (0xfeff, 0x20, 0xeaaa, 0x0280),
    (0xfeff, 0xfeff, 0xeaaa, 0xeaaa),
    (0xff0e, 0x2e, 0xeaaa, 0xe2aa),
    (0xff0e, 0xff0e, 0xeaaa, 0xeaaa),
    (0xff10, 0x30, 0xeaaa, 0xe2aa),
    (0xff10, 0xff10, 0xeaaa, 0xeaaa),
    (0xff21, 0x41, 0xeaaa, 0xe2aa),
    (0xff21, 0xff21, 0xeaaa, 0xeaaa),
    (0x1f600, 0x1f600, 0xe000, 0xe000),
    (0x1f600, 0x1f601, 0xe000, 0x0000),
    (0x1f601, 0x1f600, 0xe000, 0x0000),
    (0x1f601, 0x1f601, 0xe000, 0xe000),
];
