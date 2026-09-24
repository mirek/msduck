//! Bounded RAISERROR formatting. Output retains UTF-16, including a surrogate
//! split by string precision. The transport adapter must not round-trip it
//! through a Rust String.
use crate::diagnostic::SqlError;

#[derive(Clone, Copy, Debug)]
pub enum Argument<'a> {
    Null,
    TinyInt(u8),
    SmallInt(i16),
    Int(i32),
    BigInt(i64),
    Text(&'a str),
    Binary(&'a [u8]),
    /// A literal whose type is legal as an argument but matches no conversion.
    Incompatible,
}

const LIMIT: usize = 2047;

#[derive(Default)]
struct Output(Vec<u16>);
impl Output {
    fn extend(&mut self, units: impl IntoIterator<Item = u16>) {
        self.0.extend(
            units
                .into_iter()
                .take((LIMIT + 1).saturating_sub(self.0.len())),
        );
    }
    fn ascii(&mut self, text: &str) {
        self.extend(text.encode_utf16());
    }
    fn repeat(&mut self, unit: u8, count: usize) {
        self.extend(std::iter::repeat_n(u16::from(unit), count));
    }
    fn finish(mut self) -> Vec<u16> {
        if self.0.len() > LIMIT {
            self.0.truncate(LIMIT - 3);
            self.0.extend([46; 3]);
        }
        self.0
    }
}

struct Arguments<'a> {
    values: &'a [Argument<'a>],
    next: usize,
}
impl<'a> Arguments<'a> {
    fn take(&mut self) -> Argument<'a> {
        let value = self
            .values
            .get(self.next)
            .copied()
            .unwrap_or(Argument::Null);
        self.next += 1;
        value
    }
    fn mismatch(&self) -> SqlError {
        SqlError::new(
            2786,
            1,
            format!(
                "The data type of substitution parameter {} does not match the expected type of the format specification.",
                self.next
            ),
        )
    }
    fn dimension(&mut self) -> Result<i32, SqlError> {
        match self.take() {
            Argument::Int(value) => Ok(value),
            Argument::SmallInt(value) => Ok(i32::from(value)),
            Argument::TinyInt(value) => Ok(i32::from(value)),
            _ => Err(self.mismatch()),
        }
    }
}

#[derive(Default)]
struct Flags {
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    alternate: bool,
}

fn number(bytes: &[u8], cursor: &mut usize) -> usize {
    let mut result = 0usize;
    while let Some(digit @ b'0'..=b'9') = bytes.get(*cursor) {
        result = result
            .saturating_mul(10)
            .saturating_add((digit - b'0') as usize);
        *cursor += 1;
    }
    result
}

/// Format an ad-hoc message. Missing substitutions behave like NULL; unused
/// arguments are ignored. SQL argument eligibility/count checks belong to the
/// statement validator. Allocation is capped even for enormous field widths.
pub fn message(template: &str, values: &[Argument<'_>]) -> Result<Vec<u16>, SqlError> {
    let bytes = template.as_bytes();
    let mut cursor = 0;
    let mut output = Output::default();
    let mut args = Arguments { values, next: 0 };
    while cursor < bytes.len() {
        let start = cursor;
        if bytes[cursor] != b'%' {
            while cursor < bytes.len() && bytes[cursor] != b'%' {
                cursor += 1;
            }
            output.ascii(&template[start..cursor]);
            continue;
        }
        cursor += 1;
        if bytes.get(cursor) == Some(&b'%') {
            output.ascii("%");
            cursor += 1;
            continue;
        }
        let invalid = || {
            SqlError::new(
                2787,
                1,
                format!("Invalid format specification: '{}'.", &template[start..]),
            )
        };
        let mut flags = Flags::default();
        loop {
            match bytes.get(cursor) {
                Some(b'-') => flags.left = true,
                Some(b'+') => flags.plus = true,
                Some(b' ') => flags.space = true,
                Some(b'0') => flags.zero = true,
                Some(b'#') => flags.alternate = true,
                _ => break,
            }
            cursor += 1;
        }
        let width = if bytes.get(cursor) == Some(&b'*') {
            cursor += 1;
            args.dimension()?.max(0) as usize
        } else {
            number(bytes, &mut cursor)
        };
        let precision = if bytes.get(cursor) == Some(&b'.') {
            cursor += 1;
            if bytes.get(cursor) == Some(&b'*') {
                cursor += 1;
                let value = args.dimension()?;
                (value >= 0).then_some(value as usize)
            } else {
                Some(number(bytes, &mut cursor))
            }
        } else {
            None
        };
        let wide = bytes.get(cursor..cursor + 3) == Some(b"I64");
        let short = bytes.get(cursor) == Some(&b'h');
        if wide {
            cursor += 3;
        } else if short || bytes.get(cursor) == Some(&b'l') {
            cursor += 1;
        }
        let conversion = *bytes.get(cursor).ok_or_else(invalid)?;
        if !matches!(conversion, b's' | b'd' | b'i' | b'u' | b'o' | b'x' | b'X')
            || ((wide || short) && conversion == b's')
        {
            return Err(invalid());
        }
        cursor += 1;
        let value = args.take();
        if let Argument::Null = value {
            output.ascii("(null)");
            continue;
        }
        if conversion == b's' {
            let Argument::Text(text) = value else {
                return Err(args.mismatch());
            };
            let length = text
                .encode_utf16()
                .count()
                .min(precision.unwrap_or(usize::MAX));
            let padding = width.saturating_sub(length);
            if !flags.left {
                output.repeat(b' ', padding);
            }
            output.extend(text.encode_utf16().take(length));
            if flags.left {
                output.repeat(b' ', padding);
            }
            continue;
        }
        if short && !matches!(value, Argument::SmallInt(_) | Argument::TinyInt(_)) {
            return Err(args.mismatch());
        }
        let (signed, unsigned) = match (wide, value) {
            (false, Argument::TinyInt(value)) => (i64::from(value), u64::from(value)),
            (false, Argument::SmallInt(value)) => (
                i64::from(value),
                if short {
                    u64::from(value as u16)
                } else {
                    u64::from(i32::from(value) as u32)
                },
            ),
            (false, Argument::Int(value)) => (i64::from(value), u64::from(value as u32)),
            (true, Argument::BigInt(value)) => (value, value as u64),
            _ => return Err(args.mismatch()),
        };
        let is_signed = matches!(conversion, b'd' | b'i');
        let magnitude = if is_signed {
            signed.unsigned_abs()
        } else {
            unsigned
        };
        let digits = if magnitude == 0 && precision == Some(0) {
            String::new()
        } else {
            match conversion {
                b'x' => format!("{magnitude:x}"),
                b'X' => format!("{magnitude:X}"),
                b'o' => format!("{magnitude:o}"),
                _ => magnitude.to_string(),
            }
        };
        let mut prefix = "";
        if is_signed {
            prefix = if signed < 0 {
                "-"
            } else if flags.plus {
                "+"
            } else if flags.space {
                " "
            } else {
                ""
            };
        } else if flags.alternate && magnitude != 0 {
            prefix = match conversion {
                b'x' => "0x",
                b'X' => "0X",
                _ => "",
            };
        }
        let mut zeros = precision.unwrap_or(0).saturating_sub(digits.len());
        if flags.alternate && conversion == b'o' && zeros == 0 && !digits.starts_with('0') {
            zeros = 1;
        }
        let length = prefix
            .len()
            .saturating_add(zeros)
            .saturating_add(digits.len());
        let padding = width.saturating_sub(length);
        let zero_padding = flags.zero && !flags.left && precision.is_none();
        if !flags.left && !zero_padding {
            output.repeat(b' ', padding);
        }
        output.ascii(prefix);
        if zero_padding {
            output.repeat(b'0', padding);
        }
        output.repeat(b'0', zeros);
        output.ascii(&digits);
        if flags.left {
            output.repeat(b' ', padding);
        }
    }
    Ok(output.finish())
}

#[cfg(test)]
mod tests {
    use super::{Argument::*, *};

    #[test]
    fn live_formats_and_typed_substitutions() {
        for (template, args, expected) in [
            ("%s:%d:%%", vec![Text("duck"), Int(42)], "duck:42:%"),
            (
                "%08x|%+6d|%.3s",
                vec![Int(255), Int(12), Text("duck")],
                "000000ff|   +12|duc",
            ),
            ("%*.*s", vec![Int(6), Int(3), Text("duck")], "   duc"),
            ("%*s", vec![Int(-6), Text("duck")], "duck"),
            (
                "%u|%x|%X|%o",
                vec![Int(-1), Int(-1), Int(255), Int(9)],
                "4294967295|ffffffff|FF|11",
            ),
            (
                "%#x|%#o|% d|%-5d",
                vec![Int(255), Int(9), Int(12), Int(12)],
                "0xff|011| 12|12   ",
            ),
            (
                "%.0d|%.4d|%05d",
                vec![Int(0), Int(12), Int(-12)],
                "|0012|-0012",
            ),
            ("%s:%d", vec![Null, Null], "(null):(null)"),
            ("%s:%d", vec![Text("duck")], "duck:(null)"),
            ("plain", vec![Int(42)], "plain"),
            (
                "%hu|%u|%hx|%x",
                vec![SmallInt(-1); 4],
                "65535|4294967295|ffff|ffffffff",
            ),
            ("%hd", vec![TinyInt(12)], "12"),
            ("%*s", vec![SmallInt(6), Text("duck")], "  duck"),
            (
                "%.*s|%*s",
                vec![Int(-1), Text("duck"), Int(6), Text("duck")],
                "duck|  duck",
            ),
            ("%.2d|%8s", vec![Null, Null], "(null)|(null)"),
            ("%hd|%d", vec![SmallInt(12), SmallInt(12)], "12|12"),
            ("%d|%u", vec![TinyInt(12), TinyInt(12)], "12|12"),
            ("%ld|%ls", vec![Int(123), Text("duck")], "123|duck"),
            ("%I64d", vec![BigInt(i64::MAX)], "9223372036854775807"),
        ] {
            assert_eq!(
                message(template, &args).unwrap(),
                expected.encode_utf16().collect::<Vec<_>>(),
                "{template}"
            );
        }
        for (template, args, number) in [
            ("%q", vec![Int(1)], 2787),
            ("%hd", vec![Int(12)], 2786),
            ("%hs", vec![Text("duck")], 2787),
            ("%I64d", vec![Int(12)], 2786),
            ("%*s", vec![Null, Text("duck")], 2786),
            ("%.*s", vec![Null, Text("duck")], 2786),
            ("%d", vec![Text("duck")], 2786),
            ("%s", vec![Int(1)], 2786),
            ("%d", vec![BigInt(i64::MAX)], 2786),
        ] {
            let error = message(template, &args).unwrap_err();
            assert_eq!((error.number, error.state, error.severity), (number, 1, 16));
        }
    }

    #[test]
    fn utf16_precision_and_bounded_output_keep_validating() {
        assert_eq!(message("%.1s", &[Text("🦆x")]).unwrap(), [0xd83e]);
        for length in [2044, 2047] {
            let text = "x".repeat(length);
            let expected: Vec<_> = text.encode_utf16().collect();
            assert_eq!(message(&text, &[]).unwrap(), expected);
            assert_eq!(message("%s", &[Text(&text)]).unwrap(), expected);
        }
        let long = "x".repeat(2100);
        let expected: Vec<_> = format!("{}...", "x".repeat(2044)).encode_utf16().collect();
        assert_eq!(message(&long, &[]).unwrap(), expected);
        assert_eq!(message("%s", &[Text(&long)]).unwrap(), expected);
        assert_eq!(
            message("%999999999999999999999999d", &[Int(1)])
                .unwrap()
                .len(),
            LIMIT
        );
        assert_eq!(
            message("%999999999999999999999999d %s", &[Int(1), Int(2)])
                .unwrap_err()
                .number,
            2786
        );
    }
}
