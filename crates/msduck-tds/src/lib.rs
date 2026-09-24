//! Deterministic TDS 7.2–7.4 byte codecs and tokens; transport lives in the server.
#![forbid(unsafe_code)]
use anyhow::{Result, bail, ensure};

pub mod attention_completion;
pub mod collation;
pub mod framing;
pub mod request_lifecycle;
pub mod smp;

pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
pub const COLLATION: [u8; 5] = [0x09, 0x04, 0xd0, 0x00, 0x34];

pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.remaining(), "truncated TDS payload");
        let result = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(result)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    pub fn text(&mut self, chars: usize) -> Result<String> {
        decode_text(
            self.take(
                chars
                    .checked_mul(2)
                    .ok_or_else(|| anyhow::anyhow!("text too large"))?,
            )?,
        )
    }
}
pub fn decode_text(bytes: &[u8]) -> Result<String> {
    ensure!(bytes.len().is_multiple_of(2), "odd UTF-16 payload");
    Ok(String::from_utf16(
        &bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    )?)
}
pub fn text(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn btext(out: &mut Vec<u8>, s: &str) {
    let units: Vec<_> = s.encode_utf16().take(128).collect();
    out.push(units.len() as u8);
    out.extend(units.into_iter().flat_map(u16::to_le_bytes));
}
fn token(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    out.extend((body.len() as u16).to_le_bytes());
    out.extend(body);
}

pub struct Message {
    pub kind: u8,
    pub status: u8,
    pub payload: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptionPolicy {
    Unsupported,
    Required,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptionResult {
    Plaintext,
    Tls,
    Reject,
}

pub struct PreloginResponse {
    pub payload: Vec<u8>,
    pub encryption: EncryptionResult,
}

pub fn prelogin(data: &[u8]) -> Result<Vec<u8>> {
    let response = prelogin_with_policy(data, EncryptionPolicy::Unsupported)?;
    ensure!(
        response.encryption == EncryptionResult::Plaintext,
        "TLS requested but not configured"
    );
    Ok(response.payload)
}

/// MS-TDS 2.2.6.5: required encryption upgrades capable OFF clients too.
/// The caller must provide a TLS transport when selecting Required.
pub fn prelogin_with_policy(data: &[u8], policy: EncryptionPolicy) -> Result<PreloginResponse> {
    let mut c = Cursor::new(data);
    let mut options = Vec::new();
    loop {
        let id = c.u8()?;
        if id == 0xff {
            break;
        }
        let offset = u16::from_be_bytes(c.take(2)?.try_into()?) as usize;
        let len = u16::from_be_bytes(c.take(2)?.try_into()?) as usize;
        ensure!(offset + len <= data.len(), "invalid PRELOGIN option range");
        ensure!(
            !options.iter().any(|(k, _, _)| *k == id),
            "duplicate PRELOGIN option"
        );
        options.push((id, offset, len));
    }
    ensure!(
        matches!(options.first(), Some((0, _, 6))),
        "PRELOGIN VERSION must be first and six bytes"
    );
    let mut encryption = 0;
    for (id, offset, len) in options {
        ensure!(offset >= c.pos, "PRELOGIN option overlaps directory");
        if id == 1 {
            ensure!(
                len == 1 && data[offset] <= 3,
                "unsupported PRELOGIN encryption value"
            );
            encryption = data[offset];
        }
    }
    use EncryptionResult::*;
    let (response, encryption) = match (policy, encryption) {
        (EncryptionPolicy::Unsupported, 0 | 2) => (2, Plaintext),
        (EncryptionPolicy::Unsupported, _) => (2, Reject),
        (EncryptionPolicy::Required, 0) => (3, Tls),
        (EncryptionPolicy::Required, 1 | 3) => (1, Tls),
        (EncryptionPolicy::Required, _) => (3, Reject),
    };
    // VERSION, negotiated encryption, MARS disabled. Offsets are big-endian.
    Ok(PreloginResponse {
        payload: vec![
            0, 0, 16, 0, 6, 1, 0, 22, 0, 1, 4, 0, 23, 0, 1, 255, 16, 0, 0, 0, 0, 0, response, 0,
        ],
        encryption,
    })
}

pub struct Login {
    pub packet_size: usize,
    pub version: u32,
    pub database: String,
    pub user_name: String,
    pub password: zeroize::Zeroizing<String>,
}
pub fn login(data: &[u8]) -> Result<Login> {
    ensure!((94..=131071).contains(&data.len()), "invalid LOGIN7 size");
    let mut c = Cursor::new(data);
    ensure!(c.u32()? as usize == data.len(), "invalid LOGIN7 length");
    let requested = c.u32()?;
    let version = match requested {
        0x74000004 => 0x74000004,
        0x730b0003 => 0x730b0003,
        0x730a0003 => 0x730a0003,
        0x72090002 => 0x72090002,
        _ => bail!("unsupported TDS version {requested:08x}"),
    };
    let packet_size = c.u32()? as usize;
    ensure!((512..=32767).contains(&packet_size), "invalid packet size");
    ensure!(
        data[25] & 0x80 == 0,
        "integrated authentication is not supported"
    );
    ensure!(
        data[27] & 1 == 0,
        "LOGIN7 password changes are not supported"
    );
    // Credentials are decoded only after validating their bounded ranges.
    let mut user_name = String::new();
    let mut password = zeroize::Zeroizing::new(String::new());
    // Validate every ordinary offset/count pair, including the password; never log secrets.
    let mut database = String::new();
    for index in [0, 1, 2, 3, 4, 6, 7, 8] {
        let mut pair = Cursor::new(&data[36 + 4 * index..]);
        let offset = pair.u16()? as usize;
        let count = pair.u16()? as usize;
        ensure!(count <= 128, "LOGIN7 string exceeds protocol limit");
        ensure!(
            count == 0 || (offset >= 94 && offset + count * 2 <= data.len()),
            "invalid LOGIN7 string range"
        );
        if index == 1 && count > 0 {
            user_name = decode_text(&data[offset..offset + count * 2])?;
        }
        if index == 2 && count > 0 {
            let units = zeroize::Zeroizing::new(
                data[offset..offset + count * 2]
                    .chunks_exact(2)
                    .map(|bytes| {
                        u16::from_le_bytes([
                            (bytes[0] ^ 0xa5).rotate_right(4),
                            (bytes[1] ^ 0xa5).rotate_right(4),
                        ])
                    })
                    .collect::<Vec<_>>(),
            );
            password = zeroize::Zeroizing::new(String::from_utf16(&units)?);
        }
        if index == 8 && count > 0 {
            database = decode_text(&data[offset..offset + count * 2])?;
        }
    }
    ensure!(
        database.is_empty() || database.eq_ignore_ascii_case("master"),
        "database is not available"
    );
    Ok(Login {
        packet_size,
        version,
        database: "master".into(),
        user_name,
        password,
    })
}
pub fn done(out: &mut Vec<u8>, id: u8, status: u16, command: u16, count: u64) {
    out.push(id);
    out.extend(status.to_le_bytes());
    out.extend(command.to_le_bytes());
    out.extend(count.to_le_bytes());
}
pub fn env_text(out: &mut Vec<u8>, kind: u8, new: &str, old: &str) {
    let mut body = vec![kind];
    btext(&mut body, new);
    btext(&mut body, old);
    token(out, 0xe3, &body);
}
pub fn login_response(login: &Login) -> Vec<u8> {
    let mut out = Vec::new();
    env_text(&mut out, 1, &login.database, "");
    env_text(&mut out, 2, "us_english", "");
    env_text(&mut out, 4, &login.packet_size.to_string(), "4096");
    let mut collation = vec![7, 5];
    collation.extend(COLLATION);
    collation.push(0);
    token(&mut out, 0xe3, &collation);
    let mut ack = vec![1];
    ack.extend(login.version.to_be_bytes());
    btext(&mut ack, "msduck");
    ack.extend([16, 0, 0, 0]);
    token(&mut out, 0xad, &ack);
    done(&mut out, 0xfd, 0, 0, 0);
    out
}
pub fn error(out: &mut Vec<u8>, number: i32, message: &str) {
    let severity = if matches!(
        number,
        102 | 310
            | 8155
            | 8156
            | 8158
            | 8159
            | 144
            | 164
            | 10713
            | 10714
            | 5324
            | 174
            | 10759
            | 10753
            | 10754
            | 10755
            | 4106
            | 10756
            | 4194
            | 4123
    ) {
        15
    } else {
        16
    };
    diagnostic(out, 0xaa, severity, 1, number, message);
}
pub fn error_state(out: &mut Vec<u8>, number: i32, state: u8, message: &str) {
    diagnostic(out, 0xaa, 16, state, number, message);
}
/// Encode a typed SQL error, preserving number, severity, state and message.
/// Legacy `error` retains its separate message/number-based adapter policy.
pub fn sql_error(out: &mut Vec<u8>, error: &msduck_core::diagnostic::SqlError) {
    if let Some(units) = &error.message_utf16 {
        diagnostic_utf16(
            out,
            DiagnosticKind::Error,
            error.severity,
            error.state,
            error.number,
            units,
        );
        return;
    }
    diagnostic(
        out,
        0xaa,
        error.severity,
        error.state,
        error.number,
        &error.message,
    );
}
pub fn print_message(out: &mut Vec<u8>, message: &str) {
    diagnostic(out, 0xab, 0, 1, 0, message);
}
pub fn info(out: &mut Vec<u8>, number: i32, message: &str) {
    diagnostic(out, 0xab, 10, 1, number, message);
}
/// Select the token independently of severity: SQL normalizes RAISERROR severity
/// 10 to zero before calling the codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    Error,
    Information,
}

/// Encode a diagnostic without replacing unpaired UTF-16 surrogates. Callers
/// supply normalized SQL attributes; this codec has no error policy or effects.
/// Messages share the existing 16,000-unit transport bound.
pub fn diagnostic_utf16(
    out: &mut Vec<u8>,
    kind: DiagnosticKind,
    severity: u8,
    state: u8,
    number: i32,
    message: &[u16],
) {
    let mut body = Vec::new();
    body.extend(number.to_le_bytes());
    body.extend([state, severity]);
    let units = &message[..message.len().min(16000)];
    body.extend((units.len() as u16).to_le_bytes());
    body.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    btext(&mut body, "msduck");
    btext(&mut body, "");
    body.extend(1u32.to_le_bytes());
    token(
        out,
        match kind {
            DiagnosticKind::Error => 0xaa,
            DiagnosticKind::Information => 0xab,
        },
        &body,
    );
}
fn diagnostic(out: &mut Vec<u8>, kind: u8, severity: u8, state: u8, number: i32, message: &str) {
    let units: Vec<_> = message.encode_utf16().take(16000).collect();
    diagnostic_utf16(
        out,
        if kind == 0xaa {
            DiagnosticKind::Error
        } else {
            DiagnosticKind::Information
        },
        severity,
        state,
        number,
        &units,
    );
}
pub fn request_headers(data: &[u8]) -> Result<(&[u8], Option<u64>)> {
    let mut descriptor = None;
    let mut c = Cursor::new(data);
    let total = c.u32()? as usize;
    ensure!(
        total >= 4 && total <= data.len(),
        "invalid ALL_HEADERS length"
    );
    while c.pos < total {
        let len = c.u32()? as usize;
        let kind = c.u16()?;
        ensure!(
            len >= 6 && c.pos + len - 6 <= total,
            "invalid ALL_HEADERS entry"
        );
        if kind == 2 {
            ensure!(
                len == 18 && descriptor.is_none(),
                "invalid transaction header"
            );
            descriptor = Some(c.u64()?);
            c.u32()?;
        } else {
            c.take(len - 6)?;
        }
    }
    Ok((&data[total..], descriptor))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Variant,
    Int(u8),
    Bit,
    Float(u8),
    Text,
    Varchar(u16),
    Nvarchar(u16),
    Nchar(u16),
    Char(u16),
    Binary,
    Varbinary(u16),
    FixedBinary(u16),
    Decimal(u8, u8),
    Money(u8),
    Date,
    Time(u8),
    DateTime,
    LegacyDateTime(u8),
    DateTime2(u8),
    DateTimeOffset(u8),
    Guid,
}

/// Encode a Unicode ROW value from raw UTF-16, including isolated surrogates.
/// `Text` is this server's NVARCHAR(MAX) wire descriptor. Validate the complete
/// value before appending so a rejected value leaves the output unchanged.
pub fn unicode_value(out: &mut Vec<u8>, kind: &Type, value: Option<&[u16]>) -> Result<()> {
    let width = match kind {
        Type::Text => None,
        Type::Nvarchar(width) | Type::Nchar(width) => {
            ensure!((1..=4000).contains(width), "invalid Unicode result width");
            Some(usize::from(*width))
        }
        _ => bail!("Unicode value requires a Unicode result descriptor"),
    };
    let Some(units) = value else {
        if width.is_some() {
            out.extend(u16::MAX.to_le_bytes());
        } else {
            out.extend(u64::MAX.to_le_bytes());
        }
        return Ok(());
    };
    let bytes = units
        .len()
        .checked_mul(2)
        .ok_or_else(|| anyhow::anyhow!("Unicode result length overflow"))?;
    if let Some(width) = width {
        ensure!(
            units.len() <= width,
            "NVARCHAR value exceeds declared width"
        );
        ensure!(
            !matches!(kind, Type::Nchar(_)) || units.len() == width,
            "NCHAR value does not match declared width"
        );
        out.extend((bytes as u16).to_le_bytes());
    } else {
        ensure!(
            bytes <= i32::MAX as usize,
            "NVARCHAR(MAX) value exceeds SQL storage limit"
        );
        out.extend((bytes as u64).to_le_bytes());
        if bytes != 0 {
            out.extend((bytes as u32).to_le_bytes());
        }
    }
    out.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    if width.is_none() {
        out.extend(0u32.to_le_bytes());
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Column {
    pub collation: Option<collation::Collation>,
    pub properties: msduck_core::result::Properties,
    pub name: String,
    pub kind: Type,
}
impl Column {
    /// Fixed scalar TYPE_INFO is valid only with an explicit NOT NULL proof.
    /// Row encoders must use the same decision and omit the length prefix.
    pub fn fixed_scalar_type(&self) -> Option<u8> {
        if self.properties.nullable != Some(false) {
            return None;
        }
        Some(match self.kind {
            Type::LegacyDateTime(8) => 0x3d,
            Type::LegacyDateTime(4) => 0x3a,
            Type::Int(1) => 0x30,
            Type::Int(2) => 0x34,
            Type::Int(4) => 0x38,
            Type::Int(8) => 0x7f,
            Type::Bit => 0x32,
            Type::Float(4) => 0x3b,
            Type::Float(8) => 0x3e,
            Type::Money(4) => 0x7a,
            Type::Money(8) => 0x3c,
            _ => return None,
        })
    }
}
pub fn metadata(out: &mut Vec<u8>, columns: &[Column]) -> Result<()> {
    ensure!(columns.len() < 65535, "too many columns");
    out.push(0x81);
    out.extend((columns.len() as u16).to_le_bytes());
    for col in columns {
        out.extend(0u32.to_le_bytes());
        use msduck_core::result::Origin;
        let origin = match col.properties.origin {
            Origin::Expression => 0x20,
            Origin::Stored => 0x08,
            Origin::Identity => 0x10,
            Origin::Derived | Origin::Unknown => 0,
        };
        // COLLATION packs IgnoreCase at bit 20; fCaseSen is its inverse,
        // and applies only to character columns.
        let case_sensitive = matches!(
            col.kind,
            Type::Text | Type::Nvarchar(_) | Type::Nchar(_) | Type::Varchar(_) | Type::Char(_)
        ) && col.collation.unwrap_or_default().bytes()[2] & 0x10 == 0;
        let flags = origin
            | u16::from(col.properties.nullable != Some(false))
            | (u16::from(case_sensitive) << 1);
        out.extend(flags.to_le_bytes());
        if let Some(token) = col.fixed_scalar_type() {
            out.push(token);
        } else {
            match col.kind {
                Type::Variant => {
                    out.push(0x62);
                    out.extend(8009u32.to_le_bytes());
                }
                Type::Int(n) => out.extend([0x26, n]),
                Type::Bit => out.extend([0x68, 1]),
                Type::Float(n) => out.extend([0x6d, n]),
                Type::Text => {
                    out.extend([0xe7, 255, 255]);
                    out.extend(col.collation.unwrap_or_default().bytes());
                }
                Type::Nvarchar(width) | Type::Nchar(width) => {
                    ensure!((1..=4000).contains(&width), "invalid NVARCHAR width");
                    out.push(if matches!(col.kind, Type::Nchar(_)) {
                        0xef
                    } else {
                        0xe7
                    });
                    out.extend((width * 2).to_le_bytes());
                    out.extend(col.collation.unwrap_or_default().bytes());
                }
                Type::Varchar(width) | Type::Char(width) => {
                    ensure!(
                        width <= 8000 || matches!(col.kind, Type::Varchar(u16::MAX)),
                        "invalid VARCHAR width"
                    );
                    out.push(if matches!(col.kind, Type::Char(_)) {
                        0xaf
                    } else {
                        0xa7
                    });
                    out.extend(width.to_le_bytes());
                    out.extend(col.collation.unwrap_or_default().bytes());
                }
                Type::Binary => out.extend([0xa5, 255, 255]),
                Type::Varbinary(width) | Type::FixedBinary(width) => {
                    ensure!((1..=8000).contains(&width), "invalid binary width");
                    out.push(if matches!(col.kind, Type::FixedBinary(_)) {
                        0xad
                    } else {
                        0xa5
                    });
                    out.extend(width.to_le_bytes());
                }
                Type::Decimal(p, s) => {
                    ensure!((1..=38).contains(&p) && s <= p, "invalid decimal metadata");
                    out.extend([0x6a, DECIMAL_RESULT_MAX_LENGTH, p, s]);
                }
                Type::Money(width) => {
                    ensure!(matches!(width, 4 | 8), "invalid money metadata");
                    out.extend([0x6e, width]);
                }
                Type::Guid => out.extend([0x24, 16]),
                Type::Date => out.push(0x28),
                Type::Time(scale) => {
                    ensure!(scale <= 7, "invalid TIME scale");
                    out.extend([0x29, scale]);
                }
                Type::DateTime => out.extend([0x2a, 7]),
                Type::LegacyDateTime(width) => {
                    ensure!(matches!(width, 4 | 8), "invalid legacy datetime width");
                    out.extend([0x6f, width]);
                }
                Type::DateTimeOffset(scale) => {
                    ensure!(scale <= 7, "invalid DATETIMEOFFSET scale");
                    out.extend([0x2b, scale]);
                }
                Type::DateTime2(scale) => {
                    ensure!(scale <= 7, "invalid DATETIME2 scale");
                    out.extend([0x2a, scale]);
                }
            }
        }
        btext(out, &col.name);
    }
    Ok(())
}

/// DATETIME/SMALLDATETIME fixed or nullable payload. Round to 1/300 second,
/// then to minutes for SMALLDATETIME. Input is an
/// explicit Unix timestamp; encoding performs no clock or environment access.
pub fn legacy_datetime(out: &mut Vec<u8>, width: u8, unix_nanos: i128, fixed: bool) -> Result<()> {
    ensure!(matches!(width, 4 | 8), "invalid legacy datetime width");
    const DAY: i128 = 86_400_000_000_000;
    let mut days = unix_nanos.div_euclid(DAY) + 25_567;
    let mut ticks = (unix_nanos.rem_euclid(DAY) * 3 + 5_000_000) / 10_000_000;
    if ticks == 25_920_000 {
        days += 1;
        ticks = 0;
    }
    if width == 4 {
        let mut minutes = (ticks + 9_000) / 18_000;
        if minutes == 1_440 {
            days += 1;
            minutes = 0;
        }
        ensure!(
            (0..=65_535).contains(&days),
            "SMALLDATETIME result out of range"
        );
        if !fixed {
            out.push(4);
        }
        out.extend((days as u16).to_le_bytes());
        out.extend((minutes as u16).to_le_bytes());
        return Ok(());
    }
    ensure!(
        (-53_690..=2_958_463).contains(&days),
        "DATETIME result out of range"
    );
    if !fixed {
        out.push(8);
    }
    out.extend((days as i32).to_le_bytes());
    out.extend((ticks as u32).to_le_bytes());
    Ok(())
}

/// MONEYNTYPE row value from an exact coefficient in units of 0.0001.
/// Validate before appending; MONEY uses high-word-first, with each word LE.
pub fn money(out: &mut Vec<u8>, width: u8, coefficient: Option<i128>) -> Result<()> {
    money_value(out, width, coefficient, true)
}
/// Fixed MONEY/SMALLMONEY payload, with no nullable length byte.
pub fn fixed_money(out: &mut Vec<u8>, width: u8, coefficient: i128) -> Result<()> {
    money_value(out, width, Some(coefficient), false)
}
fn money_value(
    out: &mut Vec<u8>,
    width: u8,
    coefficient: Option<i128>,
    nullable: bool,
) -> Result<()> {
    use msduck_core::money::MoneyType;
    let kind = match width {
        4 => MoneyType::SmallMoney,
        8 => MoneyType::Money,
        _ => bail!("invalid money width"),
    };
    let Some(coefficient) = coefficient else {
        out.push(0);
        return Ok(());
    };
    kind.check_scaled(coefficient)?;
    if nullable {
        out.push(width);
    }
    if width == 4 {
        out.extend((coefficient as i32).to_le_bytes());
    } else {
        out.extend(((coefficient >> 32) as i32).to_le_bytes());
        out.extend((coefficient as u32).to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn money_wire_vectors_preserve_words_signs_and_bounds() {
        for (width, coefficient, expected) in [
            (8, 1_234_567, vec![8, 0, 0, 0, 0, 0x87, 0xd6, 0x12, 0]),
            (
                8,
                -10_000,
                vec![8, 255, 255, 255, 255, 0xf0, 0xd8, 255, 255],
            ),
            (8, 0x0102_0304_0506_0708, vec![8, 4, 3, 2, 1, 8, 7, 6, 5]),
            (8, i64::MIN as i128, vec![8, 0, 0, 0, 128, 0, 0, 0, 0]),
            (
                8,
                i64::MAX as i128,
                vec![8, 255, 255, 255, 127, 255, 255, 255, 255],
            ),
            (4, 214_748, vec![4, 0xdc, 0x46, 3, 0]),
            (4, i32::MIN as i128, vec![4, 0, 0, 0, 128]),
            (4, i32::MAX as i128, vec![4, 255, 255, 255, 127]),
        ] {
            let mut out = vec![];
            money(&mut out, width, Some(coefficient)).unwrap();
            assert_eq!(out, expected);
        }
        for width in [4, 8] {
            let mut out = vec![];
            money(&mut out, width, None).unwrap();
            assert_eq!(out, [0]);
        }
        for (width, coefficient) in [(4, i32::MAX as i128 + 1), (8, i64::MIN as i128 - 1), (3, 0)] {
            let mut out = vec![0xaa];
            assert!(money(&mut out, width, Some(coefficient)).is_err());
            assert_eq!(out, [0xaa]);
        }
        for width in [4, 8] {
            let mut out = vec![];
            metadata(
                &mut out,
                &[Column {
                    collation: None,
                    properties: Default::default(),
                    name: "m".into(),
                    kind: Type::Money(width),
                }],
            )
            .unwrap();
            assert_eq!(out, [0x81, 1, 0, 0, 0, 0, 0, 1, 0, 0x6e, width, 1, b'm', 0]);
        }
    }
    #[test]
    fn typed_syntax_diagnostic_keeps_severity_and_state() {
        let mut out = vec![];
        sql_error(
            &mut out,
            &msduck_core::diagnostic::SqlError::syntax(4113, 6, "invalid window"),
        );
        assert_eq!(&out[3..9], &[0x11, 0x10, 0, 0, 6, 15]);
    }
    #[test]
    fn typed_runtime_error_vector_preserves_identity_and_utf16() {
        let mut bytes = Vec::new();
        sql_error(
            &mut bytes,
            &msduck_core::diagnostic::SqlError::new(51000, 7, "x🦆"),
        );
        assert_eq!(
            bytes,
            [
                0xaa, 32, 0, 0x38, 0xc7, 0, 0, 7, 16, 3, 0, 0x78, 0, 0x3e, 0xd8, 0x86, 0xdd, 6,
                b'm', 0, b's', 0, b'd', 0, b'u', 0, b'c', 0, b'k', 0, 0, 1, 0, 0, 0,
            ]
        );
    }
    #[test]
    fn diagnostic_units_preserve_split_surrogates_and_information_attributes() {
        // SQL Server's %.1s formats the duck emoji as just this high surrogate.
        // Message length counts UTF-16 units, not Unicode scalar values.
        let mut bytes = Vec::new();
        diagnostic_utf16(
            &mut bytes,
            DiagnosticKind::Information,
            9,
            7,
            50000,
            &[0xd83e],
        );
        assert_eq!(
            bytes,
            [
                0xab, 28, 0, 0x50, 0xc3, 0, 0, 7, 9, 1, 0, 0x3e, 0xd8, 6, b'm', 0, b's', 0, b'd',
                0, b'u', 0, b'c', 0, b'k', 0, 0, 1, 0, 0, 0,
            ]
        );
        bytes.clear();
        diagnostic_utf16(&mut bytes, DiagnosticKind::Error, 16, 255, 50000, &[0xd83e]);
        assert_eq!((bytes[0], bytes[7], bytes[8]), (0xaa, 255, 16));
        assert_eq!(&bytes[9..13], &[1, 0, 0x3e, 0xd8]);

        bytes.clear();
        diagnostic_utf16(
            &mut bytes,
            DiagnosticKind::Information,
            0,
            1,
            50000,
            &vec![0xdc00; 20000],
        );
        assert_eq!(u16::from_le_bytes([bytes[9], bytes[10]]), 16000);
        assert_eq!(
            usize::from(u16::from_le_bytes([bytes[1], bytes[2]])),
            bytes.len() - 3
        );
        assert!(
            bytes[11..32011]
                .chunks_exact(2)
                .all(|unit| unit == [0, 0xdc])
        );
    }
    #[test]
    fn attention_vector() {
        let mut b = vec![];
        done(&mut b, 0xfd, 0x20, 0, 0);
        assert_eq!(b, [0xfd, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    #[test]
    fn reject_header_overrun() {
        assert!(batch_body(&[10, 0, 0, 0, 0, 0, 0, 0, 2, 0]).is_err());
    }
}

#[cfg(test)]
mod reference_vectors {
    use super::*;
    fn hex(source: &str) -> Vec<u8> {
        source
            .split_whitespace()
            .map(|v| u8::from_str_radix(v, 16).unwrap())
            .collect()
    }
    #[test]
    fn prelogin_reference_vector_and_encryption_policy() {
        // Adapted from mssqlite prelogin.test.ts / MS-TDS example 4.1.
        let mut request = hex(
            "00 00 1A 00 06 01 00 20 00 01 02 00 21 00 01 03 00 22 00 04 04 00 26 00 01 FF 09 00 00 00 00 00 01 00 B8 0D 00 00 01",
        );
        assert!(prelogin(&request).is_err()); // Client requires encryption.
        request[32] = 0;
        assert_eq!(
            prelogin(&request).unwrap(),
            hex("00 00 10 00 06 01 00 16 00 01 04 00 17 00 01 FF 10 00 00 00 00 00 02 00")
        );
    }
    #[test]
    fn encryption_matrix_requires_tls_for_every_capable_client() {
        let mut request = hex("00 00 0B 00 06 01 00 11 00 01 FF 10 00 00 00 00 00 00");
        use EncryptionResult::*;
        for (policy, expected) in [
            (
                EncryptionPolicy::Unsupported,
                [(2, Plaintext), (2, Reject), (2, Plaintext), (2, Reject)],
            ),
            (
                EncryptionPolicy::Required,
                [(3, Tls), (1, Tls), (3, Reject), (1, Tls)],
            ),
        ] {
            for (client, expected) in expected.into_iter().enumerate() {
                request[17] = client as u8;
                let actual = prelogin_with_policy(&request, policy).unwrap();
                assert_eq!((actual.payload[22], actual.encryption), expected);
            }
        }
        for invalid in [4, 0x20, 0x80, 0x81, 0x82, 0xff] {
            request[17] = invalid;
            assert!(prelogin_with_policy(&request, EncryptionPolicy::Required).is_err());
        }
        assert!(prelogin(&[0xff]).is_err());
        request[0] = 4;
        assert!(prelogin(&request).is_err());
    }
    #[test]
    fn login_credentials_decode_scrambling_and_reject_invalid_ranges() {
        let mut request = vec![0; 94];
        request[4..8].copy_from_slice(&0x74000004u32.to_le_bytes());
        request[8..12].copy_from_slice(&4096u32.to_le_bytes());
        request[36..38].copy_from_slice(&94u16.to_le_bytes());
        request[40..42].copy_from_slice(&94u16.to_le_bytes());
        request[42..44].copy_from_slice(&2u16.to_le_bytes());
        request[44..46].copy_from_slice(&98u16.to_le_bytes());
        request[46..48].copy_from_slice(&9u16.to_le_bytes());
        request.extend(text("sa"));
        // Independent Node/mssqlite scramble vector for p@ssw0rd!.
        request.extend(hex("a2 a5 a1 a5 92 a5 92 a5 d2 a5 a6 a5 82 a5 e3 a5 b7 a5"));
        let length = request.len() as u32;
        request[..4].copy_from_slice(&length.to_le_bytes());
        let decoded = login(&request).unwrap();
        assert_eq!(decoded.user_name, "sa");
        assert_eq!(decoded.password.as_str(), "p@ssw0rd!");
        for (position, bytes) in [(44, [93, 0]), (46, [129, 0]), (40, [255, 255])] {
            let mut invalid = request.clone();
            invalid[position..position + 2].copy_from_slice(&bytes);
            assert!(login(&invalid).is_err());
        }
        let mut invalid = request.clone();
        invalid[25] |= 0x80;
        assert!(login(&invalid).is_err());
        invalid = request.clone();
        invalid[27] |= 1;
        assert!(login(&invalid).is_err());
        // A lone UTF-16 surrogate cannot become a silently replaced credential.
        request[94..96].copy_from_slice(&0xd800u16.to_le_bytes());
        assert!(login(&request).is_err());
    }
    #[test]
    fn batch_reference_header() {
        // MS-TDS ALL_HEADERS vector reused by mssqlite requests.test.ts.
        let mut payload = hex("16 00 00 00 12 00 00 00 02 00 00 00 00 00 00 00 00 01 00 00 00 00");
        payload.extend(text("select 1"));
        assert_eq!(
            decode_text(batch_body(&payload).unwrap()).unwrap(),
            "select 1"
        );
    }
}

pub fn batch_body(data: &[u8]) -> Result<&[u8]> {
    Ok(request_headers(data)?.0)
}

pub fn transaction_env(out: &mut Vec<u8>, kind: u8, descriptor: u64) {
    let mut body = vec![kind];
    if kind == 8 {
        body.push(8);
        body.extend(descriptor.to_le_bytes());
        body.push(0);
    } else {
        body.extend([0, 8]);
        body.extend(descriptor.to_le_bytes());
    }
    token(out, 0xe3, &body);
}

#[derive(Debug, PartialEq)]
pub struct BeginTransaction {
    pub isolation: u8,
    pub name: String,
}
#[derive(Debug, PartialEq)]
pub enum TransactionRequest {
    Begin(BeginTransaction),
    Commit {
        restart: Option<BeginTransaction>,
    },
    Rollback {
        name: String,
        restart: Option<BeginTransaction>,
    },
    Save {
        name: String,
    },
}
pub fn transaction_request(data: &[u8]) -> Result<TransactionRequest> {
    fn name(c: &mut Cursor<'_>) -> Result<String> {
        let length = c.u8()? as usize;
        ensure!(length <= 64, "transaction name exceeds 32 UTF-16 units");
        decode_text(c.take(length)?)
    }
    fn begin(c: &mut Cursor<'_>) -> Result<BeginTransaction> {
        let isolation = c.u8()?;
        ensure!(isolation <= 5, "invalid transaction isolation level");
        Ok(BeginTransaction {
            isolation,
            name: name(c)?,
        })
    }
    let mut c = Cursor::new(batch_body(data)?);
    let operation = c.u16()?;
    let request = match operation {
        5 => TransactionRequest::Begin(begin(&mut c)?),
        7 | 8 => {
            let name = name(&mut c)?;
            let flags = c.u8()?;
            ensure!(flags & !1 == 0, "invalid transaction flags");
            let restart = if flags == 1 {
                Some(begin(&mut c)?)
            } else {
                None
            };
            if operation == 7 {
                TransactionRequest::Commit { restart }
            } else {
                TransactionRequest::Rollback { name, restart }
            }
        }
        9 => {
            let name = name(&mut c)?;
            ensure!(!name.is_empty(), "savepoint name must not be empty");
            TransactionRequest::Save { name }
        }
        _ => bail!("unsupported transaction manager operation {operation}"),
    };
    ensure!(c.remaining() == 0, "trailing transaction manager bytes");
    Ok(request)
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    fn request(body: &[u8]) -> Vec<u8> {
        let mut data = vec![22, 0, 0, 0, 18, 0, 0, 0, 2, 0];
        data.extend(42u64.to_le_bytes());
        data.extend(1u32.to_le_bytes());
        data.extend(body);
        data
    }
    #[test]
    fn transaction_requests_and_restart_decode() {
        let data = request(&[5, 0, 2, 2, b'x', 0]);
        assert_eq!(request_headers(&data).unwrap().1, Some(42));
        assert_eq!(
            transaction_request(&data).unwrap(),
            TransactionRequest::Begin(BeginTransaction {
                isolation: 2,
                name: "x".into()
            })
        );
        assert_eq!(
            transaction_request(&request(&[7, 0, 0, 1, 5, 0])).unwrap(),
            TransactionRequest::Commit {
                restart: Some(BeginTransaction {
                    isolation: 5,
                    name: String::new()
                })
            }
        );
        assert_eq!(
            transaction_request(&request(&[8, 0, 2, b'x', 0, 0])).unwrap(),
            TransactionRequest::Rollback {
                name: "x".into(),
                restart: None
            }
        );
    }
    #[test]
    fn malformed_transactions_are_rejected_before_execution() {
        for body in [
            &[5, 0][..],
            &[5, 0, 6, 0],
            &[5, 0, 0, 1, 65],
            &[7, 0, 0],
            &[7, 0, 0, 2],
            &[7, 0, 0, 1, 0],
            &[8, 0, 0, 0, 1],
            &[9, 0, 0],
        ] {
            assert!(transaction_request(&request(body)).is_err(), "{body:?}");
        }
        let mut duplicate = request(&[]);
        duplicate[..4].copy_from_slice(&40u32.to_le_bytes());
        duplicate.extend_from_within(4..22);
        assert!(request_headers(&duplicate).is_err());
    }
    #[test]
    fn transaction_environment_vectors() {
        let mut bytes = Vec::new();
        transaction_env(&mut bytes, 8, 1);
        assert_eq!(bytes, [0xe3, 11, 0, 8, 8, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        bytes.clear();
        transaction_env(&mut bytes, 9, 1);
        assert_eq!(bytes, [0xe3, 11, 0, 9, 0, 8, 1, 0, 0, 0, 0, 0, 0, 0]);
        bytes.clear();
        transaction_env(&mut bytes, 10, 1);
        assert_eq!(bytes, [0xe3, 11, 0, 10, 0, 8, 1, 0, 0, 0, 0, 0, 0, 0]);
    }
}

/// RETURNVALUE for the integer OUTPUT handle returned by preparation RPCs.
pub fn return_handle(out: &mut Vec<u8>, name: &str, value: i32) {
    out.push(0xac);
    out.extend(0u16.to_le_bytes());
    btext(out, name.trim_start_matches('@'));
    out.push(1);
    out.extend(0u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend([0x26, 4, 4]);
    out.extend(value.to_le_bytes());
}

/// SQL Server advertises full coefficient capacity for result decimals.
pub const DECIMAL_RESULT_MAX_LENGTH: u8 = 17;

/// Actual result payload: sign plus the smallest whole 32-bit magnitude groups.
/// The caller must first validate the coefficient against its declared precision.
pub fn decimal_value_length(magnitude: u128) -> u8 {
    if magnitude <= u32::MAX as u128 {
        5
    } else if magnitude <= u64::MAX as u128 {
        9
    } else if magnitude >> 96 == 0 {
        13
    } else {
        17
    }
}

/// Precision-based capacity used by compact client RPC declarations.
pub fn decimal_length(precision: u8) -> u8 {
    match precision {
        0..=9 => 5,
        10..=19 => 9,
        20..=28 => 13,
        _ => 17,
    }
}

// Character encoding is shared with SQL conversion rules.
pub use msduck_core::encoding::{decode_cp1252, encode_cp1252};

#[cfg(test)]
mod result_property_tests {
    use super::*;
    use msduck_core::result::{Origin, Properties};
    #[test]
    fn fixed_scalar_metadata_omits_length_and_nullable_metadata_keeps_it() {
        for (kind, token, width, nullable_token) in [
            (Type::Int(1), 0x30, 1, 0x26),
            (Type::Int(2), 0x34, 2, 0x26),
            (Type::Int(4), 0x38, 4, 0x26),
            (Type::Int(8), 0x7f, 8, 0x26),
            (Type::Bit, 0x32, 1, 0x68),
            (Type::Float(4), 0x3b, 4, 0x6d),
            (Type::Float(8), 0x3e, 8, 0x6d),
            (Type::Money(4), 0x7a, 4, 0x6e),
            (Type::Money(8), 0x3c, 8, 0x6e),
        ] {
            for nullable in [Some(false), Some(true), None] {
                let column = Column {
                    collation: None,
                    name: "x".into(),
                    kind: kind.clone(),
                    properties: Properties {
                        nullable,
                        origin: Origin::Stored,
                    },
                };
                let mut bytes = Vec::new();
                metadata(&mut bytes, &[column]).unwrap();
                let expected = if nullable == Some(false) {
                    vec![token, 1, b'x', 0]
                } else {
                    vec![nullable_token, width, 1, b'x', 0]
                };
                assert_eq!(&bytes[9..], expected);
            }
        }
        for (width, coefficient, bytes) in [
            (4, -1, vec![255; 4]),
            (8, 0x0102030405060708, vec![4, 3, 2, 1, 8, 7, 6, 5]),
        ] {
            let mut out = vec![];
            fixed_money(&mut out, width, coefficient).unwrap();
            assert_eq!(out, bytes);
        }
        let mut out = vec![0xaa];
        assert!(fixed_money(&mut out, 4, i32::MAX as i128 + 1).is_err());
        assert_eq!(out, [0xaa]);
    }
    #[test]
    fn column_flags_encode_declared_nullability_and_origin() {
        for (origin, nullable, expected) in [
            (Origin::Expression, Some(false), 32),
            (Origin::Expression, Some(true), 33),
            (Origin::Stored, Some(false), 8),
            (Origin::Stored, Some(true), 9),
            (Origin::Identity, Some(false), 16),
            (Origin::Identity, Some(true), 17),
            (Origin::Derived, Some(false), 0),
            (Origin::Derived, Some(true), 1),
            (Origin::Unknown, None, 1),
        ] {
            let mut out = Vec::new();
            metadata(
                &mut out,
                &[Column {
                    collation: None,
                    name: "x".into(),
                    kind: Type::Int(4),
                    properties: Properties { origin, nullable },
                }],
            )
            .unwrap();
            assert_eq!(u16::from_le_bytes([out[7], out[8]]), expected);
            if nullable == Some(false) {
                assert_eq!(out[9], 0x38);
            } else {
                assert_eq!(&out[9..11], &[0x26, 4]);
            }
        }
    }
    #[test]
    fn case_sensitivity_flag_follows_character_collation_only() {
        for (name, sensitive) in [
            ("SQL_Latin1_General_CP1_CI_AS", false),
            ("Latin1_General_100_CI_AS", false),
            ("Latin1_General_100_CS_AS", true),
            ("Latin1_General_100_CI_AI", false),
            ("Latin1_General_100_CS_AI", true),
            ("Latin1_General_100_BIN2", true),
        ] {
            for kind in [
                Type::Text,
                Type::Nvarchar(1),
                Type::Nchar(1),
                Type::Varchar(1),
                Type::Char(1),
                Type::Int(4),
            ] {
                let character = !matches!(kind, Type::Int(_));
                let mut out = Vec::new();
                metadata(
                    &mut out,
                    &[Column {
                        name: "x".into(),
                        kind,
                        collation: collation::Collation::for_name(name),
                        properties: Properties::expression(true),
                    }],
                )
                .unwrap();
                assert_eq!(
                    u16::from_le_bytes([out[7], out[8]]),
                    if sensitive && character { 35 } else { 33 },
                    "{name}"
                );
            }
        }
    }
}

#[cfg(test)]
mod decimal_result_tests {
    use super::*;
    #[test]
    fn decimal_metadata_capacity_is_independent_of_value_width() {
        for precision in [1, 9, 10, 19, 20, 28, 29, 38] {
            let mut bytes = Vec::new();
            metadata(
                &mut bytes,
                &[Column {
                    collation: None,
                    name: "d".into(),
                    kind: Type::Decimal(precision, 0),
                    properties: msduck_core::result::Properties::expression(true),
                }],
            )
            .unwrap();
            assert_eq!(&bytes[9..], &[0x6a, 17, precision, 0, 1, b'd', 0]);
        }
        for (magnitude, width) in [
            (0, 5),
            (u32::MAX as u128, 5),
            (1u128 << 32, 9),
            (u64::MAX as u128, 9),
            (1u128 << 64, 13),
            ((1u128 << 96) - 1, 13),
            (1u128 << 96, 17),
            (10u128.pow(38) - 1, 17),
        ] {
            assert_eq!(decimal_value_length(magnitude), width);
        }
    }
}

#[cfg(test)]
mod unicode_value_tests {
    use super::*;

    #[test]
    fn bounded_unicode_vectors_preserve_surrogates_null_and_padding() {
        let mut out = vec![];
        unicode_value(&mut out, &Type::Nvarchar(1), Some(&[0xd83e])).unwrap();
        unicode_value(&mut out, &Type::Nvarchar(1), Some(&[0xdd86])).unwrap();
        unicode_value(&mut out, &Type::Nvarchar(1), Some(&[])).unwrap();
        unicode_value(&mut out, &Type::Nvarchar(1), None).unwrap();
        unicode_value(&mut out, &Type::Nchar(2), Some(&[65, 32])).unwrap();
        assert_eq!(
            out,
            [
                2, 0, 0x3e, 0xd8, 2, 0, 0x86, 0xdd, 0, 0, 255, 255, 4, 0, 65, 0, 32, 0
            ]
        );
    }

    #[test]
    fn max_unicode_vectors_distinguish_empty_null_and_surrogate_values() {
        let mut out = vec![];
        unicode_value(&mut out, &Type::Text, Some(&[0xd83e])).unwrap();
        assert_eq!(
            out,
            [2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0x3e, 0xd8, 0, 0, 0, 0]
        );
        out.clear();
        unicode_value(&mut out, &Type::Text, Some(&[])).unwrap();
        assert_eq!(out, [0; 12]);
        out.clear();
        unicode_value(&mut out, &Type::Text, None).unwrap();
        assert_eq!(out, [255; 8]);
    }

    #[test]
    fn invalid_unicode_values_leave_output_untouched() {
        for (kind, value) in [
            (Type::Nvarchar(0), None),
            (Type::Nvarchar(4001), None),
            (Type::Nvarchar(1), Some(&[0xd83e, 0xdd86][..])),
            (Type::Nchar(2), Some(&[65][..])),
            (Type::Binary, Some(&[65][..])),
        ] {
            let mut out = vec![0xd1];
            assert!(unicode_value(&mut out, &kind, value).is_err());
            assert_eq!(out, [0xd1]);
        }
    }
}

#[cfg(test)]
mod legacy_datetime_tests {
    use super::*;

    #[test]
    fn datetime_metadata_distinguishes_fixed_and_nullable_forms() {
        for (nullable, token) in [(false, vec![0x3d]), (true, vec![0x6f, 8])] {
            let column = Column {
                name: "d".into(),
                kind: Type::LegacyDateTime(8),
                collation: None,
                properties: msduck_core::result::Properties {
                    nullable: Some(nullable),
                    origin: msduck_core::result::Origin::Stored,
                },
            };
            let mut actual = vec![];
            metadata(&mut actual, &[column]).unwrap();
            let mut expected = vec![0x81, 1, 0, 0, 0, 0, 0, if nullable { 9 } else { 8 }, 0];
            expected.extend(token);
            expected.extend([1, b'd', 0]);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn smalldatetime_payload_rounds_minutes_and_checks_unsigned_day_range() {
        let day = 86_400_000_000_000i128;
        for (nanos, days, minutes) in [
            (0, 25_567u16, 0u16),
            (29_998_000_000, 25_567, 0),
            (29_999_000_000, 25_567, 1),
            (day - 30_001_000_000, 25_568, 0),
            (-25_567 * day, 0, 0),
            ((65_535 - 25_567) * day, 65_535, 0),
        ] {
            for fixed in [false, true] {
                let mut actual = vec![];
                legacy_datetime(&mut actual, 4, nanos, fixed).unwrap();
                let mut expected = if fixed { vec![] } else { vec![4] };
                expected.extend(days.to_le_bytes());
                expected.extend(minutes.to_le_bytes());
                assert_eq!(actual, expected);
            }
        }
        for (width, nanos) in [(4, -25_568 * day), (4, (65_536 - 25_567) * day), (3, 0)] {
            let mut out = vec![42];
            assert!(legacy_datetime(&mut out, width, nanos, false).is_err());
            assert_eq!(out, [42]);
        }
        for nullable in [false, true] {
            let column = Column {
                name: "s".into(),
                kind: Type::LegacyDateTime(4),
                collation: None,
                properties: msduck_core::result::Properties {
                    nullable: Some(nullable),
                    origin: msduck_core::result::Origin::Stored,
                },
            };
            let mut actual = vec![];
            metadata(&mut actual, &[column]).unwrap();
            let mut expected = vec![0x81, 1, 0, 0, 0, 0, 0, if nullable { 9 } else { 8 }, 0];
            expected.extend(if nullable { vec![0x6f, 4] } else { vec![0x3a] });
            expected.extend([1, b's', 0]);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn datetime_payload_rounds_signed_timestamps_and_validates_before_writing() {
        let day = 86_400_000_000_000i128;
        for (nanos, days, ticks) in [
            (0, 25_567i32, 0u32),
            (-1, 25_567, 0),
            (-5_000_000, 25_566, 25_919_999),
            (1_666_666, 25_567, 0),
            (1_666_667, 25_567, 1),
            (day - 1, 25_568, 0),
            ((-53_690 - 25_567) * day, -53_690, 0),
            (
                (2_958_463 - 25_567) * day + day - 3_333_333,
                2_958_463,
                25_919_999,
            ),
        ] {
            for fixed in [false, true] {
                let mut actual = vec![];
                legacy_datetime(&mut actual, 8, nanos, fixed).unwrap();
                let mut expected = if fixed { vec![] } else { vec![8] };
                expected.extend(days.to_le_bytes());
                expected.extend(ticks.to_le_bytes());
                assert_eq!(actual, expected, "{nanos}, fixed={fixed}");
            }
        }
        for nanos in [
            (-53_691 - 25_567) * day,
            (2_958_464 - 25_567) * day,
            i128::MIN,
            i128::MAX,
        ] {
            let mut out = vec![42];
            assert!(legacy_datetime(&mut out, 8, nanos, false).is_err());
            assert_eq!(out, [42]);
        }
    }
}
