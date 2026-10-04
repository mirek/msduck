//! BulkLoad capacity after source declaration/wire admission, before storage.
//! This is not CAST/assignment policy or a whole-load transaction implementation.
use super::{
    CP1251_TO_CP1252, CP1251_TO_SQL_CHAR, CP1252_TO_CP1251, CP1252_TO_SQL_CHAR, ProjectedValue,
    ProjectionError, ProjectionLimits, ProjectionTarget, Resource, allocate, check_limit,
};
use crate::ansi_bytes::{AnsiBytes, AnsiView, ByteError, EncodingIdentity};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Variable,
    Fixed,
}

/// Explicit source declaration form after root wire/declaration admission.
/// This fact must not be inferred from a row's bytes or current length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceForm {
    Bounded,
    Max,
}

/// Native targets count bytes; SQL UTF16 targets count two-byte units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capacity {
    Bounded(usize),
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityError {
    Projection(ProjectionError),
    InvalidWidth {
        requested: usize,
        maximum: usize,
    },
    FixedMax,
    Utf8StreamInput,
    Utf8BcpInput,
    Utf8Boundary,
    Truncation {
        source_bytes: usize,
        target_capacity: usize,
    },
}

impl From<ProjectionError> for CapacityError {
    fn from(error: ProjectionError) -> Self {
        Self::Projection(error)
    }
}
impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Projection(error) => error.fmt(f),
            Self::InvalidWidth { requested, maximum } => {
                write!(f, "ANSI capacity {requested} is outside 1..={maximum}")
            }
            Self::FixedMax => f.write_str("fixed character capacity cannot be MAX"),
            Self::Utf8StreamInput => f.write_str("invalid UTF8 BulkLoad stream input (7339/1/16)"),
            Self::Utf8BcpInput => {
                f.write_str("incomplete UTF8 MAX native BulkLoad input (4896/7/17)")
            }
            Self::Utf8Boundary => {
                f.write_str("invalid UTF8 BulkLoad conversion boundary (9833/2/16)")
            }
            Self::Truncation {
                source_bytes,
                target_capacity,
            } => write!(
                f,
                "ANSI source payload {source_bytes} exceeds SQL capacity {target_capacity}"
            ),
        }
    }
}
impl std::error::Error for CapacityError {}

/// Validated declaration facts, independent of each row's nullable value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    source: EncodingIdentity,
    source_form: SourceForm,
    target: ProjectionTarget,
    family: Family,
    capacity: Capacity,
}

impl Plan {
    pub fn new(
        source: EncodingIdentity,
        source_form: SourceForm,
        target: ProjectionTarget,
        family: Family,
        capacity: Capacity,
    ) -> Result<Self, CapacityError> {
        if !matches!(
            source,
            EncodingIdentity::Cp1251 | EncodingIdentity::Cp1252 | EncodingIdentity::Utf8
        ) {
            return Err(ProjectionError::UnsupportedSource(source).into());
        }
        if let ProjectionTarget::Native(encoding) = target
            && !matches!(
                encoding,
                EncodingIdentity::Cp1251 | EncodingIdentity::Cp1252 | EncodingIdentity::Utf8
            )
        {
            return Err(ProjectionError::UnsupportedTarget(encoding).into());
        }
        if family == Family::Fixed && capacity == Capacity::Max {
            return Err(CapacityError::FixedMax);
        }
        if let Capacity::Bounded(width) = capacity {
            let maximum = if target == ProjectionTarget::SqlUtf16 {
                4000
            } else {
                8000
            };
            if width == 0 || width > maximum {
                return Err(CapacityError::InvalidWidth {
                    requested: width,
                    maximum,
                });
            }
        }
        Ok(Self {
            source,
            source_form,
            target,
            family,
            capacity,
        })
    }

    /// Apply to source bytes already admitted by the root declaration/wire plan.
    /// In particular, this cannot validate source CHAR-to-MAX or TYPE_INFO shape.
    /// Only the final payload is allocated; conversion expansion is preflighted.
    pub fn apply(
        self,
        value: Option<AnsiView<'_>>,
        limits: ProjectionLimits,
    ) -> Result<Option<ProjectedValue>, CapacityError> {
        let Some(value) = value else { return Ok(None) };
        if value.encoding() != self.source {
            return Err(ProjectionError::SourceEncodingMismatch {
                declared: self.source,
                actual: value.encoding(),
            }
            .into());
        }
        let input = value.bytes();
        check_limit(Resource::Input, input.len(), limits.input_bytes)?;
        if self.source == EncodingIdentity::Utf8 {
            return self.apply_utf8(input, limits).map(Some);
        }
        if let Capacity::Bounded(width) = self.capacity
            && input.len() > width
        {
            // Bounded sources and same-codepage MAX conversion inspect all
            // overflow; cross-encoding MAX conversion has a shorter window.
            // Do not infer source form from values or replace this with
            // universal rtrim/a Unicode whitespace predicate.
            let end = if self.source_form == SourceForm::Bounded
                || self.target == ProjectionTarget::Native(self.source)
            {
                input.len()
            } else {
                width + width.min(input.len() - width)
            };
            if input[width..end].iter().any(|&byte| byte != b' ') {
                return Err(CapacityError::Truncation {
                    source_bytes: input.len(),
                    target_capacity: width,
                });
            }
        }
        let characters = if self.source == EncodingIdentity::Cp1251 {
            &CP1251_TO_SQL_CHAR
        } else {
            &CP1252_TO_SQL_CHAR
        };
        let mut prefix_len = 0usize;
        let mut payload_len = 0usize;
        for &byte in input {
            let size = if self.target == ProjectionTarget::Native(EncodingIdentity::Utf8) {
                characters[usize::from(byte)].len_utf8()
            } else {
                1
            };
            let next = payload_len
                .checked_add(size)
                .ok_or(ProjectionError::LengthOverflow)?;
            if matches!(self.capacity, Capacity::Bounded(width) if next > width) {
                break;
            }
            payload_len = next;
            prefix_len += 1;
        }
        let final_len = match (self.family, self.capacity) {
            (Family::Fixed, Capacity::Bounded(width)) => width,
            _ => payload_len,
        };
        let output_bytes = if self.target == ProjectionTarget::SqlUtf16 {
            final_len
                .checked_mul(2)
                .ok_or(ProjectionError::LengthOverflow)?
        } else {
            final_len
        };
        check_limit(Resource::Output, output_bytes, limits.output_bytes)?;
        let result = match self.target {
            ProjectionTarget::SqlUtf16 => {
                let mut units = allocate(final_len, output_bytes)?;
                units.extend(
                    input[..prefix_len]
                        .iter()
                        .map(|&byte| characters[usize::from(byte)] as u16),
                );
                units.resize(final_len, u16::from(b' '));
                ProjectedValue::SqlUtf16(units)
            }
            ProjectionTarget::Native(encoding) => {
                let mut bytes = allocate(final_len, output_bytes)?;
                match encoding {
                    EncodingIdentity::Cp1251 if self.source == EncodingIdentity::Cp1251 => {
                        bytes.extend_from_slice(&input[..prefix_len])
                    }
                    EncodingIdentity::Cp1251 => bytes.extend(
                        input[..prefix_len]
                            .iter()
                            .map(|&byte| CP1252_TO_CP1251[usize::from(byte)]),
                    ),
                    EncodingIdentity::Cp1252 if self.source == EncodingIdentity::Cp1252 => {
                        bytes.extend_from_slice(&input[..prefix_len])
                    }
                    EncodingIdentity::Cp1252 => bytes.extend(
                        input[..prefix_len]
                            .iter()
                            .map(|&byte| CP1251_TO_CP1252[usize::from(byte)]),
                    ),
                    EncodingIdentity::Utf8 => {
                        let mut encoded = [0; 4];
                        for &byte in &input[..prefix_len] {
                            bytes.extend_from_slice(
                                characters[usize::from(byte)]
                                    .encode_utf8(&mut encoded)
                                    .as_bytes(),
                            );
                        }
                    }
                    _ => unreachable!("validated capacity plan target"),
                }
                bytes.resize(final_len, b' ');
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
        Ok(Some(result))
    }
}

// BulkLoad validates the original EOF before conversion. This nominal span
// check intentionally does not substitute strict scalar UTF8 validation.
fn nominal_width(byte: u8) -> usize {
    match byte {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}
fn tail_span(input: &[u8]) -> Result<usize, CapacityError> {
    if input.is_empty() {
        return Ok(0);
    }
    for distance in 1..=input.len().min(4) {
        let width = nominal_width(input[input.len() - distance]);
        if width == distance {
            return Ok(distance);
        }
        if width != 0 {
            return Err(CapacityError::Utf8StreamInput);
        }
    }
    Err(CapacityError::Utf8Boundary)
}
// This fitting operation is independently observed at BulkLoad's conversion
// window. Original source EOF admission above remains stricter than at-rest
// fitting. Selector/base handling agrees with stored_utf8's measured primitive.
fn previous_span(input: &[u8]) -> Result<usize, CapacityError> {
    for distance in 1..=input.len().min(4) {
        let width = nominal_width(input[input.len() - distance]);
        if width == distance {
            return Ok(distance);
        }
        if width != 0 {
            return Err(CapacityError::Utf8Boundary);
        }
    }
    Err(CapacityError::Utf8Boundary)
}
// An incomplete selector can make its preceding base unavailable at EOF.
// Complete selectors protect a previous pair from being removed a second time.
// These exact ranges and partial prefixes are grounded in SQL17 endpoint probes.
fn selector_prefix(input: &[u8]) -> bool {
    match input {
        [0xe1]
        | [0xef]
        | [0xf3]
        | [0xe1, 0xa0]
        | [0xef, 0xb8]
        | [0xf3, 0xa0]
        | [0xe1, 0xa0, 0x8b..=0x8d, ..]
        | [0xef, 0xb8, 0x80..=0x8f, ..]
        | [0xf3, 0xa0, 0x84..=0x87] => true,
        // SQL's nominal supplementary-selector recognition masks the final
        // payload bits even when that byte is not a valid continuation. The
        // repair pass still emits replacements for those original bytes.
        [0xf3, 0xa0, 0x84..=0x86, _, ..] => true,
        [0xf3, 0xa0, 0x87, last, ..] => last & 0x3f <= 0x2f,
        _ => false,
    }
}

fn discard_previous(input: &[u8]) -> Result<&[u8], CapacityError> {
    if input.is_empty() {
        return Ok(input);
    }
    let distance = previous_span(input)?;
    let boundary = input.len() - distance;
    if selector_prefix(&input[boundary..]) {
        Ok(input)
    } else {
        Ok(&input[..boundary])
    }
}
fn fit_utf8(input: &[u8]) -> Result<&[u8], CapacityError> {
    let Some(&last) = input.last() else {
        return Ok(input);
    };
    if last < 0x80 {
        return Ok(input);
    }
    for distance in 1..=input.len().min(4) {
        let boundary = input.len() - distance;
        let width = nominal_width(input[boundary]);
        if width == distance {
            return Ok(input);
        }
        if width != 0 {
            let prefix = &input[..boundary];
            if selector_prefix(&input[boundary..]) {
                return discard_previous(prefix);
            }
            if !prefix.is_empty() {
                previous_span(prefix)?;
            }
            return Ok(prefix);
        }
    }
    Err(CapacityError::Utf8Boundary)
}

// SQL's forward repair consumes a forbidden range's lead/second byte together.
// The walker emits complete scalar pairs and uses no expanded temporary.
fn walk_utf8(input: &[u8], mut emit: impl FnMut(&[u16])) {
    let mut offset = 0;
    while offset < input.len() {
        let byte = input[offset];
        let mut units = [0u16; 2];
        let mut count = 1;
        if byte < 0x80 {
            units[0] = u16::from(byte);
            offset += 1;
        } else {
            let width = nominal_width(byte);
            if width == 0 {
                units[0] = 0xfffd;
                offset += 1;
            } else {
                let mut end = offset + 1;
                while end < input.len()
                    && end - offset < width
                    && (0x80..=0xbf).contains(&input[end])
                {
                    end += 1;
                }
                let forbidden = end > offset + 1
                    && match byte {
                        0xe0 => input[offset + 1] < 0xa0,
                        0xed => input[offset + 1] >= 0xa0,
                        0xf0 => input[offset + 1] < 0x90,
                        0xf4 => input[offset + 1] >= 0x90,
                        _ => false,
                    };
                if forbidden {
                    units[0] = 0xfffd;
                    offset += 2;
                } else if end - offset < width {
                    units[0] = 0xfffd;
                    offset = end;
                } else {
                    let mut scalar = u32::from(byte & ((1 << (7 - width)) - 1));
                    for &b in &input[offset + 1..end] {
                        scalar = (scalar << 6) | u32::from(b & 0x3f);
                    }
                    offset = end;
                    if scalar <= 0xffff {
                        units[0] = scalar as u16;
                    } else {
                        let n = scalar - 0x10000;
                        units = [0xd800 + (n >> 10) as u16, 0xdc00 + (n & 0x3ff) as u16];
                        count = 2;
                    }
                }
            }
        }
        emit(&units[..count]);
    }
}
fn codepage_unit(target: EncodingIdentity, unit: u16) -> u8 {
    if target == EncodingIdentity::Cp1252 {
        const MAP: &[u8; 65536] = include_bytes!("../character/windows_1252.bin");
        MAP[usize::from(unit)]
    } else {
        CP1251_BMP
            .binary_search_by_key(&unit, |&(unit, _)| unit)
            .map_or(b'?', |index| CP1251_BMP[index].1)
    }
}
impl Plan {
    fn apply_utf8(
        self,
        input: &[u8],
        limits: ProjectionLimits,
    ) -> Result<ProjectedValue, CapacityError> {
        if let Err(error) = tail_span(input) {
            return Err(
                if error == CapacityError::Utf8StreamInput
                    && self.capacity == Capacity::Max
                    && self.target == ProjectionTarget::Native(EncodingIdentity::Utf8)
                {
                    CapacityError::Utf8BcpInput
                } else {
                    error
                },
            );
        }
        if self.target == ProjectionTarget::Native(EncodingIdentity::Utf8) {
            let prefix = match self.capacity {
                Capacity::Bounded(width) => fit_utf8(&input[..input.len().min(width)])?,
                Capacity::Max => input,
            };
            if let Capacity::Bounded(width) = self.capacity
                && input
                    .get(width..)
                    .is_some_and(|tail| tail.iter().any(|&b| b != b' '))
            {
                return Err(CapacityError::Truncation {
                    source_bytes: input.len(),
                    target_capacity: width,
                });
            }
            let final_len = match (self.family, self.capacity) {
                (Family::Fixed, Capacity::Bounded(width)) => width,
                _ => prefix.len(),
            };
            check_limit(Resource::Output, final_len, limits.output_bytes)?;
            let mut bytes = allocate(final_len, final_len)?;
            bytes.extend_from_slice(prefix);
            bytes.resize(final_len, b' ');
            return Ok(ProjectedValue::Native(
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
            ));
        }
        let window = match (self.source_form, self.capacity) {
            (SourceForm::Max, Capacity::Bounded(width)) => {
                let end = width
                    .checked_mul(3)
                    .ok_or(ProjectionError::LengthOverflow)?
                    .min(input.len());
                fit_utf8(&input[..end])?
            }
            _ => input,
        };
        let native_codepage = matches!(
            self.target,
            ProjectionTarget::Native(EncodingIdentity::Cp1251 | EncodingIdentity::Cp1252)
        );
        if native_codepage
            && let Capacity::Bounded(width) = self.capacity
            && window
                .get(width..)
                .is_some_and(|tail| tail.iter().any(|&byte| byte != b' '))
        {
            return Err(CapacityError::Truncation {
                source_bytes: input.len(),
                target_capacity: width,
            });
        }
        let mut count = 0usize;
        let mut prefix_units = 0usize;
        let mut stopped = false;
        let mut truncated = false;
        let mut overflow = false;
        walk_utf8(window, |units| {
            let Some(next) = count.checked_add(units.len()) else {
                overflow = true;
                return;
            };
            if let Capacity::Bounded(width) = self.capacity {
                for (i, &unit) in units.iter().enumerate() {
                    if !native_codepage && count + i >= width && unit != u16::from(b' ') {
                        truncated = true;
                    }
                }
                if next > width {
                    stopped = true;
                }
            }
            if !stopped {
                prefix_units = next;
            }
            count = next;
        });
        if overflow {
            return Err(ProjectionError::LengthOverflow.into());
        }
        if truncated {
            return Err(CapacityError::Truncation {
                source_bytes: input.len(),
                target_capacity: match self.capacity {
                    Capacity::Bounded(w) => w,
                    Capacity::Max => unreachable!(),
                },
            });
        }
        let final_len = match (self.family, self.capacity) {
            (Family::Fixed, Capacity::Bounded(width)) => width,
            _ => prefix_units,
        };
        let bytes = if self.target == ProjectionTarget::SqlUtf16 {
            final_len
                .checked_mul(2)
                .ok_or(ProjectionError::LengthOverflow)?
        } else {
            final_len
        };
        check_limit(Resource::Output, bytes, limits.output_bytes)?;
        if self.target == ProjectionTarget::SqlUtf16 {
            let mut output = allocate(final_len, bytes)?;
            walk_utf8(window, |units| {
                if output
                    .len()
                    .checked_add(units.len())
                    .is_some_and(|next| next <= prefix_units)
                {
                    output.extend_from_slice(units);
                }
            });
            output.resize(final_len, u16::from(b' '));
            Ok(ProjectedValue::SqlUtf16(output))
        } else {
            let ProjectionTarget::Native(target) = self.target else {
                unreachable!()
            };
            let mut output = allocate(final_len, bytes)?;
            walk_utf8(window, |units| {
                if output
                    .len()
                    .checked_add(units.len())
                    .is_some_and(|next| next <= prefix_units)
                {
                    output.extend(units.iter().map(|&unit| codepage_unit(target, unit)));
                }
            });
            output.resize(final_len, b' ');
            Ok(ProjectedValue::Native(
                AnsiBytes::from_vec(target, output, limits.output_bytes).map_err(|error| {
                    match error {
                        ByteError::LengthOverflow => ProjectionError::LengthOverflow,
                        ByteError::Limit { requested, maximum } => ProjectionError::Limit {
                            resource: Resource::Output,
                            requested,
                            maximum,
                        },
                    }
                })?,
            ))
        }
    }
}

// Original native CP1251 output for every valid BMP scalar, acquired by task942.
// Surrogate units project to question marks, as original supplementary controls retain.
const CP1251_BMP: &[(u16, u8)] = &[
    (0x0000, 0x00),
    (0x0001, 0x01),
    (0x0002, 0x02),
    (0x0003, 0x03),
    (0x0004, 0x04),
    (0x0005, 0x05),
    (0x0006, 0x06),
    (0x0007, 0x07),
    (0x0008, 0x08),
    (0x0009, 0x09),
    (0x000a, 0x0a),
    (0x000b, 0x0b),
    (0x000c, 0x0c),
    (0x000d, 0x0d),
    (0x000e, 0x0e),
    (0x000f, 0x0f),
    (0x0010, 0x10),
    (0x0011, 0x11),
    (0x0012, 0x12),
    (0x0013, 0x13),
    (0x0014, 0x14),
    (0x0015, 0x15),
    (0x0016, 0x16),
    (0x0017, 0x17),
    (0x0018, 0x18),
    (0x0019, 0x19),
    (0x001a, 0x1a),
    (0x001b, 0x1b),
    (0x001c, 0x1c),
    (0x001d, 0x1d),
    (0x001e, 0x1e),
    (0x001f, 0x1f),
    (0x0020, 0x20),
    (0x0021, 0x21),
    (0x0022, 0x22),
    (0x0023, 0x23),
    (0x0024, 0x24),
    (0x0025, 0x25),
    (0x0026, 0x26),
    (0x0027, 0x27),
    (0x0028, 0x28),
    (0x0029, 0x29),
    (0x002a, 0x2a),
    (0x002b, 0x2b),
    (0x002c, 0x2c),
    (0x002d, 0x2d),
    (0x002e, 0x2e),
    (0x002f, 0x2f),
    (0x0030, 0x30),
    (0x0031, 0x31),
    (0x0032, 0x32),
    (0x0033, 0x33),
    (0x0034, 0x34),
    (0x0035, 0x35),
    (0x0036, 0x36),
    (0x0037, 0x37),
    (0x0038, 0x38),
    (0x0039, 0x39),
    (0x003a, 0x3a),
    (0x003b, 0x3b),
    (0x003c, 0x3c),
    (0x003d, 0x3d),
    (0x003e, 0x3e),
    (0x0040, 0x40),
    (0x0041, 0x41),
    (0x0042, 0x42),
    (0x0043, 0x43),
    (0x0044, 0x44),
    (0x0045, 0x45),
    (0x0046, 0x46),
    (0x0047, 0x47),
    (0x0048, 0x48),
    (0x0049, 0x49),
    (0x004a, 0x4a),
    (0x004b, 0x4b),
    (0x004c, 0x4c),
    (0x004d, 0x4d),
    (0x004e, 0x4e),
    (0x004f, 0x4f),
    (0x0050, 0x50),
    (0x0051, 0x51),
    (0x0052, 0x52),
    (0x0053, 0x53),
    (0x0054, 0x54),
    (0x0055, 0x55),
    (0x0056, 0x56),
    (0x0057, 0x57),
    (0x0058, 0x58),
    (0x0059, 0x59),
    (0x005a, 0x5a),
    (0x005b, 0x5b),
    (0x005c, 0x5c),
    (0x005d, 0x5d),
    (0x005e, 0x5e),
    (0x005f, 0x5f),
    (0x0060, 0x60),
    (0x0061, 0x61),
    (0x0062, 0x62),
    (0x0063, 0x63),
    (0x0064, 0x64),
    (0x0065, 0x65),
    (0x0066, 0x66),
    (0x0067, 0x67),
    (0x0068, 0x68),
    (0x0069, 0x69),
    (0x006a, 0x6a),
    (0x006b, 0x6b),
    (0x006c, 0x6c),
    (0x006d, 0x6d),
    (0x006e, 0x6e),
    (0x006f, 0x6f),
    (0x0070, 0x70),
    (0x0071, 0x71),
    (0x0072, 0x72),
    (0x0073, 0x73),
    (0x0074, 0x74),
    (0x0075, 0x75),
    (0x0076, 0x76),
    (0x0077, 0x77),
    (0x0078, 0x78),
    (0x0079, 0x79),
    (0x007a, 0x7a),
    (0x007b, 0x7b),
    (0x007c, 0x7c),
    (0x007d, 0x7d),
    (0x007e, 0x7e),
    (0x007f, 0x7f),
    (0x0098, 0x98),
    (0x00a0, 0xa0),
    (0x00a4, 0xa4),
    (0x00a6, 0xa6),
    (0x00a7, 0xa7),
    (0x00a9, 0xa9),
    (0x00ab, 0xab),
    (0x00ac, 0xac),
    (0x00ad, 0xad),
    (0x00ae, 0xae),
    (0x00b0, 0xb0),
    (0x00b1, 0xb1),
    (0x00b5, 0xb5),
    (0x00b6, 0xb6),
    (0x00b7, 0xb7),
    (0x00bb, 0xbb),
    (0x00c0, 0x41),
    (0x00c1, 0x41),
    (0x00c2, 0x41),
    (0x00c3, 0x41),
    (0x00c4, 0x41),
    (0x00c5, 0x41),
    (0x00c7, 0x43),
    (0x00c8, 0x45),
    (0x00c9, 0x45),
    (0x00ca, 0x45),
    (0x00cb, 0x45),
    (0x00cc, 0x49),
    (0x00cd, 0x49),
    (0x00ce, 0x49),
    (0x00cf, 0x49),
    (0x00d1, 0x4e),
    (0x00d2, 0x4f),
    (0x00d3, 0x4f),
    (0x00d4, 0x4f),
    (0x00d5, 0x4f),
    (0x00d6, 0x4f),
    (0x00d8, 0x4f),
    (0x00d9, 0x55),
    (0x00da, 0x55),
    (0x00db, 0x55),
    (0x00dc, 0x55),
    (0x00dd, 0x59),
    (0x00e0, 0x61),
    (0x00e1, 0x61),
    (0x00e2, 0x61),
    (0x00e3, 0x61),
    (0x00e4, 0x61),
    (0x00e5, 0x61),
    (0x00e7, 0x63),
    (0x00e8, 0x65),
    (0x00e9, 0x65),
    (0x00ea, 0x65),
    (0x00eb, 0x65),
    (0x00ec, 0x69),
    (0x00ed, 0x69),
    (0x00ee, 0x69),
    (0x00ef, 0x69),
    (0x00f1, 0x6e),
    (0x00f2, 0x6f),
    (0x00f3, 0x6f),
    (0x00f4, 0x6f),
    (0x00f5, 0x6f),
    (0x00f6, 0x6f),
    (0x00f8, 0x6f),
    (0x00f9, 0x75),
    (0x00fa, 0x75),
    (0x00fb, 0x75),
    (0x00fc, 0x75),
    (0x00fd, 0x79),
    (0x00ff, 0x79),
    (0x0100, 0x41),
    (0x0101, 0x61),
    (0x0102, 0x41),
    (0x0103, 0x61),
    (0x0104, 0x41),
    (0x0105, 0x61),
    (0x0106, 0x43),
    (0x0107, 0x63),
    (0x0108, 0x43),
    (0x0109, 0x63),
    (0x010a, 0x43),
    (0x010b, 0x63),
    (0x010c, 0x43),
    (0x010d, 0x63),
    (0x010e, 0x44),
    (0x010f, 0x64),
    (0x0110, 0x44),
    (0x0111, 0x64),
    (0x0112, 0x45),
    (0x0113, 0x65),
    (0x0114, 0x45),
    (0x0115, 0x65),
    (0x0116, 0x45),
    (0x0117, 0x65),
    (0x0118, 0x45),
    (0x0119, 0x65),
    (0x011a, 0x45),
    (0x011b, 0x65),
    (0x011c, 0x47),
    (0x011d, 0x67),
    (0x011e, 0x47),
    (0x011f, 0x67),
    (0x0120, 0x47),
    (0x0121, 0x67),
    (0x0122, 0x47),
    (0x0123, 0x67),
    (0x0124, 0x48),
    (0x0125, 0x68),
    (0x0126, 0x48),
    (0x0127, 0x68),
    (0x0128, 0x49),
    (0x0129, 0x69),
    (0x012a, 0x49),
    (0x012b, 0x69),
    (0x012c, 0x49),
    (0x012d, 0x69),
    (0x012e, 0x49),
    (0x012f, 0x69),
    (0x0130, 0x49),
    (0x0134, 0x4a),
    (0x0135, 0x6a),
    (0x0136, 0x4b),
    (0x0137, 0x6b),
    (0x0139, 0x4c),
    (0x013a, 0x6c),
    (0x013b, 0x4c),
    (0x013c, 0x6c),
    (0x013d, 0x4c),
    (0x013e, 0x6c),
    (0x0141, 0x4c),
    (0x0142, 0x6c),
    (0x0143, 0x4e),
    (0x0144, 0x6e),
    (0x0145, 0x4e),
    (0x0146, 0x6e),
    (0x0147, 0x4e),
    (0x0148, 0x6e),
    (0x014c, 0x4f),
    (0x014d, 0x6f),
    (0x014e, 0x4f),
    (0x014f, 0x6f),
    (0x0150, 0x4f),
    (0x0151, 0x6f),
    (0x0154, 0x52),
    (0x0155, 0x72),
    (0x0156, 0x52),
    (0x0157, 0x72),
    (0x0158, 0x52),
    (0x0159, 0x72),
    (0x015a, 0x53),
    (0x015b, 0x73),
    (0x015c, 0x53),
    (0x015d, 0x73),
    (0x015e, 0x53),
    (0x015f, 0x73),
    (0x0160, 0x53),
    (0x0161, 0x73),
    (0x0162, 0x54),
    (0x0163, 0x74),
    (0x0164, 0x54),
    (0x0165, 0x74),
    (0x0166, 0x54),
    (0x0167, 0x74),
    (0x0168, 0x55),
    (0x0169, 0x75),
    (0x016a, 0x55),
    (0x016b, 0x75),
    (0x016c, 0x55),
    (0x016d, 0x75),
    (0x016e, 0x55),
    (0x016f, 0x75),
    (0x0170, 0x55),
    (0x0171, 0x75),
    (0x0172, 0x55),
    (0x0173, 0x75),
    (0x0174, 0x57),
    (0x0175, 0x77),
    (0x0176, 0x59),
    (0x0177, 0x79),
    (0x0178, 0x59),
    (0x0179, 0x5a),
    (0x017a, 0x7a),
    (0x017b, 0x5a),
    (0x017c, 0x7a),
    (0x017d, 0x5a),
    (0x017e, 0x7a),
    (0x0180, 0x62),
    (0x0197, 0x49),
    (0x019a, 0x6c),
    (0x019f, 0x4f),
    (0x01a0, 0x4f),
    (0x01a1, 0x6f),
    (0x01ab, 0x74),
    (0x01ae, 0x54),
    (0x01af, 0x55),
    (0x01b0, 0x75),
    (0x01cd, 0x41),
    (0x01ce, 0x61),
    (0x01cf, 0x49),
    (0x01d0, 0x69),
    (0x01d1, 0x4f),
    (0x01d2, 0x6f),
    (0x01d3, 0x55),
    (0x01d4, 0x75),
    (0x01d5, 0x55),
    (0x01d6, 0x75),
    (0x01d7, 0x55),
    (0x01d8, 0x75),
    (0x01d9, 0x55),
    (0x01da, 0x75),
    (0x01db, 0x55),
    (0x01dc, 0x75),
    (0x01de, 0x41),
    (0x01df, 0x61),
    (0x01e4, 0x47),
    (0x01e5, 0x67),
    (0x01e6, 0x47),
    (0x01e7, 0x67),
    (0x01e8, 0x4b),
    (0x01e9, 0x6b),
    (0x01ea, 0x4f),
    (0x01eb, 0x6f),
    (0x01ec, 0x4f),
    (0x01ed, 0x6f),
    (0x01f0, 0x6a),
    (0x0401, 0xa8),
    (0x0402, 0x80),
    (0x0403, 0x81),
    (0x0404, 0xaa),
    (0x0405, 0xbd),
    (0x0406, 0xb2),
    (0x0407, 0xaf),
    (0x0408, 0xa3),
    (0x0409, 0x8a),
    (0x040a, 0x8c),
    (0x040b, 0x8e),
    (0x040c, 0x8d),
    (0x040e, 0xa1),
    (0x040f, 0x8f),
    (0x0410, 0xc0),
    (0x0411, 0xc1),
    (0x0412, 0xc2),
    (0x0413, 0xc3),
    (0x0414, 0xc4),
    (0x0415, 0xc5),
    (0x0416, 0xc6),
    (0x0417, 0xc7),
    (0x0418, 0xc8),
    (0x0419, 0xc9),
    (0x041a, 0xca),
    (0x041b, 0xcb),
    (0x041c, 0xcc),
    (0x041d, 0xcd),
    (0x041e, 0xce),
    (0x041f, 0xcf),
    (0x0420, 0xd0),
    (0x0421, 0xd1),
    (0x0422, 0xd2),
    (0x0423, 0xd3),
    (0x0424, 0xd4),
    (0x0425, 0xd5),
    (0x0426, 0xd6),
    (0x0427, 0xd7),
    (0x0428, 0xd8),
    (0x0429, 0xd9),
    (0x042a, 0xda),
    (0x042b, 0xdb),
    (0x042c, 0xdc),
    (0x042d, 0xdd),
    (0x042e, 0xde),
    (0x042f, 0xdf),
    (0x0430, 0xe0),
    (0x0431, 0xe1),
    (0x0432, 0xe2),
    (0x0433, 0xe3),
    (0x0434, 0xe4),
    (0x0435, 0xe5),
    (0x0436, 0xe6),
    (0x0437, 0xe7),
    (0x0438, 0xe8),
    (0x0439, 0xe9),
    (0x043a, 0xea),
    (0x043b, 0xeb),
    (0x043c, 0xec),
    (0x043d, 0xed),
    (0x043e, 0xee),
    (0x043f, 0xef),
    (0x0440, 0xf0),
    (0x0441, 0xf1),
    (0x0442, 0xf2),
    (0x0443, 0xf3),
    (0x0444, 0xf4),
    (0x0445, 0xf5),
    (0x0446, 0xf6),
    (0x0447, 0xf7),
    (0x0448, 0xf8),
    (0x0449, 0xf9),
    (0x044a, 0xfa),
    (0x044b, 0xfb),
    (0x044c, 0xfc),
    (0x044d, 0xfd),
    (0x044e, 0xfe),
    (0x044f, 0xff),
    (0x0451, 0xb8),
    (0x0452, 0x90),
    (0x0453, 0x83),
    (0x0454, 0xba),
    (0x0455, 0xbe),
    (0x0456, 0xb3),
    (0x0457, 0xbf),
    (0x0458, 0xbc),
    (0x0459, 0x9a),
    (0x045a, 0x9c),
    (0x045b, 0x9e),
    (0x045c, 0x9d),
    (0x045e, 0xa2),
    (0x045f, 0x9f),
    (0x0490, 0xa5),
    (0x0491, 0xb4),
    (0x2000, 0x20),
    (0x2001, 0x20),
    (0x2002, 0x20),
    (0x2003, 0x20),
    (0x2004, 0x20),
    (0x2005, 0x20),
    (0x2006, 0x20),
    (0x2007, 0xa0),
    (0x2008, 0x20),
    (0x2009, 0x20),
    (0x200a, 0x20),
    (0x2010, 0x2d),
    (0x2011, 0x2d),
    (0x2012, 0x2d),
    (0x2013, 0x96),
    (0x2014, 0x97),
    (0x2015, 0x2d),
    (0x2018, 0x91),
    (0x2019, 0x92),
    (0x201a, 0x82),
    (0x201b, 0x27),
    (0x201c, 0x93),
    (0x201d, 0x94),
    (0x201e, 0x84),
    (0x201f, 0x22),
    (0x2020, 0x86),
    (0x2021, 0x87),
    (0x2022, 0x95),
    (0x2024, 0x2e),
    (0x2026, 0x85),
    (0x202f, 0xa0),
    (0x2030, 0x89),
    (0x2032, 0x27),
    (0x2033, 0x22),
    (0x2035, 0x27),
    (0x2036, 0x22),
    (0x2039, 0x8b),
    (0x203a, 0x9b),
    (0x203c, 0x21),
    (0x2044, 0x2f),
    (0x20ac, 0x88),
    (0x2116, 0xb9),
    (0x2122, 0x99),
    (0x2190, 0x3c),
    (0x2191, 0x5e),
    (0x2192, 0x3e),
    (0x2193, 0x76),
    (0x2194, 0x2d),
    (0x2195, 0xa6),
    (0x21a8, 0xa6),
    (0x2219, 0x95),
    (0x221a, 0x76),
    (0x221f, 0x4c),
    (0x2236, 0x3a),
    (0x2302, 0xa6),
    (0x2500, 0x2d),
    (0x2502, 0xa6),
    (0x250c, 0x2d),
    (0x2510, 0xac),
    (0x2514, 0x4c),
    (0x2518, 0x2d),
    (0x251c, 0x2b),
    (0x2524, 0x2b),
    (0x252c, 0x54),
    (0x2534, 0x2b),
    (0x253c, 0x2b),
    (0x2550, 0x3d),
    (0x2551, 0xa6),
    (0x2552, 0x2d),
    (0x2553, 0xe3),
    (0x2554, 0xe3),
    (0x2555, 0xac),
    (0x2556, 0xac),
    (0x2557, 0xac),
    (0x2558, 0x4c),
    (0x2559, 0x4c),
    (0x255a, 0x4c),
    (0x255b, 0x2d),
    (0x255c, 0x2d),
    (0x255d, 0x2d),
    (0x255e, 0xa6),
    (0x255f, 0xa6),
    (0x2560, 0xa6),
    (0x2561, 0xa6),
    (0x2562, 0xa6),
    (0x2563, 0xa6),
    (0x2564, 0x54),
    (0x2565, 0x54),
    (0x2566, 0x54),
    (0x2567, 0xa6),
    (0x2568, 0xa6),
    (0x2569, 0xa6),
    (0x256a, 0x2b),
    (0x256b, 0x2b),
    (0x256c, 0x2b),
    (0x2580, 0x2d),
    (0x2584, 0x2d),
    (0x2588, 0x2d),
    (0x258c, 0xa6),
    (0x2590, 0xa6),
    (0x2591, 0x2d),
    (0x2592, 0x2d),
    (0x2593, 0x2d),
    (0x25a0, 0xa6),
    (0x25ac, 0x2d),
    (0x25b2, 0x5e),
    (0x25ba, 0x3e),
    (0x25bc, 0xa1),
    (0x25c4, 0x3c),
    (0x25cb, 0x30),
    (0x25d8, 0x95),
    (0x25d9, 0x30),
    (0x263a, 0x4f),
    (0x263b, 0x4f),
    (0x263c, 0x30),
    (0x2640, 0x2b),
    (0x2642, 0x3e),
    (0x2660, 0xa6),
    (0x2663, 0xa6),
    (0x2665, 0xa6),
    (0x2666, 0xa6),
    (0x266a, 0x64),
    (0x266b, 0x64),
    (0xff01, 0x21),
    (0xff02, 0x22),
    (0xff03, 0x23),
    (0xff04, 0x24),
    (0xff05, 0x25),
    (0xff06, 0x26),
    (0xff07, 0x27),
    (0xff08, 0x28),
    (0xff09, 0x29),
    (0xff0a, 0x2a),
    (0xff0b, 0x2b),
    (0xff0c, 0x2c),
    (0xff0d, 0x2d),
    (0xff0e, 0x2e),
    (0xff0f, 0x2f),
    (0xff10, 0x30),
    (0xff11, 0x31),
    (0xff12, 0x32),
    (0xff13, 0x33),
    (0xff14, 0x34),
    (0xff15, 0x35),
    (0xff16, 0x36),
    (0xff17, 0x37),
    (0xff18, 0x38),
    (0xff19, 0x39),
    (0xff1a, 0x3a),
    (0xff1b, 0x3b),
    (0xff1c, 0x3c),
    (0xff1d, 0x3d),
    (0xff1e, 0x3e),
    (0xff20, 0x40),
    (0xff21, 0x41),
    (0xff22, 0x42),
    (0xff23, 0x43),
    (0xff24, 0x44),
    (0xff25, 0x45),
    (0xff26, 0x46),
    (0xff27, 0x47),
    (0xff28, 0x48),
    (0xff29, 0x49),
    (0xff2a, 0x4a),
    (0xff2b, 0x4b),
    (0xff2c, 0x4c),
    (0xff2d, 0x4d),
    (0xff2e, 0x4e),
    (0xff2f, 0x4f),
    (0xff30, 0x50),
    (0xff31, 0x51),
    (0xff32, 0x52),
    (0xff33, 0x53),
    (0xff34, 0x54),
    (0xff35, 0x55),
    (0xff36, 0x56),
    (0xff37, 0x57),
    (0xff38, 0x58),
    (0xff39, 0x59),
    (0xff3a, 0x5a),
    (0xff3b, 0x5b),
    (0xff3c, 0x5c),
    (0xff3d, 0x5d),
    (0xff3e, 0x5e),
    (0xff3f, 0x5f),
    (0xff40, 0x60),
    (0xff41, 0x61),
    (0xff42, 0x62),
    (0xff43, 0x63),
    (0xff44, 0x64),
    (0xff45, 0x65),
    (0xff46, 0x66),
    (0xff47, 0x67),
    (0xff48, 0x68),
    (0xff49, 0x69),
    (0xff4a, 0x6a),
    (0xff4b, 0x6b),
    (0xff4c, 0x6c),
    (0xff4d, 0x6d),
    (0xff4e, 0x6e),
    (0xff4f, 0x6f),
    (0xff50, 0x70),
    (0xff51, 0x71),
    (0xff52, 0x72),
    (0xff53, 0x73),
    (0xff54, 0x74),
    (0xff55, 0x75),
    (0xff56, 0x76),
    (0xff57, 0x77),
    (0xff58, 0x78),
    (0xff59, 0x79),
    (0xff5a, 0x7a),
    (0xff5b, 0x7b),
    (0xff5c, 0x7c),
    (0xff5d, 0x7d),
    (0xff5e, 0x7e),
];
