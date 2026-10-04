//! Native CP1251/CP1252 and valid UTF8 projections from retained SQL Server probes.
//! Complete projections are separate from the BulkLoad capacity submodule.
//! SQL diagnostics and wire collation admission belong to root adapters.
use crate::ansi_bytes::{AnsiBytes, AnsiView, ByteError, EncodingIdentity};
use std::fmt;

pub mod capacity;
pub mod stored_utf8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionTarget {
    Native(EncodingIdentity),
    SqlUtf16,
}

/// Active payload limits. UTF16 output is counted in bytes, not code units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionLimits {
    pub input_bytes: usize,
    pub output_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Input,
    Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    UnsupportedSource(EncodingIdentity),
    UnsupportedTarget(EncodingIdentity),
    SourceEncodingMismatch {
        declared: EncodingIdentity,
        actual: EncodingIdentity,
    },
    /// Outside the valid UTF8 scalar domain; this is not a SQL diagnostic.
    InvalidUtf8 {
        valid_up_to: usize,
        error_len: Option<usize>,
    },
    Limit {
        resource: Resource,
        requested: usize,
        maximum: usize,
    },
    LengthOverflow,
    AllocationFailed {
        requested_bytes: usize,
    },
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSource(source) => {
                write!(f, "unsupported ANSI projection source {source:?}")
            }
            Self::UnsupportedTarget(target) => {
                write!(f, "unsupported ANSI projection target {target:?}")
            }
            Self::SourceEncodingMismatch { declared, actual } => write!(
                f,
                "ANSI source {actual:?} differs from declaration {declared:?}"
            ),
            Self::InvalidUtf8 {
                valid_up_to,
                error_len,
            } => write!(
                f,
                "invalid UTF8 projection bytes at {valid_up_to} (error length {error_len:?})"
            ),
            Self::Limit {
                resource,
                requested,
                maximum,
            } => write!(
                f,
                "ANSI projection {resource:?} payload {requested} exceeds limit {maximum}"
            ),
            Self::LengthOverflow => f.write_str("ANSI projection byte count overflow"),
            Self::AllocationFailed { requested_bytes } => write!(
                f,
                "cannot allocate ANSI projection payload of {requested_bytes} bytes"
            ),
        }
    }
}
impl std::error::Error for ProjectionError {}

#[derive(PartialEq, Eq)]
pub enum ProjectedValue {
    Native(AnsiBytes),
    SqlUtf16(Vec<u16>),
}

impl fmt::Debug for ProjectedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(value) => f.debug_tuple("Native").field(value).finish(),
            Self::SqlUtf16(units) => f
                .debug_struct("SqlUtf16")
                .field("unit_len", &units.len())
                .field("prefix", &&units[..units.len().min(32)])
                .finish_non_exhaustive(),
        }
    }
}

fn check_limit(
    resource: Resource,
    requested: usize,
    maximum: usize,
) -> Result<(), ProjectionError> {
    if requested > maximum {
        return Err(ProjectionError::Limit {
            resource,
            requested,
            maximum,
        });
    }
    Ok(())
}

fn allocate<T>(elements: usize, requested_bytes: usize) -> Result<Vec<T>, ProjectionError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(elements)
        .map_err(|_| ProjectionError::AllocationFailed { requested_bytes })?;
    Ok(output)
}

/// Project CP1251/CP1252 or valid UTF8 source domains, with an explicit declaration
/// even for NULL. Unsupported plans fail before inspecting the nullable value.
/// A carrier must agree with its declaration. The input is never modified.
/// Resource bounds are checked before reserving/copying the output. These are
/// complete projections: they neither truncate nor pad to a SQL column width.
pub fn project(
    source_encoding: EncodingIdentity,
    value: Option<AnsiView<'_>>,
    target: ProjectionTarget,
    limits: ProjectionLimits,
) -> Result<Option<ProjectedValue>, ProjectionError> {
    if source_encoding == EncodingIdentity::Utf8 {
        return project_utf8(value, target, limits);
    }
    let characters = match source_encoding {
        EncodingIdentity::Cp1251 => &CP1251_TO_SQL_CHAR,
        EncodingIdentity::Cp1252 => &CP1252_TO_SQL_CHAR,
        _ => return Err(ProjectionError::UnsupportedSource(source_encoding)),
    };
    if let ProjectionTarget::Native(encoding @ EncodingIdentity::Opaque(_)) = target {
        return Err(ProjectionError::UnsupportedTarget(encoding));
    }
    let Some(value) = value else { return Ok(None) };
    if value.encoding() != source_encoding {
        return Err(ProjectionError::SourceEncodingMismatch {
            declared: source_encoding,
            actual: value.encoding(),
        });
    }
    let input = value.bytes();
    check_limit(Resource::Input, input.len(), limits.input_bytes)?;
    let output_bytes = match target {
        ProjectionTarget::SqlUtf16 => input
            .len()
            .checked_mul(2)
            .ok_or(ProjectionError::LengthOverflow)?,
        ProjectionTarget::Native(EncodingIdentity::Utf8) => {
            input.iter().try_fold(0usize, |size, &byte| {
                size.checked_add(characters[usize::from(byte)].len_utf8())
                    .ok_or(ProjectionError::LengthOverflow)
            })?
        }
        ProjectionTarget::Native(_) => input.len(),
    };
    check_limit(Resource::Output, output_bytes, limits.output_bytes)?;
    let projected = match target {
        ProjectionTarget::SqlUtf16 => {
            let mut units = allocate(input.len(), output_bytes)?;
            units.extend(
                input
                    .iter()
                    .map(|&byte| characters[usize::from(byte)] as u16),
            );
            ProjectedValue::SqlUtf16(units)
        }
        ProjectionTarget::Native(encoding) => {
            let mut bytes = allocate(output_bytes, output_bytes)?;
            match encoding {
                encoding if encoding == source_encoding => bytes.extend_from_slice(input),
                EncodingIdentity::Cp1251 => bytes.extend(
                    input
                        .iter()
                        .map(|&byte| CP1252_TO_CP1251[usize::from(byte)]),
                ),
                EncodingIdentity::Cp1252 => bytes.extend(
                    input
                        .iter()
                        .map(|&byte| CP1251_TO_CP1252[usize::from(byte)]),
                ),
                EncodingIdentity::Utf8 => {
                    let mut encoded = [0; 4];
                    for &byte in input {
                        bytes.extend_from_slice(
                            characters[usize::from(byte)]
                                .encode_utf8(&mut encoded)
                                .as_bytes(),
                        );
                    }
                }
                EncodingIdentity::Opaque(_) => unreachable!("opaque target rejected before NULL"),
            }
            let owned =
                AnsiBytes::from_vec(encoding, bytes, limits.output_bytes).map_err(|error| {
                    match error {
                        ByteError::LengthOverflow => ProjectionError::LengthOverflow,
                        ByteError::Limit { requested, maximum } => ProjectionError::Limit {
                            resource: Resource::Output,
                            requested,
                            maximum,
                        },
                    }
                })?;
            ProjectedValue::Native(owned)
        }
    };
    Ok(Some(projected))
}

/// A strict scalar domain, separate from SQL Server's context-specific malformed
/// byte grammar and BulkLoad declaration/wire admission. No lossy repair.
fn project_utf8(
    value: Option<AnsiView<'_>>,
    target: ProjectionTarget,
    limits: ProjectionLimits,
) -> Result<Option<ProjectedValue>, ProjectionError> {
    if let ProjectionTarget::Native(encoding @ EncodingIdentity::Opaque(_)) = target {
        return Err(ProjectionError::UnsupportedTarget(encoding));
    }
    let Some(value) = value else { return Ok(None) };
    if value.encoding() != EncodingIdentity::Utf8 {
        return Err(ProjectionError::SourceEncodingMismatch {
            declared: EncodingIdentity::Utf8,
            actual: value.encoding(),
        });
    }
    let input = value.bytes();
    check_limit(Resource::Input, input.len(), limits.input_bytes)?;
    let text = std::str::from_utf8(input).map_err(|error| ProjectionError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
        error_len: error.error_len(),
    })?;
    let projected = match target {
        ProjectionTarget::Native(EncodingIdentity::Utf8) => {
            check_limit(Resource::Output, input.len(), limits.output_bytes)?;
            let mut bytes = allocate(input.len(), input.len())?;
            bytes.extend_from_slice(input);
            ProjectedValue::Native(
                AnsiBytes::from_vec(EncodingIdentity::Utf8, bytes, limits.output_bytes).map_err(
                    |error| match error {
                        ByteError::LengthOverflow => ProjectionError::LengthOverflow,
                        ByteError::Limit { requested, maximum } => ProjectionError::Limit {
                            resource: Resource::Output,
                            requested,
                            maximum,
                        },
                    },
                )?,
            )
        }
        ProjectionTarget::SqlUtf16 => {
            let unit_count = text.chars().try_fold(0usize, |count, scalar| {
                count
                    .checked_add(scalar.len_utf16())
                    .ok_or(ProjectionError::LengthOverflow)
            })?;
            let output_bytes = unit_count
                .checked_mul(2)
                .ok_or(ProjectionError::LengthOverflow)?;
            check_limit(Resource::Output, output_bytes, limits.output_bytes)?;
            let mut units = allocate(unit_count, output_bytes)?;
            units.extend(text.encode_utf16());
            ProjectedValue::SqlUtf16(units)
        }
        ProjectionTarget::Native(
            encoding @ (EncodingIdentity::Cp1251 | EncodingIdentity::Cp1252),
        ) => {
            let output_bytes = text.chars().try_fold(0usize, |count, scalar| {
                count
                    .checked_add(scalar.len_utf16())
                    .ok_or(ProjectionError::LengthOverflow)
            })?;
            check_limit(Resource::Output, output_bytes, limits.output_bytes)?;
            let mut bytes = allocate(output_bytes, output_bytes)?;
            bytes.extend(
                text.encode_utf16()
                    .map(|unit| capacity::codepage_unit(encoding, unit)),
            );
            ProjectedValue::Native(
                AnsiBytes::from_vec(encoding, bytes, limits.output_bytes).map_err(|error| {
                    match error {
                        ByteError::LengthOverflow => ProjectionError::LengthOverflow,
                        ByteError::Limit { requested, maximum } => ProjectionError::Limit {
                            resource: Resource::Output,
                            requested,
                            maximum,
                        },
                    }
                })?,
            )
        }
        ProjectionTarget::Native(EncodingIdentity::Opaque(_)) => {
            unreachable!("opaque target rejected before NULL")
        }
    };
    Ok(Some(projected))
}

// Exact 256-cell SQL Server projections from reference/bulk-character-conversion.json.
// Retained fixture SHA256 f55527a2b0969d7104510d9b76a3303c82b28eac05d4ad11bc2acaf472caab27.
const CP1251_TO_SQL_CHAR: [char; 256] = [
    '\u{0}', '\u{1}', '\u{2}', '\u{3}', '\u{4}', '\u{5}', '\u{6}', '\u{7}', '\u{8}', '\u{9}',
    '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{e}', '\u{f}', '\u{10}', '\u{11}', '\u{12}', '\u{13}',
    '\u{14}', '\u{15}', '\u{16}', '\u{17}', '\u{18}', '\u{19}', '\u{1a}', '\u{1b}', '\u{1c}',
    '\u{1d}', '\u{1e}', '\u{1f}', '\u{20}', '\u{21}', '\u{22}', '\u{23}', '\u{24}', '\u{25}',
    '\u{26}', '\u{27}', '\u{28}', '\u{29}', '\u{2a}', '\u{2b}', '\u{2c}', '\u{2d}', '\u{2e}',
    '\u{2f}', '\u{30}', '\u{31}', '\u{32}', '\u{33}', '\u{34}', '\u{35}', '\u{36}', '\u{37}',
    '\u{38}', '\u{39}', '\u{3a}', '\u{3b}', '\u{3c}', '\u{3d}', '\u{3e}', '\u{3f}', '\u{40}',
    '\u{41}', '\u{42}', '\u{43}', '\u{44}', '\u{45}', '\u{46}', '\u{47}', '\u{48}', '\u{49}',
    '\u{4a}', '\u{4b}', '\u{4c}', '\u{4d}', '\u{4e}', '\u{4f}', '\u{50}', '\u{51}', '\u{52}',
    '\u{53}', '\u{54}', '\u{55}', '\u{56}', '\u{57}', '\u{58}', '\u{59}', '\u{5a}', '\u{5b}',
    '\u{5c}', '\u{5d}', '\u{5e}', '\u{5f}', '\u{60}', '\u{61}', '\u{62}', '\u{63}', '\u{64}',
    '\u{65}', '\u{66}', '\u{67}', '\u{68}', '\u{69}', '\u{6a}', '\u{6b}', '\u{6c}', '\u{6d}',
    '\u{6e}', '\u{6f}', '\u{70}', '\u{71}', '\u{72}', '\u{73}', '\u{74}', '\u{75}', '\u{76}',
    '\u{77}', '\u{78}', '\u{79}', '\u{7a}', '\u{7b}', '\u{7c}', '\u{7d}', '\u{7e}', '\u{7f}',
    '\u{402}', '\u{403}', '\u{201a}', '\u{453}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20ac}', '\u{2030}', '\u{409}', '\u{2039}', '\u{40a}', '\u{40c}', '\u{40b}', '\u{40f}',
    '\u{452}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{98}', '\u{2122}', '\u{459}', '\u{203a}', '\u{45a}', '\u{45c}', '\u{45b}', '\u{45f}',
    '\u{a0}', '\u{40e}', '\u{45e}', '\u{408}', '\u{a4}', '\u{490}', '\u{a6}', '\u{a7}', '\u{401}',
    '\u{a9}', '\u{404}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{407}', '\u{b0}', '\u{b1}',
    '\u{406}', '\u{456}', '\u{491}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{451}', '\u{2116}',
    '\u{454}', '\u{bb}', '\u{458}', '\u{405}', '\u{455}', '\u{457}', '\u{410}', '\u{411}',
    '\u{412}', '\u{413}', '\u{414}', '\u{415}', '\u{416}', '\u{417}', '\u{418}', '\u{419}',
    '\u{41a}', '\u{41b}', '\u{41c}', '\u{41d}', '\u{41e}', '\u{41f}', '\u{420}', '\u{421}',
    '\u{422}', '\u{423}', '\u{424}', '\u{425}', '\u{426}', '\u{427}', '\u{428}', '\u{429}',
    '\u{42a}', '\u{42b}', '\u{42c}', '\u{42d}', '\u{42e}', '\u{42f}', '\u{430}', '\u{431}',
    '\u{432}', '\u{433}', '\u{434}', '\u{435}', '\u{436}', '\u{437}', '\u{438}', '\u{439}',
    '\u{43a}', '\u{43b}', '\u{43c}', '\u{43d}', '\u{43e}', '\u{43f}', '\u{440}', '\u{441}',
    '\u{442}', '\u{443}', '\u{444}', '\u{445}', '\u{446}', '\u{447}', '\u{448}', '\u{449}',
    '\u{44a}', '\u{44b}', '\u{44c}', '\u{44d}', '\u{44e}', '\u{44f}',
];
const CP1251_TO_CP1252: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f,
    0x3f, 0x3f, 0x82, 0x3f, 0x84, 0x85, 0x86, 0x87, 0x80, 0x89, 0x3f, 0x8b, 0x3f, 0x3f, 0x3f, 0x3f,
    0x3f, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x3f, 0x99, 0x3f, 0x9b, 0x3f, 0x3f, 0x3f, 0x3f,
    0xa0, 0x3f, 0x3f, 0x3f, 0xa4, 0x3f, 0xa6, 0xa7, 0x3f, 0xa9, 0x3f, 0xab, 0xac, 0xad, 0xae, 0x3f,
    0xb0, 0xb1, 0x3f, 0x3f, 0x3f, 0xb5, 0xb6, 0xb7, 0x3f, 0x3f, 0x3f, 0xbb, 0x3f, 0x3f, 0x3f, 0x3f,
    0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f,
    0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f,
    0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f,
    0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f, 0x3f,
];

// Both captured source domains have exactly one BMP unit per source byte.
const _: () = {
    let mut i = 0;
    while i < CP1251_TO_SQL_CHAR.len() {
        assert!(CP1251_TO_SQL_CHAR[i] as u32 <= u16::MAX as u32);
        assert!(CP1252_TO_SQL_CHAR[i] as u32 <= u16::MAX as u32);
        i += 1;
    }
};

// Exact tables from all four unchanged reference906 runs, SHA256 d9d8baa3...
const CP1252_TO_SQL_CHAR: [char; 256] = [
    '\u{0}', '\u{1}', '\u{2}', '\u{3}', '\u{4}', '\u{5}', '\u{6}', '\u{7}', '\u{8}', '\u{9}',
    '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{e}', '\u{f}', '\u{10}', '\u{11}', '\u{12}', '\u{13}',
    '\u{14}', '\u{15}', '\u{16}', '\u{17}', '\u{18}', '\u{19}', '\u{1a}', '\u{1b}', '\u{1c}',
    '\u{1d}', '\u{1e}', '\u{1f}', '\u{20}', '\u{21}', '\u{22}', '\u{23}', '\u{24}', '\u{25}',
    '\u{26}', '\u{27}', '\u{28}', '\u{29}', '\u{2a}', '\u{2b}', '\u{2c}', '\u{2d}', '\u{2e}',
    '\u{2f}', '\u{30}', '\u{31}', '\u{32}', '\u{33}', '\u{34}', '\u{35}', '\u{36}', '\u{37}',
    '\u{38}', '\u{39}', '\u{3a}', '\u{3b}', '\u{3c}', '\u{3d}', '\u{3e}', '\u{3f}', '\u{40}',
    '\u{41}', '\u{42}', '\u{43}', '\u{44}', '\u{45}', '\u{46}', '\u{47}', '\u{48}', '\u{49}',
    '\u{4a}', '\u{4b}', '\u{4c}', '\u{4d}', '\u{4e}', '\u{4f}', '\u{50}', '\u{51}', '\u{52}',
    '\u{53}', '\u{54}', '\u{55}', '\u{56}', '\u{57}', '\u{58}', '\u{59}', '\u{5a}', '\u{5b}',
    '\u{5c}', '\u{5d}', '\u{5e}', '\u{5f}', '\u{60}', '\u{61}', '\u{62}', '\u{63}', '\u{64}',
    '\u{65}', '\u{66}', '\u{67}', '\u{68}', '\u{69}', '\u{6a}', '\u{6b}', '\u{6c}', '\u{6d}',
    '\u{6e}', '\u{6f}', '\u{70}', '\u{71}', '\u{72}', '\u{73}', '\u{74}', '\u{75}', '\u{76}',
    '\u{77}', '\u{78}', '\u{79}', '\u{7a}', '\u{7b}', '\u{7c}', '\u{7d}', '\u{7e}', '\u{7f}',
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
    '\u{a0}', '\u{a1}', '\u{a2}', '\u{a3}', '\u{a4}', '\u{a5}', '\u{a6}', '\u{a7}', '\u{a8}',
    '\u{a9}', '\u{aa}', '\u{ab}', '\u{ac}', '\u{ad}', '\u{ae}', '\u{af}', '\u{b0}', '\u{b1}',
    '\u{b2}', '\u{b3}', '\u{b4}', '\u{b5}', '\u{b6}', '\u{b7}', '\u{b8}', '\u{b9}', '\u{ba}',
    '\u{bb}', '\u{bc}', '\u{bd}', '\u{be}', '\u{bf}', '\u{c0}', '\u{c1}', '\u{c2}', '\u{c3}',
    '\u{c4}', '\u{c5}', '\u{c6}', '\u{c7}', '\u{c8}', '\u{c9}', '\u{ca}', '\u{cb}', '\u{cc}',
    '\u{cd}', '\u{ce}', '\u{cf}', '\u{d0}', '\u{d1}', '\u{d2}', '\u{d3}', '\u{d4}', '\u{d5}',
    '\u{d6}', '\u{d7}', '\u{d8}', '\u{d9}', '\u{da}', '\u{db}', '\u{dc}', '\u{dd}', '\u{de}',
    '\u{df}', '\u{e0}', '\u{e1}', '\u{e2}', '\u{e3}', '\u{e4}', '\u{e5}', '\u{e6}', '\u{e7}',
    '\u{e8}', '\u{e9}', '\u{ea}', '\u{eb}', '\u{ec}', '\u{ed}', '\u{ee}', '\u{ef}', '\u{f0}',
    '\u{f1}', '\u{f2}', '\u{f3}', '\u{f4}', '\u{f5}', '\u{f6}', '\u{f7}', '\u{f8}', '\u{f9}',
    '\u{fa}', '\u{fb}', '\u{fc}', '\u{fd}', '\u{fe}', '\u{ff}',
];
const CP1252_TO_CP1251: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f,
    0x88, 0x3f, 0x82, 0x3f, 0x84, 0x85, 0x86, 0x87, 0x3f, 0x89, 0x53, 0x8b, 0x3f, 0x3f, 0x5a, 0x3f,
    0x3f, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x3f, 0x99, 0x73, 0x9b, 0x3f, 0x3f, 0x7a, 0x59,
    0xa0, 0x3f, 0x3f, 0x3f, 0xa4, 0x3f, 0xa6, 0xa7, 0x3f, 0xa9, 0x3f, 0xab, 0xac, 0xad, 0xae, 0x3f,
    0xb0, 0xb1, 0x3f, 0x3f, 0x3f, 0xb5, 0xb6, 0xb7, 0x3f, 0x3f, 0x3f, 0xbb, 0x3f, 0x3f, 0x3f, 0x3f,
    0x41, 0x41, 0x41, 0x41, 0x41, 0x41, 0x3f, 0x43, 0x45, 0x45, 0x45, 0x45, 0x49, 0x49, 0x49, 0x49,
    0x3f, 0x4e, 0x4f, 0x4f, 0x4f, 0x4f, 0x4f, 0x3f, 0x4f, 0x55, 0x55, 0x55, 0x55, 0x59, 0x3f, 0x3f,
    0x61, 0x61, 0x61, 0x61, 0x61, 0x61, 0x3f, 0x63, 0x65, 0x65, 0x65, 0x65, 0x69, 0x69, 0x69, 0x69,
    0x3f, 0x6e, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x3f, 0x6f, 0x75, 0x75, 0x75, 0x75, 0x79, 0x3f, 0x79,
];
