//! SQL Server SOUNDEX over bytes already converted to the input collation's code page.
//!
//! The caller supplies both the conversion and the database compatibility mode.
//! These rules are verified against `reference/soundex-difference.json` for the
//! default SQL_Latin1_General_CP1_CI_AS collation (Windows-1252). They must not
//! be used for other code pages without a corresponding classification table.

/// SQL Server's two captured SOUNDEX compatibility families.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Database compatibility level 100.
    Legacy,
    /// Database compatibility level 110 or later.
    Current,
}

/// Return four Windows-1252 bytes, with zero padding, for a converted input.
///
/// The caller must first apply SQL Server's VARCHAR best-fit conversion. The
/// return value is bytes because the first character can be a non-ASCII
/// Windows-1252 letter; for example, `0xff` uppercases to `0x9f` (`Ÿ`).
pub fn cp1252(input: &[u8], mode: Mode) -> [u8; 4] {
    let Some(&first) = input.first() else {
        return *b"0000";
    };
    if !is_letter(first) {
        return *b"0000";
    }
    let mut result = [b'0'; 4];
    result[0] = uppercase(first);
    let mut written = 1;
    let mut previous = if mode == Mode::Current {
        code(first)
    } else {
        0
    };
    for &byte in &input[1..] {
        if !is_letter(byte) {
            break;
        }
        let digit = code(byte);
        if digit == 0 {
            // In the current family only uppercase H/W are transparent:
            // equal codes on either side still form one run.
            if mode == Mode::Legacy || !matches!(byte, b'H' | b'W') {
                previous = 0;
            }
            continue;
        }
        if digit != previous {
            result[written] = digit;
            written += 1;
            if written == result.len() {
                break;
            }
        }
        previous = digit;
    }
    result
}

fn is_letter(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | 0xc0..=0xd6 | 0xd8..=0xf6 | 0xf8..=0xff)
}

fn uppercase(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' | 0xe0..=0xf6 | 0xf8..=0xfe => byte - 32,
        0xff => 0x9f,
        _ => byte,
    }
}

fn code(byte: u8) -> u8 {
    match byte.to_ascii_uppercase() {
        b'B' | b'F' | b'P' | b'V' => b'1',
        b'C' | b'G' | b'J' | b'K' | b'Q' | b'S' | b'X' | b'Z' => b'2',
        b'D' | b'T' => b'3',
        b'L' => b'4',
        b'M' | b'N' => b'5',
        b'R' => b'6',
        _ => 0,
    }
}
