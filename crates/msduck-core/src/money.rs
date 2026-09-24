//! Currency range rules over exact integers in units of 0.0001.
use crate::diagnostic::SqlError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoneyType {
    Money,
    SmallMoney,
}

impl MoneyType {
    /// Validate a value already converted to scale four. Source parsing and
    /// rounding must happen before this check; no floating-point conversion is
    /// performed here. NULL propagation belongs to the caller.
    pub fn check_scaled(self, value: i128) -> Result<(), SqlError> {
        let (valid, name) = match self {
            Self::Money => (i64::try_from(value).is_ok(), "money"),
            Self::SmallMoney => (i32::try_from(value).is_ok(), "smallmoney"),
        };
        if valid {
            Ok(())
        } else {
            Err(SqlError::new(
                8115,
                1,
                format!("Arithmetic overflow error converting expression to data type {name}."),
            ))
        }
    }
}

/// Character conversion diagnostics are distinct from numeric overflow.
pub const TEXT_SYNTAX: &str =
    "Cannot convert a char value to money. The char value has incorrect syntax.";
pub const TEXT_OVERFLOW: &str =
    "The conversion from char data type to money resulted in a money overflow error.";

fn currency(c: char) -> bool {
    matches!(c, '$' | '\u{00a2}'..='\u{00a5}' | '\u{09f2}' | '\u{09f3}' | '\u{0e3f}'
        | '\u{17db}' | '\u{20a0}'..='\u{20b1}' | '\u{fdfc}' | '\u{fe69}'
        | '\u{ff04}' | '\u{ffe0}' | '\u{ffe1}' | '\u{ffe5}' | '\u{ffe6}')
}

/// Parse currency text directly to units of 0.0001, without a decimal precision
/// ceiling or floating-point intermediary. Commas are ignored throughout the
/// string; a single currency prefix and sign may precede the decimal digits.
/// Retain four fractional digits and round halfway away from zero.
pub fn parse_text(text: &str) -> Result<i64, SqlError> {
    let invalid = || SqlError::new(235, 1, TEXT_SYNTAX);
    let overflow = || SqlError::new(236, 1, TEXT_OVERFLOW);
    let mut chars = text
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .chars()
        .filter(|c| *c != ',')
        .peekable();
    let mut negative = false;
    let mut sign = false;
    let mut symbol = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_whitespace() {
            chars.next();
        } else if matches!(c, '+' | '-') && !sign {
            sign = true;
            negative = c == '-';
            chars.next();
        } else if currency(c) && !symbol {
            symbol = true;
            chars.next();
        } else {
            break;
        }
    }
    let mut whole = 0_u64;
    let mut fraction = 0_u64;
    let mut places = 0_u8;
    let mut decimal = false;
    let mut round = false;
    for c in chars {
        if c == '.' && !decimal {
            decimal = true;
        } else if c.is_ascii_digit() {
            let digit = u64::from(c as u8 - b'0');
            if !decimal {
                // Saturation preserves overflow while still validating the tail.
                whole = whole.saturating_mul(10).saturating_add(digit);
            } else if places < 4 {
                fraction = fraction * 10 + digit;
                places += 1;
            } else if places == 4 {
                round = digit >= 5;
                places = 5;
            }
        } else {
            return Err(invalid());
        }
    }
    while places < 4 {
        fraction *= 10;
        places += 1;
    }
    let magnitude = i128::from(whole) * 10_000 + i128::from(fraction) + i128::from(round);
    let scaled = if negative { -magnitude } else { magnitude };
    i64::try_from(scaled).map_err(|_| overflow())
}

/// Format an exact currency coefficient using CONVERT's invariant styles.
/// Style 126 aliases 2 for all character targets; other styles use 0.
pub fn format(scaled: i64, style: i32) -> String {
    let four = style == 2 || style == 126;
    let magnitude = scaled.unsigned_abs();
    let (value, scale) = if four {
        (magnitude, 10_000)
    } else {
        ((magnitude + 50) / 100, 100)
    };
    let whole = (value / scale).to_string();
    let mut result = String::with_capacity(32);
    if scaled < 0 {
        result.push('-');
    }
    for (index, digit) in whole.chars().enumerate() {
        if style == 1 && index > 0 && (whole.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result.push('.');
    use std::fmt::Write;
    write!(
        result,
        "{:0width$}",
        value % scale,
        width = if four { 4 } else { 2 }
    )
    .expect("write to String");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_currency_boundaries_and_adjacent_ten_thousandths() {
        // SQL Server's documented decimal endpoints, expressed without floats.
        for (kind, minimum, maximum, name) in [
            (
                MoneyType::Money,
                -9_223_372_036_854_775_808_i128,
                9_223_372_036_854_775_807_i128,
                "money",
            ),
            (
                MoneyType::SmallMoney,
                -2_147_483_648,
                2_147_483_647,
                "smallmoney",
            ),
        ] {
            for value in [minimum, minimum + 1, -1, 0, 1, maximum - 1, maximum] {
                assert_eq!(kind.check_scaled(value), Ok(()), "{kind:?}: {value}");
            }
            for value in [i128::MIN, minimum - 1, maximum + 1, i128::MAX] {
                let error = kind.check_scaled(value).unwrap_err();
                assert_eq!((error.number, error.state), (8115, 1));
                assert_eq!(
                    error.to_string(),
                    format!("Arithmetic overflow error converting expression to data type {name}."),
                );
            }
        }
    }
    #[test]
    fn currency_text_keeps_exact_boundaries_and_rounding() {
        for (text, expected) in [
            ("$1,234.56789", 12_345_679),
            ("$-23", -230_000),
            (" -£12.34565 ", -123_457),
            (".00005", 1),
            ("-.00005", -1),
            ("922,337,203,685,477.58074", i64::MAX),
            ("-922337203685477.58084", i64::MIN),
            ("1,2.3,4,5,6,7", 123_457),
            ("00000000000000000000000000000000001.25", 12_500),
        ] {
            assert_eq!(parse_text(text), Ok(expected), "{text}");
        }
        for symbol in "$¢£¤¥৲৳฿៛₠₡₢₣₤₥₦₧₨₩₪₫€₭₮₯₰₱﷼﹩＄￠￡￥￦".chars()
        {
            assert_eq!(parse_text(&format!("{symbol}1.25")), Ok(12_500));
        }
        for text in [
            "922337203685477.58075",
            "-922337203685477.58085",
            &"9".repeat(8000),
        ] {
            assert_eq!(parse_text(text).unwrap_err().number, 236);
        }
        let long_fraction = format!("1.23454{}", "9".repeat(8000));
        assert_eq!(parse_text(&long_fraction), Ok(12345));
    }

    #[test]
    fn currency_text_rejects_invalid_syntax_without_numeric_fallback() {
        for text in [
            "bad", "1e3", "1e-100", "NaN", "Inf", "1.2.3", "1-", "--1", "$$1", "12 34", "１２",
            "₹1", "1\0",
        ] {
            assert_eq!(parse_text(text).unwrap_err().number, 235, "{text:?}");
        }
        assert_eq!(
            parse_text(&format!("{}x", "9".repeat(8000)))
                .unwrap_err()
                .number,
            235
        );
    }
    #[test]
    fn currency_output_styles_keep_integer_precision_and_carry() {
        for (scaled, style, expected) in [
            (12_345_678, 0, "1234.57"),
            (12_345_678, 1, "1,234.57"),
            (12_345_678, 2, "1234.5678"),
            (12_345_678, 126, "1234.5678"),
            (12_345_678, -1, "1234.57"),
            (-12_345_650, 1, "-1,234.57"),
            (9_999_950, 1, "1,000.00"),
            (0, 2, "0.0000"),
            (i64::MAX, 2, "922337203685477.5807"),
            (i64::MIN, 2, "-922337203685477.5808"),
            (i64::MIN, 1, "-922,337,203,685,477.58"),
        ] {
            assert_eq!(format(scaled, style), expected);
        }
    }
}

/// Arithmetic over coefficients in units of 0.0001. Inputs are already
/// converted to the chosen currency family; all intermediates fit in i128.
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

pub fn calculate(
    kind: MoneyType,
    operation: Operation,
    left: i64,
    right: i64,
) -> Result<i64, SqlError> {
    kind.check_scaled(i128::from(left))?;
    kind.check_scaled(i128::from(right))?;
    let (a, b) = (i128::from(left), i128::from(right));
    let rounded = |numerator: i128, denominator: i128| {
        let quotient = numerator / denominator;
        let remainder = numerator % denominator;
        if remainder.abs() * 2 >= denominator.abs() {
            quotient
                + if (numerator < 0) == (denominator < 0) {
                    1
                } else {
                    -1
                }
        } else {
            quotient
        }
    };
    let value = match operation {
        Operation::Add => a + b,
        Operation::Subtract => a - b,
        Operation::Multiply => rounded(a * b, 10000),
        Operation::Divide | Operation::Modulo if b == 0 => {
            return Err(SqlError::new(8134, 1, "Divide by zero error encountered."));
        }
        // SQL Server truncates currency division, unlike multiplication.
        Operation::Divide => a * 10000 / b,
        Operation::Modulo => a % b,
    };
    kind.check_scaled(value)?;
    Ok(value as i64)
}

#[cfg(test)]
mod arithmetic_tests {
    use super::*;
    #[test]
    fn exact_arithmetic_rounds_signed_ties_and_checks_ranges() {
        for (op, a, b, want) in [
            (Operation::Add, 12345, 20000, 32345),
            (Operation::Subtract, 10000, 12345, -2345),
            (Operation::Multiply, 1, 5000, 1),
            (Operation::Multiply, -1, 5000, -1),
            (Operation::Divide, 1, 20000, 0),
            (Operation::Divide, 1, -20000, 0),
            (Operation::Divide, 10000, 30000, 3333),
            (Operation::Divide, 20000, 30000, 6666),
            (Operation::Modulo, -55000, 20000, -15000),
        ] {
            assert_eq!(calculate(MoneyType::Money, op, a, b).unwrap(), want);
        }
        for (kind, op, a, b, number) in [
            (MoneyType::Money, Operation::Add, i64::MAX, 1, 8115),
            (MoneyType::Money, Operation::Subtract, 0, i64::MIN, 8115),
            (
                MoneyType::SmallMoney,
                Operation::Add,
                i32::MAX.into(),
                1,
                8115,
            ),
            (MoneyType::Money, Operation::Multiply, i64::MAX, 20000, 8115),
            (MoneyType::Money, Operation::Divide, 1, 0, 8134),
            (MoneyType::Money, Operation::Modulo, 1, 0, 8134),
        ] {
            assert_eq!(calculate(kind, op, a, b).unwrap_err().number, number);
        }
        assert_eq!(
            calculate(MoneyType::Money, Operation::Multiply, i64::MIN, 10000).unwrap(),
            i64::MIN
        );
        assert_eq!(
            calculate(MoneyType::Money, Operation::Divide, i64::MAX, 10000).unwrap(),
            i64::MAX
        );
    }
}
