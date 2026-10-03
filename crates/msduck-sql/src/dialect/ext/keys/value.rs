//! Key expressions for DuckDB indexes and SQL Server's display of key values.
//!
//! DuckDB cannot index the STRUCT carriers msduck uses for NVARCHAR/NCHAR,
//! DATETIME2 and DATETIMEOFFSET, and its unique indexes let any number of
//! NULL keys coexist. A keys-managed index therefore indexes expressions:
//!
//! - a tag, `CASE WHEN filter THEN tag END`, that names the index in DuckDB's
//!   duplicate-key messages and leaves rows outside a filter unchecked (DuckDB
//!   does not compare keys containing NULL);
//! - per column, the comparable value: the equality key of a Unicode
//!   carrier (trailing spaces ignored, case-folded under a case-insensitive
//!   collation such as the database default), right-trimmed (and lowercased)
//!   ANSI text, UTC ticks of a temporal struct, or the value itself;
//! - per nullable column, an `IS NULL` discriminator and the value with NULL
//!   replaced by a typed zero, so that NULL compares equal to NULL (SQL
//!   Server's single-NULL rule) and stays distinct from the zero value.
//!
//! Text values are hexadecimal so DuckDB's message stays parseable.
use msduck_core::{datetime2::DateTime2, datetimeoffset::DateTimeOffset};
use sqlparser::ast::Ident;

/// A table column as the catalogs describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub system_type_id: u8,
    /// sys.columns.max_length: bytes, or -1 for MAX types.
    pub max_length: i16,
    pub scale: u8,
    pub nullable: bool,
    /// DuckDB's storage type, as `duckdb_columns().data_type` prints it.
    pub storage: String,
    /// sys.columns.collation_name of a character column.
    pub collation: Option<String>,
}

/// How a column's values are compared in a DuckDB index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Storage {
    /// NVARCHAR/NCHAR carrier.
    Unicode,
    /// VARCHAR/CHAR text.
    Ansi,
    /// BINARY/VARBINARY.
    Binary,
    /// DATETIME2 or DATETIMEOFFSET struct: ticks field name.
    Ticks {
        field: String,
        offset: bool,
    },
    Boolean,
    /// A numeric DuckDB type, by name.
    Numeric(String),
    /// Other scalar types, compared by their canonical text.
    Text,
}

/// SQL Server's error for a column type that cannot be an index key.
pub fn invalid_key(column: &str, table: &str) -> (i32, u8, u8, String) {
    (
        1919,
        1,
        16,
        format!(
            "Column '{column}' in table '{table}' is of a type that is invalid for use as a key column in an index."
        ),
    )
}

const NUMERIC: &[&str] = &[
    "TINYINT",
    "SMALLINT",
    "INTEGER",
    "BIGINT",
    "HUGEINT",
    "UTINYINT",
    "USMALLINT",
    "UINTEGER",
    "UBIGINT",
    "FLOAT",
    "DOUBLE",
];

impl Column {
    /// Whether SQL Server allows this column as an index key: not a MAX,
    /// text, ntext, image or xml column.
    pub fn keyable(&self) -> bool {
        let large = matches!(self.system_type_id, 34 | 35 | 99 | 241);
        let max = matches!(self.system_type_id, 165 | 167 | 231) && self.max_length == -1;
        !large && !max
    }

    pub fn kind(&self) -> Result<Storage, String> {
        let storage = self.storage.as_str();
        if storage.eq_ignore_ascii_case("STRUCT(__msduck_utf16le BLOB)") {
            return Ok(Storage::Unicode);
        }
        if let Some(rest) = storage.strip_prefix("STRUCT(") {
            for (prefix, offset) in [
                ("__msduck_datetime2_", false),
                ("__msduck_datetimeoffset_", true),
            ] {
                if rest.starts_with(prefix) {
                    let field = rest.split(' ').next().unwrap_or_default();
                    return Ok(Storage::Ticks {
                        field: field.to_owned(),
                        offset,
                    });
                }
            }
            return Err(format!(
                "unsupported key column '{}' of storage type {storage}",
                self.name
            ));
        }
        if storage.contains('[') || storage.starts_with("MAP") || storage.starts_with("UNION") {
            return Err(format!(
                "unsupported key column '{}' of storage type {storage}",
                self.name
            ));
        }
        Ok(match storage {
            "VARCHAR" => Storage::Ansi,
            "BLOB" => Storage::Binary,
            "BOOLEAN" => Storage::Boolean,
            _ if NUMERIC.contains(&storage) || storage.starts_with("DECIMAL") => {
                Storage::Numeric(storage.to_owned())
            }
            _ => Storage::Text,
        })
    }

    /// The quoted DuckDB identifier.
    pub fn quoted(&self) -> String {
        Ident::with_quote('"', &self.name).to_string()
    }

    /// Whether the column's values compare without regard to case: the
    /// database default and other case-insensitive collations.
    pub fn case_insensitive(&self) -> bool {
        super::collation::case_insensitive(self.collation.as_deref())
    }

    /// The equality key expression of a Unicode carrier column.
    pub fn unicode_key(&self) -> String {
        let q = self.quoted();
        if self.case_insensitive() {
            folded_unicode_key(&q)
        } else {
            unicode_key(&q)
        }
    }

    /// The key of a constant compared with this Unicode column.
    pub fn unicode_key_of(&self, text: &str) -> String {
        if self.case_insensitive() {
            let folded: String = String::from_utf16_lossy(
                &text
                    .encode_utf16()
                    .filter(|unit| !ignorable(*unit))
                    .map(key_fold)
                    .collect::<Vec<_>>(),
            );
            unicode_key_of(&folded)
        } else {
            unicode_key_of(text)
        }
    }

    /// The equality key expression of an ANSI column.
    pub fn ansi_key(&self) -> String {
        let q = self.quoted();
        if self.case_insensitive() {
            format!("hex(rtrim(lower({q}), ' '))")
        } else {
            format!("hex(rtrim({q}, ' '))")
        }
    }

    /// The comparable value expression and its typed zero.
    fn value(&self) -> Result<(String, String), String> {
        let q = self.quoted();
        Ok(match self.kind()? {
            Storage::Unicode => (self.unicode_key(), "''".into()),
            Storage::Ansi => (self.ansi_key(), "''".into()),
            Storage::Binary => (format!("hex({q})"), "''".into()),
            Storage::Ticks { field, .. } => (format!("struct_extract({q}, '{field}')"), "0".into()),
            Storage::Boolean => (q, "false".into()),
            Storage::Numeric(kind) => (q, format!("CAST(0 AS {kind})")),
            Storage::Text => (format!("CAST({q} AS VARCHAR)"), "''".into()),
        })
    }

    /// The index expressions for this key column.
    pub fn components(&self) -> Result<Vec<String>, String> {
        let (value, zero) = self.value()?;
        Ok(if self.nullable {
            vec![
                format!("({} IS NULL)", self.quoted()),
                format!("coalesce({value}, {zero})"),
            ]
        } else {
            vec![value]
        })
    }

    /// SQL Server's display of a key value. `encoded` values come from a
    /// keys-managed index expression; others are DuckDB's text of the value.
    pub fn display(&self, value: &str, encoded: bool) -> String {
        let kind = self.kind().unwrap_or(Storage::Text);
        let text = match (&kind, encoded) {
            (Storage::Unicode, _) => {
                let bytes = hex(value).unwrap_or_default();
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            (Storage::Ansi, true) => {
                String::from_utf8_lossy(&hex(value).unwrap_or_default()).into()
            }
            (Storage::Binary, true) => return format!("0x{}", value.to_ascii_lowercase()),
            (Storage::Binary, false) => {
                return format!(
                    "0x{}",
                    unescape_blob(value)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
            }
            (Storage::Ticks { offset, .. }, _) => {
                return value
                    .parse::<i64>()
                    .ok()
                    .and_then(|ticks| DateTime2::from_ticks(ticks).ok())
                    .and_then(|time| {
                        if *offset {
                            DateTimeOffset::from_utc(time, 0)
                                .ok()?
                                .format_iso(self.scale)
                                .ok()
                        } else {
                            time.format_iso(self.scale).ok()
                        }
                    })
                    .map(|text| text.replacen('T', " ", 1))
                    .unwrap_or_else(|| value.to_owned());
            }
            (Storage::Boolean, _) => {
                return match value {
                    "true" => "1".into(),
                    "false" => "0".into(),
                    other => other.into(),
                };
            }
            _ => value.to_owned(),
        };
        match self.system_type_id {
            // char/nchar: SQL Server shows the padded value.
            175 => pad(text, self.max_length.max(0) as usize),
            239 => pad(text, (self.max_length.max(0) / 2) as usize),
            // money/smallmoney
            60 | 122 => fraction(&text, 2),
            // datetime and smalldatetime use the legacy style 0 text.
            58 | 61 => legacy(&text).unwrap_or(text),
            // time
            41 => fraction(&text, self.scale as usize),
            _ => text,
        }
    }
}

/// DuckDB's text of a BLOB: printable ASCII as is, other bytes as `\xNN`.
fn unescape_blob(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut result = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'x')
            && let Some(byte) = text
                .get(i + 2..i + 4)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            result.push(byte);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    result
}

/// `yyyy-mm-dd hh:mi:ss[.f]` as CONVERT style 0: `Jan  2 2024  3:04AM`.
fn legacy(text: &str) -> Option<String> {
    let (date, time) = text.split_once(' ')?;
    let mut date = date.split('-');
    let year = date.next()?;
    let month: usize = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    let mut time = time.split(':');
    let hour: u32 = time.next()?.parse().ok()?;
    let minute: u32 = time.next()?.parse().ok()?;
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    Some(format!(
        "{} {day:>2} {year} {:>2}:{minute:02}{}",
        MONTHS.get(month.checked_sub(1)?)?,
        if hour.is_multiple_of(12) {
            12
        } else {
            hour % 12
        },
        if hour < 12 { "AM" } else { "PM" }
    ))
}

fn pad(text: String, width: usize) -> String {
    let len = text.encode_utf16().count();
    if len >= width {
        return text;
    }
    text + &" ".repeat(width - len)
}

/// Exactly `digits` fractional digits (truncating or zero-filling).
fn fraction(text: &str, digits: usize) -> String {
    let (whole, part) = text.split_once('.').unwrap_or((text, ""));
    if digits == 0 {
        return whole.into();
    }
    let mut part: String = part.chars().take(digits).collect();
    while part.len() < digits {
        part.push('0');
    }
    format!("{whole}.{part}")
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The BIN2 equality key of a Unicode carrier: its UTF-16LE units in
/// hexadecimal without trailing spaces (`2000` groups). Only built-in
/// functions, so DuckDB can bind the index while it replays its WAL before
/// msduck registers its own functions.
pub fn unicode_key(carrier: &str) -> String {
    format!("regexp_replace(hex(struct_extract({carrier}, '__msduck_utf16le')), '(2000)+$', '')")
}

/// The UTF-16 blocks whose case pairs keys fold: Basic Latin, Latin-1,
/// Latin Extended-A, basic Greek and basic Cyrillic capitals. Comparisons
/// fold every simple case mapping; outside these blocks a unique key still
/// distinguishes case.
const FOLDED: &[(u16, u16)] = &[
    (0x41, 0x5A),
    (0xC0, 0xDE),
    (0x100, 0x17F),
    (0x391, 0x3AB),
    (0x400, 0x42F),
];

/// The lowercase unit a key folds `unit` to: its simple lowercase mapping
/// within [`FOLDED`] when that keeps the high byte, otherwise `unit`.
pub fn key_fold(unit: u16) -> u16 {
    if !FOLDED
        .iter()
        .any(|(low, high)| (*low..=*high).contains(&unit))
    {
        return unit;
    }
    let Some(character) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    let mut lower = character.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) if (single as u32) >> 8 == u32::from(unit >> 8) => single as u16,
        _ => unit,
    }
}

fn nibble(value: u16) -> char {
    char::from_digit(u32::from(value & 0xF), 16)
        .unwrap()
        .to_ascii_uppercase()
}

/// Whether SQL_Latin1_General_CP1_CI_AS ignores a UTF-16 unit in Unicode
/// comparisons: U+0000, the zero-width joiner, the left-to-right mark,
/// U+FEFF, the noncharacters U+FFFE and U+FFFF, and every surrogate (so a
/// supplementary character equals the empty string). Captured in
/// reference/default-collation.json; other format and control characters
/// are not ignored.
pub fn ignorable(unit: u16) -> bool {
    matches!(unit, 0 | 0x200D | 0x200E | 0xFEFF | 0xFFFE | 0xFFFF)
        || (0xD800..=0xDFFF).contains(&unit)
}

/// The `regexp_replace` rule that deletes [`ignorable`] units from
/// `|`-delimited UTF-16LE hexadecimal units.
pub const IGNORABLE_RULE: (&str, &str) = (
    "\\|(0000|0D20|0E20|FFFE|FEFF|FFFF|[0-9A-F]{2}D[89A-F])\\|",
    "",
);

/// `regexp_replace` rules over `|`-delimited UTF-16LE hexadecimal units
/// (`|XXXX|` each) that apply [`key_fold`]: each rule matches one whole
/// unit, and no rule's output is another rule's input.
pub fn fold_rules() -> Vec<(String, String)> {
    use std::collections::BTreeMap;
    // (high byte, changed nibble) -> unit nibbles that share the change.
    let mut low_nibble: BTreeMap<(u16, u16, u16), Vec<u16>> = BTreeMap::new();
    let mut high_nibble: BTreeMap<(u16, u16, u16), Vec<u16>> = BTreeMap::new();
    let mut whole: Vec<(u16, u16)> = vec![];
    for &(low, high) in FOLDED {
        for unit in low..=high {
            let lower = key_fold(unit);
            if lower == unit {
                continue;
            }
            let (a, b, c) = ((unit >> 4) & 0xF, unit & 0xF, unit >> 8);
            let (a2, b2) = ((lower >> 4) & 0xF, lower & 0xF);
            if b == b2 {
                high_nibble.entry((c, a, a2)).or_default().push(b);
            } else if a == a2 {
                low_nibble.entry((c, b, b2)).or_default().push(a);
            } else {
                whole.push((unit, lower));
            }
        }
    }
    let class = |digits: &[u16]| -> String {
        format!(
            "([{}])",
            digits.iter().map(|d| nibble(*d)).collect::<String>()
        )
    };
    let mut rules = vec![];
    for ((c, a, a2), digits) in high_nibble {
        rules.push((
            format!("\\|{}{}{:02X}", nibble(a), class(&digits), c),
            format!("|{}\\1{:02X}", nibble(a2), c),
        ));
    }
    for ((c, b, b2), digits) in low_nibble {
        rules.push((
            format!("\\|{}{}{:02X}", class(&digits), nibble(b), c),
            format!("|\\1{}{:02X}", nibble(b2), c),
        ));
    }
    for (unit, lower) in whole {
        rules.push((
            format!("\\|{:02X}{:02X}", unit & 0xFF, unit >> 8),
            format!("|{:02X}{:02X}", lower & 0xFF, lower >> 8),
        ));
    }
    rules
}

/// The case-insensitive equality key of a Unicode carrier: its UTF-16LE
/// units in hexadecimal without [`ignorable`] units, with [`key_fold`]
/// applied to each unit and without trailing spaces, using only built-in
/// functions.
pub fn folded_unicode_key(carrier: &str) -> String {
    let mut units = format!(
        "regexp_replace(hex(struct_extract({carrier}, '__msduck_utf16le')), '(....)', '|\\1|', 'g')"
    );
    let (pattern, replacement) = IGNORABLE_RULE;
    units = format!("regexp_replace({units}, '{pattern}', '{replacement}', 'g')");
    for (pattern, replacement) in fold_rules() {
        units = format!("regexp_replace({units}, '{pattern}', '{replacement}', 'g')");
    }
    format!("regexp_replace(replace({units}, '|', ''), '(2000)+$', '')")
}

/// The [`unicode_key`] of a constant.
pub fn unicode_key_of(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = units.iter().rposition(|u| *u != 0x20).map_or(0, |i| i + 1);
    units[..end]
        .iter()
        .flat_map(|u| u.to_le_bytes())
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// Tags start far above ordinary key values, so a tag in a duplicate-key
/// message is not mistaken for a native key value.
pub const TAG_BASE: i64 = 9_000_000_000_000_000_000;

/// The tag expression of a managed index; rows where `filter` is not true
/// get a NULL tag and are not checked.
pub fn guard(tag: i64, filter: Option<&str>) -> String {
    format!(
        "(CASE WHEN {} THEN {} END)",
        filter.unwrap_or("true"),
        TAG_BASE + tag
    )
}

/// The number of index expressions of `columns` after the tag.
pub fn component_count(columns: &[Column]) -> usize {
    columns.iter().map(|c| if c.nullable { 2 } else { 1 }).sum()
}

/// The display values of a duplicate key of a managed index, from the
/// values of its expressions after the tag. `None` when they do not fit.
pub fn managed_values(columns: &[Column], values: &[String]) -> Option<Vec<String>> {
    let mut values = values.iter();
    let mut shown = vec![];
    for column in columns {
        if column.nullable {
            let null = values.next()?;
            let value = values.next()?;
            shown.push(if null == "true" {
                "<NULL>".to_owned()
            } else {
                column.display(value, true)
            });
        } else {
            shown.push(column.display(values.next()?, true));
        }
    }
    values.next().is_none().then_some(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(
        name: &str,
        system_type_id: u8,
        max_length: i16,
        scale: u8,
        nullable: bool,
        storage: &str,
    ) -> Column {
        Column {
            name: name.into(),
            system_type_id,
            max_length,
            scale,
            nullable,
            storage: storage.into(),
            collation: None,
        }
    }

    #[test]
    fn expressions_follow_storage() {
        let mut unicode = column("n", 231, 20, 0, true, "STRUCT(__msduck_utf16le BLOB)");
        unicode.collation = Some("Latin1_General_100_BIN2".into());
        assert_eq!(
            unicode.components().unwrap(),
            [
                "(\"n\" IS NULL)",
                "coalesce(regexp_replace(hex(struct_extract(\"n\", '__msduck_utf16le')), '(2000)+$', ''), '')"
            ]
        );
        let offset = column(
            "o",
            43,
            10,
            7,
            false,
            "STRUCT(__msduck_datetimeoffset_7 BIGINT, __msduck_offset_minutes SMALLINT)",
        );
        assert_eq!(
            offset.components().unwrap(),
            ["struct_extract(\"o\", '__msduck_datetimeoffset_7')"]
        );
        let number = column("d", 106, 9, 2, true, "DECIMAL(9,2)");
        assert_eq!(
            number.components().unwrap()[1],
            "coalesce(\"d\", CAST(0 AS DECIMAL(9,2)))"
        );
        let id = column("a\"b", 56, 4, 0, false, "INTEGER");
        assert_eq!(id.components().unwrap(), ["\"a\"\"b\""]);
        assert!(
            column(
                "v",
                26,
                16,
                0,
                false,
                "STRUCT(__msduck_variant_type UTINYINT, __msduck_variant_integer BIGINT)"
            )
            .components()
            .is_err()
        );
        // The database default folds case in keys.
        unicode.collation = None;
        let folded = &unicode.components().unwrap()[1];
        assert!(
            folded.starts_with("coalesce(regexp_replace(replace(regexp_replace("),
            "{folded}"
        );
        assert!(
            !folded.contains("__msduck_")
                || !folded.replace("__msduck_utf16le", "").contains("__msduck")
        );
        let ansi = column("v", 167, 10, 0, false, "VARCHAR");
        assert_eq!(
            ansi.components().unwrap(),
            ["hex(rtrim(lower(\"v\"), ' '))"]
        );
        let mut sensitive = ansi.clone();
        sensitive.collation = Some("SQL_Latin1_General_CP1_CS_AS".into());
        assert_eq!(sensitive.components().unwrap(), ["hex(rtrim(\"v\", ' '))"]);
        assert!(!column("m", 231, -1, 0, false, "STRUCT(__msduck_utf16le BLOB)").keyable());
        assert!(column("m", 231, 900, 0, false, "STRUCT(__msduck_utf16le BLOB)").keyable());
        assert_eq!(
            guard(17, None),
            "(CASE WHEN true THEN 9000000000000000017 END)"
        );
    }

    #[test]
    fn values_display_like_sql_server() {
        let nchar = column("c", 239, 8, 0, true, "STRUCT(__msduck_utf16le BLOB)");
        assert_eq!(nchar.display("7800", true), "x   ");
        assert_eq!(unicode_key_of("a\u{20ac}  "), "6100AC20");
        let nvarchar = column("c", 231, 20, 0, true, "STRUCT(__msduck_utf16le BLOB)");
        assert_eq!(nvarchar.display("6900740027007300", true), "it's");
        let ansi = column("v", 167, 10, 0, true, "VARCHAR");
        assert_eq!(ansi.display("782C20793A207A", true), "x, y: z");
        assert_eq!(ansi.display("x, y", false), "x, y");
        let fixed = column("v", 175, 4, 0, false, "VARCHAR");
        assert_eq!(fixed.display("61", true), "a   ");
        let bit = column("b", 104, 1, 0, false, "BOOLEAN");
        assert_eq!(bit.display("true", true), "1");
        let money = column("m", 60, 8, 4, false, "DECIMAL(19,4)");
        assert_eq!(money.display("12.5000", true), "12.50");
        let legacy = column("t", 61, 8, 3, false, "TIMESTAMP");
        assert_eq!(
            legacy.display("2024-01-02 03:04:05", true),
            "Jan  2 2024  3:04AM"
        );
        assert_eq!(
            legacy.display("2024-11-12 15:04:05.5", false),
            "Nov 12 2024  3:04PM"
        );
        assert_eq!(
            legacy.display("2024-11-12 00:00:00", false),
            "Nov 12 2024 12:00AM"
        );
        let time = column("t", 41, 5, 7, false, "TIME_NS");
        assert_eq!(time.display("01:02:03.5", true), "01:02:03.5000000");
        let binary = column("b", 165, 4, 0, false, "BLOB");
        assert_eq!(binary.display("01AB", true), "0x01ab");
        assert_eq!(binary.display("\\x01\\xABz", false), "0x01ab7a");
        let stamp = DateTime2::parse_iso("2024-01-02T03:04:05.678")
            .unwrap()
            .ticks();
        let datetime2 = column("d", 42, 7, 3, false, "STRUCT(__msduck_datetime2_3 BIGINT)");
        assert_eq!(
            datetime2.display(&stamp.to_string(), true),
            "2024-01-02 03:04:05.678"
        );
        let offset = column(
            "o",
            43,
            10,
            2,
            false,
            "STRUCT(__msduck_datetimeoffset_2 BIGINT, __msduck_offset_minutes SMALLINT)",
        );
        assert_eq!(
            offset.display(&stamp.to_string(), true),
            "2024-01-02 03:04:05.68 +00:00"
        );
    }

    #[test]
    fn key_folding_rules_match_whole_units() {
        // Simulate the regexp rules over every unit of the folded blocks.
        let rules = fold_rules();
        assert!(!rules.is_empty() && rules.len() < 40, "{}", rules.len());
        for unit in 0u16..0x500 {
            let mut hex = format!("{:02X}{:02X}", unit & 0xFF, unit >> 8);
            for (pattern, replacement) in &rules {
                // `\|`, then four unit characters, one of which may be a
                // captured `([...])` class of nibbles.
                let body = pattern.strip_prefix("\\|").expect(pattern);
                let mut chars = hex.chars();
                let mut matched = true;
                let mut captured = None;
                let mut rest = body.chars();
                while let Some(p) = rest.next() {
                    let c = chars.next().unwrap();
                    if p == '(' {
                        assert_eq!(rest.next(), Some('['), "{pattern}");
                        let class: String = rest.by_ref().take_while(|ch| *ch != ']').collect();
                        assert_eq!(rest.next(), Some(')'), "{pattern}");
                        if class.contains(c) {
                            captured = Some(c);
                        } else {
                            matched = false;
                        }
                    } else if p != c {
                        matched = false;
                    }
                }
                if matched {
                    hex = replacement
                        .strip_prefix('|')
                        .expect(replacement)
                        .replace("\\1", &captured.map(String::from).unwrap_or_default());
                }
            }
            let expected = key_fold(unit);
            assert_eq!(
                hex,
                format!("{:02X}{:02X}", expected & 0xFF, expected >> 8),
                "{unit:04X}"
            );
        }
        assert_eq!(key_fold(u16::from(b'A')), u16::from(b'a'));
        assert_eq!(key_fold(0xC9), 0xE9);
        assert_eq!(key_fold(0x100), 0x101);
        assert_eq!(key_fold(0x410), 0x430);
        assert_eq!(key_fold(0xD7), 0xD7);
        assert_eq!(key_fold(0x178), 0x178);
        assert!(ignorable(0) && ignorable(0xD83E) && ignorable(0xFEFF));
        assert!(!ignorable(0x1) && !ignorable(0xAD) && !ignorable(0x200B) && !ignorable(0xE000));
        let mut default = Column {
            name: "n".into(),
            system_type_id: 231,
            max_length: 20,
            scale: 0,
            nullable: false,
            storage: "STRUCT(__msduck_utf16le BLOB)".into(),
            collation: None,
        };
        assert_eq!(default.unicode_key_of("A\u{0}\u{1F986}B  "), "61006200");
        default.collation = Some("Latin1_General_100_BIN2".into());
        assert_eq!(default.unicode_key_of("A\u{0}B  "), "410000004200");
    }

    #[test]
    fn managed_values_decode_nulls() {
        let columns = [
            column("a", 56, 4, 0, true, "INTEGER"),
            column("b", 231, 20, 0, false, "STRUCT(__msduck_utf16le BLOB)"),
        ];
        assert_eq!(component_count(&columns), 3);
        let values = ["true", "0", "6100"].map(String::from);
        assert_eq!(managed_values(&columns, &values).unwrap(), ["<NULL>", "a"]);
        let values = ["false", "5", "6100"].map(String::from);
        assert_eq!(managed_values(&columns, &values).unwrap(), ["5", "a"]);
        assert!(managed_values(&columns, &values[..2]).is_none());
    }
}
