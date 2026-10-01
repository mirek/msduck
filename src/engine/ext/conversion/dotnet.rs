//! .NET Framework formatting, as SQL Server's FORMAT applies it, for the
//! en-US and invariant cultures: standard and custom numeric, date/time and
//! TimeSpan format strings. A format .NET rejects (FormatException) gives
//! `None`, which FORMAT returns as NULL. The rules follow the outputs
//! captured in reference/format.json.
use crate::datetime2::DateTime2;

/// The formatting conventions of one culture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Culture {
    pub en_us: bool,
}

impl Culture {
    pub const EN_US: Self = Self { en_us: true };
    pub const INVARIANT: Self = Self { en_us: false };

    fn currency(self) -> &'static str {
        if self.en_us { "$" } else { "\u{a4}" }
    }
    fn short_date(self) -> &'static str {
        if self.en_us { "M/d/yyyy" } else { "MM/dd/yyyy" }
    }
    fn long_date(self) -> &'static str {
        if self.en_us {
            "dddd, MMMM d, yyyy"
        } else {
            "dddd, dd MMMM yyyy"
        }
    }
    fn short_time(self) -> &'static str {
        if self.en_us { "h:mm tt" } else { "HH:mm" }
    }
    fn long_time(self) -> &'static str {
        if self.en_us { "h:mm:ss tt" } else { "HH:mm:ss" }
    }
    fn month_day(self) -> &'static str {
        if self.en_us { "MMMM d" } else { "MMMM dd" }
    }
    fn year_month(self) -> &'static str {
        if self.en_us { "MMMM yyyy" } else { "yyyy MMMM" }
    }
}

/// Why a culture name is not usable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CultureError {
    /// 9818: .NET does not accept the name.
    Invalid,
    /// A real culture whose conventions msduck does not implement.
    Unsupported,
}

/// Languages .NET has culture data for. Well-formed tags for other languages
/// format like the invariant culture.
const LANGUAGES: &[&str] = &[
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "co", "cs", "cy",
    "da", "de", "dv", "el", "en", "es", "et", "eu", "fa", "fi", "fil", "fo", "fr", "fy", "ga",
    "gd", "gl", "gsw", "gu", "ha", "he", "hi", "hr", "hsb", "hu", "hy", "id", "ig", "ii", "is",
    "it", "iu", "ja", "ka", "kk", "kl", "km", "kn", "ko", "kok", "ky", "lb", "lo", "lt", "lv",
    "mi", "mk", "ml", "mn", "moh", "mr", "ms", "mt", "my", "nb", "ne", "nl", "nn", "no", "nso",
    "oc", "om", "or", "pa", "pl", "prs", "ps", "pt", "quz", "rm", "ro", "ru", "rw", "sa", "sah",
    "se", "si", "sk", "sl", "sma", "smj", "smn", "sms", "sq", "sr", "sv", "sw", "syr", "ta", "te",
    "tg", "th", "tk", "tn", "tr", "tt", "tzm", "ug", "uk", "ur", "uz", "vi", "wo", "xh", "yo",
    "zh", "zu",
];

/// Resolve a FORMAT culture argument.
pub(super) fn culture(name: &str) -> Result<Culture, CultureError> {
    let subtags: Vec<&str> = name.split(['-', '_']).collect();
    let alpha = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic());
    let alnum = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    let language = subtags[0];
    if !(alpha(language) && (2..=3).contains(&language.len())) {
        return Err(CultureError::Invalid);
    }
    let mut region = None;
    let mut position = 1;
    if let Some(script) = subtags.get(position)
        && script.len() == 4
        && alpha(script)
    {
        position += 1;
    }
    if let Some(candidate) = subtags.get(position)
        && ((candidate.len() == 2 && alpha(candidate))
            || (candidate.len() == 3 && candidate.bytes().all(|b| b.is_ascii_digit())))
    {
        region = Some(candidate.to_ascii_uppercase());
        position += 1;
    }
    while let Some(subtag) = subtags.get(position) {
        if subtag.eq_ignore_ascii_case("x") {
            let private = &subtags[position + 1..];
            if private.is_empty() || !private.iter().all(|s| alnum(s) && s.len() <= 8) {
                return Err(CultureError::Invalid);
            }
            break;
        }
        let variant = (5..=8).contains(&subtag.len()) && alnum(subtag)
            || (subtag.len() == 4 && subtag.as_bytes()[0].is_ascii_digit() && alnum(subtag));
        if !variant {
            return Err(CultureError::Invalid);
        }
        position += 1;
    }
    let language = language.to_ascii_lowercase();
    if language == "iv" && subtags.len() == 1 {
        return Ok(Culture::INVARIANT);
    }
    if language == "en" && region.as_deref().is_none_or(|r| r == "US") {
        return Ok(Culture::EN_US);
    }
    if LANGUAGES.contains(&language.as_str()) {
        return Err(CultureError::Unsupported);
    }
    Ok(Culture::INVARIANT)
}

// ---------------------------------------------------------------------------
// Numbers

/// A numeric value as FORMAT receives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Number {
    /// An integer and the bit width of its SQL type (for X).
    Integer(i128, u8),
    /// An exact decimal: coefficient and scale.
    Decimal(i128, u8),
    Double(f64),
    Single(f32),
}

/// Significant digits and the decimal exponent: the value is
/// 0.d1d2d3... * 10^scale, as in .NET's NUMBER buffer.
#[derive(Clone, Debug, PartialEq)]
struct Digits {
    negative: bool,
    digits: Vec<u8>,
    scale: i32,
}

impl Digits {
    fn from_text(negative: bool, integer: &str, fraction: &str) -> Self {
        let mut all: Vec<u8> = integer
            .bytes()
            .chain(fraction.bytes())
            .map(|b| b - b'0')
            .collect();
        let mut scale = integer.len() as i32;
        while all.first() == Some(&0) {
            all.remove(0);
            scale -= 1;
        }
        if all.is_empty() {
            scale = 0;
        }
        Self {
            negative,
            digits: all,
            scale,
        }
    }

    fn from_decimal(coefficient: i128, scale: u8, keep_zeros: bool) -> Self {
        let text = coefficient.unsigned_abs().to_string();
        let scale = usize::from(scale);
        let padded = if text.len() <= scale {
            format!("{}{text}", "0".repeat(scale - text.len() + 1))
        } else {
            text
        };
        let (integer, fraction) = padded.split_at(padded.len() - scale);
        let mut value = Self::from_text(coefficient < 0, integer, fraction);
        if !keep_zeros {
            value.trim();
        }
        value
    }

    /// `precision` significant digits of a binary float, rounded to nearest.
    fn from_float(value: f64, precision: usize) -> Self {
        if value == 0.0 || !value.is_finite() {
            return Self {
                negative: false,
                digits: Vec::new(),
                scale: 0,
            };
        }
        let text = format!("{:.*e}", precision - 1, value.abs());
        let (mantissa, exponent) = text.split_once('e').expect("scientific notation");
        let exponent: i32 = exponent.parse().expect("exponent");
        let digits: Vec<u8> = mantissa
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect();
        let mut value = Self {
            negative: value < 0.0,
            digits,
            scale: exponent + 1,
        };
        value.trim();
        value
    }

    /// Remove trailing zero digits.
    fn trim(&mut self) {
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        if self.digits.is_empty() {
            self.scale = 0;
        }
    }

    fn is_zero(&self) -> bool {
        self.digits.iter().all(|d| *d == 0)
    }

    /// Round half away from zero, keeping `position` digits after the first.
    fn round(&mut self, position: i32) {
        if position < 0 {
            self.digits.clear();
            self.scale = 0;
            return;
        }
        let position = position as usize;
        if position >= self.digits.len() {
            return;
        }
        let up = self.digits[position] >= 5;
        self.digits.truncate(position);
        if up {
            let mut i = position;
            loop {
                if i == 0 {
                    self.digits.insert(0, 1);
                    self.scale += 1;
                    break;
                }
                i -= 1;
                if self.digits[i] == 9 {
                    self.digits[i] = 0;
                } else {
                    self.digits[i] += 1;
                    break;
                }
            }
        }
        self.trim();
    }

    /// Digit `i` counted from the first significant digit, or 0.
    fn digit(&self, i: i32) -> u8 {
        if i < 0 {
            return 0;
        }
        self.digits.get(i as usize).copied().unwrap_or(0)
    }

    /// Integer part digits ("0" when empty).
    fn integer(&self) -> String {
        if self.scale <= 0 {
            return "0".into();
        }
        (0..self.scale)
            .map(|i| (b'0' + self.digit(i)) as char)
            .collect()
    }

    /// `count` fraction digits.
    fn fraction(&self, count: usize) -> String {
        (0..count as i32)
            .map(|i| (b'0' + self.digit(self.scale + i)) as char)
            .collect()
    }

    fn scaled(mut self, power: i32) -> Self {
        if !self.digits.is_empty() {
            self.scale += power;
        }
        self
    }
}

fn group(integer: &str) -> String {
    let mut out = String::new();
    let len = integer.len();
    for (i, c) in integer.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl Number {
    fn digits(self, precision: Option<usize>) -> Digits {
        match self {
            Self::Integer(value, _) => {
                Digits::from_text(value < 0, &value.unsigned_abs().to_string(), "")
            }
            Self::Decimal(coefficient, scale) => Digits::from_decimal(coefficient, scale, false),
            Self::Double(value) => Digits::from_float(value, precision.unwrap_or(15)),
            Self::Single(value) => Digits::from_float(f64::from(value), precision.unwrap_or(7)),
        }
    }
}

/// Format a number with a standard or custom numeric format.
pub(super) fn number(value: Number, format: &str, culture: Culture) -> Option<String> {
    let mut chars = format.chars();
    if let Some(letter) = chars.next()
        && letter.is_ascii_alphabetic()
    {
        let rest: String = chars.collect();
        if rest.is_empty() || (rest.len() <= 2 && rest.bytes().all(|b| b.is_ascii_digit())) {
            let precision = if rest.is_empty() {
                None
            } else {
                Some(rest.parse::<usize>().ok()?)
            };
            return standard(value, letter, precision, culture);
        }
    }
    if format.is_empty() {
        return standard(value, 'G', None, culture);
    }
    custom(value, format)
}

fn exponent(value: i32, letter: char, minimum: usize, always_sign: bool) -> String {
    let sign = if value < 0 {
        "-"
    } else if always_sign {
        "+"
    } else {
        ""
    };
    format!("{letter}{sign}{:0minimum$}", value.unsigned_abs())
}

fn standard(
    value: Number,
    letter: char,
    precision: Option<usize>,
    culture: Culture,
) -> Option<String> {
    let integer = matches!(value, Number::Integer(..));
    let sign = |negative: bool, text: String| {
        if negative { format!("-{text}") } else { text }
    };
    let fixed = |precision: usize, grouped: bool, digits: Digits| {
        let mut digits = digits;
        digits.round(digits.scale + precision as i32);
        let negative = digits.negative && !digits.is_zero();
        let whole = digits.integer();
        let whole = if grouped { group(&whole) } else { whole };
        let text = if precision > 0 {
            format!("{whole}.{}", digits.fraction(precision))
        } else {
            whole
        };
        (negative, text)
    };
    match letter.to_ascii_uppercase() {
        'C' => {
            let (negative, text) = fixed(precision.unwrap_or(2), true, value.digits(None));
            let text = format!("{}{text}", culture.currency());
            Some(if negative { format!("({text})") } else { text })
        }
        'D' => {
            let Number::Integer(v, _) = value else {
                return None;
            };
            let digits = v.unsigned_abs().to_string();
            let width = precision.unwrap_or(0);
            Some(sign(v < 0, format!("{digits:0>width$}")))
        }
        'E' => {
            let precision = precision.unwrap_or(6);
            let mut digits = value.digits(None);
            digits.round(precision as i32 + 1);
            let negative = digits.negative && !digits.is_zero();
            let exp = if digits.is_zero() {
                0
            } else {
                digits.scale - 1
            };
            let mantissa: String = (0..=precision as i32)
                .map(|i| (b'0' + digits.digit(i)) as char)
                .collect();
            let mantissa = if precision > 0 {
                format!("{}.{}", &mantissa[..1], &mantissa[1..])
            } else {
                mantissa
            };
            let e = if letter == 'e' { 'e' } else { 'E' };
            Some(sign(
                negative,
                format!("{mantissa}{}", exponent(exp, e, 3, true)),
            ))
        }
        'F' => {
            let (negative, text) = fixed(precision.unwrap_or(2), false, value.digits(None));
            Some(sign(negative, text))
        }
        'N' => {
            let (negative, text) = fixed(precision.unwrap_or(2), true, value.digits(None));
            Some(sign(negative, text))
        }
        'P' => {
            let (negative, text) =
                fixed(precision.unwrap_or(2), true, value.digits(None).scaled(2));
            let text = if culture.en_us {
                format!("{text}%")
            } else {
                format!("{text} %")
            };
            Some(sign(negative, text))
        }
        'G' => {
            let mut digits = match (value, precision) {
                (Number::Decimal(coefficient, scale), None) => {
                    Digits::from_decimal(coefficient, scale, true)
                }
                (Number::Double(_), Some(p)) | (Number::Single(_), Some(p)) if p > 0 => {
                    value.digits(Some(p))
                }
                _ => value.digits(None),
            };
            let default = match value {
                Number::Integer(..) | Number::Decimal(..) => digits.digits.len().max(1),
                Number::Double(_) => 15,
                Number::Single(_) => 7,
            };
            let limit = precision.filter(|p| *p > 0).unwrap_or(default);
            let keep_zeros = matches!(value, Number::Decimal(..)) && precision.is_none();
            if !keep_zeros || digits.digits.len() > limit {
                digits.round(limit as i32);
            }
            let negative = digits.negative && !digits.is_zero();
            if digits.is_zero() {
                return Some(if keep_zeros {
                    let Number::Decimal(_, scale) = value else {
                        unreachable!()
                    };
                    if scale == 0 {
                        "0".into()
                    } else {
                        format!("0.{}", "0".repeat(usize::from(scale)))
                    }
                } else {
                    "0".into()
                });
            }
            let exp = digits.scale - 1;
            let scientific = digits.scale > limit as i32 || digits.scale < -3;
            let text = if scientific {
                let mantissa: String = digits.digits.iter().map(|d| (b'0' + d) as char).collect();
                let mantissa = if mantissa.len() > 1 {
                    format!("{}.{}", &mantissa[..1], &mantissa[1..])
                } else {
                    mantissa
                };
                let e = if letter == 'g' { 'e' } else { 'E' };
                format!("{mantissa}{}", exponent(exp, e, 2, true))
            } else {
                let fraction_digits = (digits.digits.len() as i32 - digits.scale).max(0) as usize;
                let whole = digits.integer();
                if fraction_digits > 0 {
                    format!("{whole}.{}", digits.fraction(fraction_digits))
                } else {
                    whole
                }
            };
            Some(sign(negative, text))
        }
        'R' => {
            let text = match value {
                Number::Double(v) => {
                    let short = Digits::from_float(v, 15);
                    let parsed = render_plain(&short).parse::<f64>().ok();
                    if parsed == Some(v.abs()) {
                        general_digits(short, 15, 'E')
                    } else {
                        general_digits(Digits::from_float(v, 17), 17, 'E')
                    }
                }
                Number::Single(v) => {
                    let short = Digits::from_float(f64::from(v), 7);
                    let parsed = render_plain(&short).parse::<f32>().ok();
                    if parsed == Some(v.abs()) {
                        general_digits(short, 7, 'E')
                    } else {
                        general_digits(Digits::from_float(f64::from(v), 9), 9, 'E')
                    }
                }
                _ => return None,
            };
            Some(text)
        }
        'X' => {
            let Number::Integer(v, bits) = value else {
                return None;
            };
            let unsigned = if v < 0 {
                (v + (1i128 << bits)) as u128
            } else {
                v as u128
            };
            let text = if letter == 'x' {
                format!("{unsigned:x}")
            } else {
                format!("{unsigned:X}")
            };
            let width = precision.unwrap_or(0);
            Some(format!("{text:0>width$}"))
        }
        _ => {
            let _ = integer;
            None
        }
    }
}

/// Plain decimal text of digits (no grouping), for round-trip checks.
fn render_plain(digits: &Digits) -> String {
    if digits.digits.is_empty() {
        return "0".into();
    }
    let mantissa: String = digits.digits.iter().map(|d| (b'0' + d) as char).collect();
    format!("0.{mantissa}e{}", digits.scale)
}

/// The G rendering of already-rounded digits.
fn general_digits(digits: Digits, limit: usize, letter: char) -> String {
    if digits.is_zero() {
        return "0".into();
    }
    let negative = digits.negative;
    let exp = digits.scale - 1;
    let text = if digits.scale > limit as i32 || digits.scale < -3 {
        let mantissa: String = digits.digits.iter().map(|d| (b'0' + d) as char).collect();
        let mantissa = if mantissa.len() > 1 {
            format!("{}.{}", &mantissa[..1], &mantissa[1..])
        } else {
            mantissa
        };
        format!("{mantissa}{}", exponent(exp, letter, 2, true))
    } else {
        let fraction_digits = (digits.digits.len() as i32 - digits.scale).max(0) as usize;
        let whole = digits.integer();
        if fraction_digits > 0 {
            format!("{whole}.{}", digits.fraction(fraction_digits))
        } else {
            whole
        }
    };
    if negative { format!("-{text}") } else { text }
}

/// One section of a custom numeric format, split into literal text and
/// placeholders.
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Literal(String),
    Zero,
    Hash,
    Point,
    Group,
    Percent,
    PerMille,
    Exponent {
        upper: bool,
        sign: bool,
        digits: usize,
    },
}

fn pieces(section: &str) -> Vec<Piece> {
    let chars: Vec<char> = section.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '0' => out.push(Piece::Zero),
            '#' => out.push(Piece::Hash),
            '.' => out.push(Piece::Point),
            ',' => out.push(Piece::Group),
            '%' => out.push(Piece::Percent),
            '\u{2030}' => out.push(Piece::PerMille),
            '\\' => {
                i += 1;
                if let Some(next) = chars.get(i) {
                    out.push(Piece::Literal(next.to_string()));
                }
            }
            '\'' | '"' => {
                // SQL Server's FORMAT ends a quoted run at the next quote of
                // the same kind but starts another run there, so everything
                // after the first quote is literal text.
                let quote = c;
                let text: String = chars[i + 1..].iter().filter(|ch| **ch != quote).collect();
                out.push(Piece::Literal(text));
                break;
            }
            'E' | 'e' => {
                let mut j = i + 1;
                let mut sign = false;
                if matches!(chars.get(j), Some('+') | Some('-')) {
                    sign = chars[j] == '+';
                    j += 1;
                }
                let start = j;
                while chars.get(j) == Some(&'0') {
                    j += 1;
                }
                if j > start {
                    out.push(Piece::Exponent {
                        upper: c == 'E',
                        sign,
                        digits: j - start,
                    });
                    i = j;
                    continue;
                }
                out.push(Piece::Literal(c.to_string()));
            }
            other => out.push(Piece::Literal(other.to_string())),
        }
        i += 1;
    }
    out
}

/// Split sections on unquoted, unescaped semicolons.
fn sections(format: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = format.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if !quoted => {
                out.last_mut().unwrap().push(c);
                if let Some(next) = chars.next() {
                    out.last_mut().unwrap().push(next);
                }
            }
            '\'' | '"' => {
                // Everything after a quote is literal (see `pieces`).
                quoted = true;
                out.last_mut().unwrap().push(c);
            }
            ';' if !quoted => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn custom(value: Number, format: &str) -> Option<String> {
    let sections = sections(format);
    let base = value.digits(None);
    let negative = base.negative && !base.is_zero();
    let zero = base.is_zero();
    let zero_section = sections.len() >= 3 && !sections[2].is_empty();
    let (index, explicit_sign) = match sections.len() {
        1 => (0, negative),
        _ if zero && zero_section => (2, false),
        _ if negative && !sections[1].is_empty() => (1, false),
        _ => (0, negative),
    };
    let (text, rounded_zero) = render(&base, &sections[index], explicit_sign);
    // A value that rounds to zero takes the zero section.
    if rounded_zero && zero_section && index != 2 {
        return Some(render(&base, &sections[2], false).0);
    }
    Some(text)
}

/// Render one custom format section; also report whether the rounded value
/// is zero.
fn render(base: &Digits, section: &str, explicit_sign: bool) -> (String, bool) {
    let pieces = pieces(section);
    // Scan: digit placeholders before and after the point, scaling and
    // grouping, exponent.
    let point = pieces.iter().position(|p| *p == Piece::Point);
    let digit_positions: Vec<usize> = pieces
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Piece::Zero | Piece::Hash))
        .map(|(i, _)| i)
        .collect();
    let exponent_at = pieces
        .iter()
        .position(|p| matches!(p, Piece::Exponent { .. }));
    let integer_end = point.or(exponent_at).unwrap_or(pieces.len());
    let integer_digits: Vec<usize> = digit_positions
        .iter()
        .copied()
        .filter(|i| *i < integer_end)
        .collect();
    let fraction_end = exponent_at.unwrap_or(pieces.len());
    let fraction_digits: Vec<usize> = match point {
        Some(p) => digit_positions
            .iter()
            .copied()
            .filter(|i| *i > p && *i < fraction_end)
            .collect(),
        None => Vec::new(),
    };
    // Scaling commas: immediately left of the point (or of the end of the
    // integer placeholders when there is no point).
    let mut scaling = 0;
    if let Some(last) = integer_digits.last() {
        let mut j = last + 1;
        while j < pieces.len() && pieces[j] == Piece::Group {
            scaling += 1;
            j += 1;
        }
        if !(j >= integer_end || point == Some(j) || exponent_at == Some(j)) {
            scaling = 0;
        }
    }
    let grouping = integer_digits.len() >= 2
        && pieces[integer_digits[0]..*integer_digits.last().unwrap()].contains(&Piece::Group);
    let mut digits = base.clone();
    digits.negative = false;
    for p in &pieces {
        match p {
            Piece::Percent => digits = digits.scaled(2),
            Piece::PerMille => digits = digits.scaled(3),
            _ => {}
        }
    }
    digits = digits.scaled(-3 * scaling);
    let first_zero = integer_digits
        .iter()
        .position(|i| pieces[*i] == Piece::Zero)
        .map(|k| integer_digits.len() - k)
        .unwrap_or(0);
    let last_zero = fraction_digits
        .iter()
        .rposition(|i| pieces[*i] == Piece::Zero)
        .map(|k| k + 1)
        .unwrap_or(0);
    let mut exponent_value = 0;
    if exponent_at.is_some() {
        if !digits.is_zero() {
            let shift = integer_digits.len().max(1) as i32;
            exponent_value = digits.scale - shift;
            digits.scale = shift;
        }
        digits.round(digits.scale + fraction_digits.len() as i32);
    } else {
        digits.round(digits.scale + fraction_digits.len() as i32);
    }
    // Integer text: all integer digits of the value, zero-padded to the
    // number of '0' placeholders counted from the point.
    let mut whole = if digits.scale > 0 {
        digits.integer()
    } else {
        String::new()
    };
    if whole.len() < first_zero {
        whole = format!("{}{whole}", "0".repeat(first_zero - whole.len()));
    }
    if grouping {
        whole = group(&whole);
    }
    let mut fraction = digits.fraction(fraction_digits.len());
    // Optional (#) trailing digits are dropped when zero.
    while fraction.len() > last_zero && fraction.ends_with('0') {
        fraction.pop();
    }
    let rendered_zero = whole.chars().all(|c| c == '0' || c == ',')
        && fraction.chars().all(|c| c == '0')
        && exponent_value == 0;
    let mut out = String::new();
    let mut integer_emitted = false;
    let mut fraction_index = 0;
    let fraction_chars: Vec<char> = fraction.chars().collect();
    for (i, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Literal(text) => out.push_str(text),
            Piece::Zero | Piece::Hash => {
                if i < integer_end {
                    if !integer_emitted {
                        out.push_str(&whole);
                        integer_emitted = true;
                    }
                } else if let Some(c) = fraction_chars.get(fraction_index) {
                    out.push(*c);
                    fraction_index += 1;
                } else {
                    fraction_index += 1;
                }
            }
            Piece::Point => {
                if !fraction.is_empty() {
                    out.push('.');
                }
            }
            Piece::Group => {}
            Piece::Percent => out.push('%'),
            Piece::PerMille => out.push('\u{2030}'),
            Piece::Exponent {
                upper,
                sign,
                digits: width,
            } => {
                out.push_str(&exponent(
                    exponent_value,
                    if *upper { 'E' } else { 'e' },
                    *width,
                    *sign,
                ));
            }
        }
    }
    let text = if explicit_sign && (!rendered_zero || digit_positions.is_empty()) {
        format!("-{out}")
    } else {
        out
    };
    (text, rendered_zero && !digit_positions.is_empty())
}

// ---------------------------------------------------------------------------
// Dates and times

/// A date/time value as FORMAT receives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Moment {
    /// Local date and time.
    pub local: DateTime2,
    /// The offset of a datetimeoffset, in minutes.
    pub offset: Option<i16>,
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn weekday(value: DateTime2) -> usize {
    // 0001-01-01 was a Monday.
    ((value.ticks() / (86_400 * 10_000_000) + 1) % 7) as usize
}

/// Format a date/time value with a standard or custom format.
pub(super) fn moment(value: Moment, format: &str, culture: Culture) -> Option<String> {
    let mut chars = format.chars();
    let (first, second) = (chars.next(), chars.next());
    let pattern: String = match (first, second) {
        (None, _) => return moment(value, "G", culture),
        (Some(letter), None) => {
            let utc = |value: Moment| -> Option<Moment> {
                Some(Moment {
                    local: match value.offset {
                        Some(minutes) => DateTime2::from_ticks(
                            value.local.ticks() - i64::from(minutes) * 600_000_000,
                        )
                        .ok()?,
                        None => value.local,
                    },
                    offset: None,
                })
            };
            return Some(match letter {
                'd' => custom_moment(value, culture.short_date(), culture)?,
                'D' => custom_moment(value, culture.long_date(), culture)?,
                'f' => custom_moment(
                    value,
                    &format!("{} {}", culture.long_date(), culture.short_time()),
                    culture,
                )?,
                'F' => custom_moment(
                    value,
                    &format!("{} {}", culture.long_date(), culture.long_time()),
                    culture,
                )?,
                'g' => custom_moment(
                    value,
                    &format!("{} {}", culture.short_date(), culture.short_time()),
                    culture,
                )?,
                'G' => custom_moment(
                    value,
                    &format!("{} {}", culture.short_date(), culture.long_time()),
                    culture,
                )?,
                'm' | 'M' => custom_moment(value, culture.month_day(), culture)?,
                'o' | 'O' => {
                    let pattern = if value.offset.is_some() {
                        "yyyy'-'MM'-'dd'T'HH':'mm':'ss'.'fffffffzzz"
                    } else {
                        "yyyy'-'MM'-'dd'T'HH':'mm':'ss'.'fffffff"
                    };
                    custom_moment(value, pattern, Culture::INVARIANT)?
                }
                'r' | 'R' => custom_moment(
                    utc(value)?,
                    "ddd, dd MMM yyyy HH':'mm':'ss 'GMT'",
                    Culture::INVARIANT,
                )?,
                's' => custom_moment(value, "yyyy'-'MM'-'dd'T'HH':'mm':'ss", Culture::INVARIANT)?,
                't' => custom_moment(value, culture.short_time(), culture)?,
                'T' => custom_moment(value, culture.long_time(), culture)?,
                'u' => custom_moment(
                    utc(value)?,
                    "yyyy'-'MM'-'dd HH':'mm':'ss'Z'",
                    Culture::INVARIANT,
                )?,
                'U' => {
                    if value.offset.is_some() {
                        return None;
                    }
                    custom_moment(
                        value,
                        &format!("{} {}", culture.long_date(), culture.long_time()),
                        culture,
                    )?
                }
                'y' | 'Y' => custom_moment(value, culture.year_month(), culture)?,
                _ => return None,
            });
        }
        _ => format.into(),
    };
    custom_moment(value, &pattern, culture)
}

fn custom_moment(value: Moment, format: &str, culture: Culture) -> Option<String> {
    let format = format
        .strip_prefix('%')
        .filter(|f| f.chars().count() == 1)
        .unwrap_or(format);
    let parts = value.local.parts();
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let run = |i: usize, c: char| {
        let mut n = 1;
        while chars.get(i + n) == Some(&c) {
            n += 1;
        }
        n
    };
    while i < chars.len() {
        let c = chars[i];
        let n = run(i, c);
        match c {
            'd' => {
                match n {
                    1 => out.push_str(&parts.day.to_string()),
                    2 => out.push_str(&format!("{:02}", parts.day)),
                    3 => out.push_str(&DAYS[weekday(value.local)][..3]),
                    _ => out.push_str(DAYS[weekday(value.local)]),
                }
                i += n;
            }
            'f' | 'F' => {
                if n > 7 {
                    return None;
                }
                let digits = format!("{:07}", parts.fraction);
                let mut text = digits[..n].to_owned();
                if c == 'F' {
                    while text.ends_with('0') {
                        text.pop();
                    }
                    if text.is_empty() && out.ends_with('.') {
                        out.pop();
                    }
                }
                out.push_str(&text);
                i += n;
            }
            'g' => {
                out.push_str("A.D.");
                i += n;
            }
            'h' => {
                let hour = match parts.hour % 12 {
                    0 => 12,
                    h => h,
                };
                out.push_str(&if n == 1 {
                    hour.to_string()
                } else {
                    format!("{hour:02}")
                });
                i += n;
            }
            'H' => {
                out.push_str(&if n == 1 {
                    parts.hour.to_string()
                } else {
                    format!("{:02}", parts.hour)
                });
                i += n;
            }
            'K' => {
                if let Some(minutes) = value.offset {
                    out.push_str(&offset_text(minutes, 3));
                }
                i += n;
            }
            'm' => {
                out.push_str(&if n == 1 {
                    parts.minute.to_string()
                } else {
                    format!("{:02}", parts.minute)
                });
                i += n;
            }
            'M' => {
                match n {
                    1 => out.push_str(&parts.month.to_string()),
                    2 => out.push_str(&format!("{:02}", parts.month)),
                    3 => out.push_str(&MONTH_NAMES[usize::from(parts.month - 1)][..3]),
                    _ => out.push_str(MONTH_NAMES[usize::from(parts.month - 1)]),
                }
                i += n;
            }
            's' => {
                out.push_str(&if n == 1 {
                    parts.second.to_string()
                } else {
                    format!("{:02}", parts.second)
                });
                i += n;
            }
            't' => {
                let designator = if parts.hour < 12 { "AM" } else { "PM" };
                out.push_str(if n == 1 { &designator[..1] } else { designator });
                i += n;
            }
            'y' => {
                let year = u32::from(parts.year);
                out.push_str(&match n {
                    1 => (year % 100).to_string(),
                    2 => format!("{:02}", year % 100),
                    _ => format!("{year:0n$}"),
                });
                i += n;
            }
            'z' => {
                // DATETIME and DATETIME2 are local to the server, which
                // msduck reports as UTC.
                out.push_str(&offset_text(value.offset.unwrap_or(0), n.min(3)));
                i += n;
            }
            ':' => {
                out.push(':');
                i += 1;
            }
            '/' => {
                out.push('/');
                i += 1;
            }
            '\'' | '"' => {
                let close = chars[i + 1..].iter().position(|ch| *ch == c)?;
                out.extend(&chars[i + 1..i + 1 + close]);
                i += close + 2;
            }
            '%' => {
                // A '%' inside a longer format only marks the next character.
                i += 1;
            }
            '\\' => {
                out.push(*chars.get(i + 1)?);
                i += 2;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    let _ = culture;
    Some(out)
}

fn offset_text(minutes: i16, width: usize) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let abs = minutes.unsigned_abs();
    match width {
        1 => format!("{sign}{}", abs / 60),
        2 => format!("{sign}{:02}", abs / 60),
        _ => format!("{sign}{:02}:{:02}", abs / 60, abs % 60),
    }
}

/// Format a TIME value (a .NET TimeSpan) with a standard or custom format.
pub(super) fn span(ticks: i64, format: &str, culture: Culture) -> Option<String> {
    let _ = culture;
    let total_seconds = ticks / 10_000_000;
    let fraction = ticks % 10_000_000;
    let days = total_seconds / 86_400;
    let hours = total_seconds / 3600 % 24;
    let minutes = total_seconds / 60 % 60;
    let seconds = total_seconds % 60;
    let fraction_text = format!("{fraction:07}");
    match format {
        "" | "c" | "t" | "T" => {
            let mut out = String::new();
            if days > 0 {
                out.push_str(&format!("{days}."));
            }
            out.push_str(&format!("{hours:02}:{minutes:02}:{seconds:02}"));
            if fraction > 0 {
                out.push_str(&format!(".{fraction_text}"));
            }
            return Some(out);
        }
        "g" => {
            let mut out = String::new();
            if days > 0 {
                out.push_str(&format!("{days}:"));
            }
            out.push_str(&format!("{hours}:{minutes:02}:{seconds:02}"));
            let trimmed = fraction_text.trim_end_matches('0');
            if !trimmed.is_empty() {
                out.push_str(&format!(".{trimmed}"));
            }
            return Some(out);
        }
        "G" => {
            return Some(format!(
                "{days}:{hours:02}:{minutes:02}:{seconds:02}.{fraction_text}"
            ));
        }
        _ if format.chars().count() == 1 => return None,
        _ => {}
    }
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut n = 1;
        while chars.get(i + n) == Some(&c) {
            n += 1;
        }
        match c {
            'd' => {
                if n > 8 {
                    return None;
                }
                out.push_str(&format!("{days:0n$}"));
            }
            'h' | 'm' | 's' => {
                if n > 2 {
                    return None;
                }
                let value = match c {
                    'h' => hours,
                    'm' => minutes,
                    _ => seconds,
                };
                out.push_str(&if n == 1 {
                    value.to_string()
                } else {
                    format!("{value:02}")
                });
            }
            'f' | 'F' => {
                if n > 7 {
                    return None;
                }
                let mut text = fraction_text[..n].to_owned();
                if c == 'F' {
                    while text.ends_with('0') {
                        text.pop();
                    }
                }
                out.push_str(&text);
            }
            '\'' | '"' => {
                let close = chars[i + 1..].iter().position(|ch| *ch == c)?;
                out.extend(&chars[i + 1..i + 1 + close]);
                i += close + 2;
                continue;
            }
            '\\' => {
                out.push(*chars.get(i + 1)?);
                i += 2;
                continue;
            }
            '%' => {
                i += 1;
                continue;
            }
            _ => return None,
        }
        i += n;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: Culture = Culture::EN_US;
    const IV: Culture = Culture::INVARIANT;

    #[test]
    fn cultures_follow_dotnet_names() {
        assert_eq!(culture("en-US"), Ok(US));
        assert_eq!(culture("EN-us"), Ok(US));
        assert_eq!(culture("en_US"), Ok(US));
        assert_eq!(culture("en"), Ok(US));
        assert_eq!(culture("iv"), Ok(IV));
        assert_eq!(culture("xx-XX"), Ok(IV));
        assert_eq!(culture("tlh"), Ok(IV));
        assert_eq!(culture("de-DE"), Err(CultureError::Unsupported));
        assert_eq!(culture("en-GB"), Err(CultureError::Unsupported));
        for invalid in [
            "", "x", "abcdefgh", "Klingo", "klingon", "en-USA", "C", "POSIX", "en-US ", " de-DE",
            "1031",
        ] {
            assert_eq!(culture(invalid), Err(CultureError::Invalid), "{invalid:?}");
        }
        assert_eq!(culture("en-US-x-test"), Ok(US));
        assert_eq!(culture("zh-Hans"), Err(CultureError::Unsupported));
    }

    #[test]
    fn numeric_standard_formats() {
        let int = |v| Number::Integer(v, 32);
        let dec = Number::Decimal(-12345678, 4);
        let float = Number::Double(0.1);
        let cases: &[(Number, &str, &str)] = &[
            (int(1234), "C", "$1,234.00"),
            (int(-1234), "C0", "($1,234)"),
            (dec, "C", "($1,234.57)"),
            (int(1234), "D10", "0000001234"),
            (int(-1234), "D10", "-0000001234"),
            (int(1234), "E", "1.234000E+003"),
            (int(1234), "e2", "1.23e+003"),
            (float, "E", "1.000000E-001"),
            (int(1234), "F3", "1234.000"),
            (dec, "F", "-1234.57"),
            (int(1234), "G", "1234"),
            (dec, "G", "-1234.5678"),
            (dec, "G5", "-1234.6"),
            (float, "G", "0.1"),
            (int(-1234), "N", "-1,234.00"),
            (int(1234), "P1", "123,400.0%"),
            (float, "P", "10.00%"),
            (float, "R", "0.1"),
            (int(1234), "X", "4D2"),
            (int(-1234), "x8", "fffffb2e"),
            (Number::Integer(-32768, 16), "X", "8000"),
            (Number::Integer(255, 8), "X", "FF"),
            (Number::Decimal(150000, 5), "G", "1.50000"),
            (
                Number::Double(1.23456789012346e20),
                "G",
                "1.23456789012346E+20",
            ),
            (Number::Double(1.5e-7), "G", "1.5E-07"),
            (Number::Double(-0.0), "G", "0"),
            (
                Number::Double(1.2345678901234567e20),
                "N2",
                "123,456,789,012,346,000,000.00",
            ),
            (Number::Decimal(12345, 1), "N0", "1,235"),
        ];
        for (value, format, expected) in cases {
            assert_eq!(
                number(*value, format, US).as_deref(),
                Some(*expected),
                "{value:?} {format}"
            );
        }
        assert_eq!(number(int(1234), "D", US).as_deref(), Some("1234"));
        assert_eq!(number(dec, "D", US), None);
        assert_eq!(number(int(1), "R", US), None);
        assert_eq!(number(int(1), "B", US), None);
        assert_eq!(number(dec, "C", IV).as_deref(), Some("(\u{a4}1,234.57)"));
        assert_eq!(
            number(Number::Decimal(-12345678951, 4), "P", IV).as_deref(),
            Some("-123,456,789.51 %")
        );
        assert_eq!(number(int(1234), "", US).as_deref(), Some("1234"));
        assert_eq!(number(int(-1234), " ", US).as_deref(), Some("- "));
        assert_eq!(number(int(1234), "N100", US).as_deref(), Some("N11234"));
    }

    #[test]
    fn numeric_custom_formats() {
        let positive = Number::Decimal(1234567891, 3);
        let negative = Number::Decimal(-1234567891, 3);
        let zero = Number::Decimal(0, 3);
        let tiny = Number::Double(0.000123);
        let cases: &[(&str, [&str; 4])] = &[
            (
                "#,##0.00",
                ["1,234,567.89", "-1,234,567.89", "0.00", "0.00"],
            ),
            (
                "0000000000",
                ["0001234568", "-0001234568", "0000000000", "0000000000"],
            ),
            ("#.##", ["1234567.89", "-1234567.89", "", ""]),
            ("#,##0,", ["1,235", "-1,235", "0", "0"]),
            ("0,,.0", ["1.2", "-1.2", "0.0", "0.0"]),
            (
                "0.00;(0.00);zero",
                ["1234567.89", "(1234567.89)", "zero", "zero"],
            ),
            ("0.0;neg", ["1234567.9", "neg", "0.0", "0.0"]),
            ("0.0e+00", ["1.2e+06", "-1.2e+06", "0.0e+00", "1.2e-04"]),
            ("0.00E0", ["1.23E6", "-1.23E6", "0.00E0", "1.23E-4"]),
            ("0.0%", ["123456789.1%", "-123456789.1%", "0.0%", "0.0%"]),
            (
                "0.0\u{2030}",
                [
                    "1234567891.0\u{2030}",
                    "-1234567891.0\u{2030}",
                    "0.0\u{2030}",
                    "0.1\u{2030}",
                ],
            ),
            ("'#'0'!'", ["#0!", "-#0!", "#0!", "#0!"]),
            ("\\#0", ["#1234568", "-#1234568", "#0", "#0"]),
            ("\"n=\"0", ["n=0", "-n=0", "n=0", "n=0"]),
            ("abc", ["abc", "-abc", "abc", "abc"]),
            ("#", ["1234568", "-1234568", "", ""]),
        ];
        for (format, expected) in cases {
            for (value, expected) in [positive, negative, zero, tiny].into_iter().zip(expected) {
                assert_eq!(
                    number(value, format, US).as_deref(),
                    Some(*expected),
                    "{value:?} {format}"
                );
            }
        }
    }

    #[test]
    fn date_formats() {
        let value = Moment {
            local: DateTime2::parse_iso("2024-03-05T14:07:09.1234567").unwrap(),
            offset: None,
        };
        for (format, expected) in [
            ("d", "3/5/2024"),
            ("D", "Tuesday, March 5, 2024"),
            ("f", "Tuesday, March 5, 2024 2:07 PM"),
            ("F", "Tuesday, March 5, 2024 2:07:09 PM"),
            ("g", "3/5/2024 2:07 PM"),
            ("G", "3/5/2024 2:07:09 PM"),
            ("M", "March 5"),
            ("O", "2024-03-05T14:07:09.1234567"),
            ("R", "Tue, 05 Mar 2024 14:07:09 GMT"),
            ("s", "2024-03-05T14:07:09"),
            ("t", "2:07 PM"),
            ("T", "2:07:09 PM"),
            ("u", "2024-03-05 14:07:09Z"),
            ("U", "Tuesday, March 5, 2024 2:07:09 PM"),
            ("Y", "March 2024"),
            ("yyyy-MM-dd HH:mm:ss.fffffff", "2024-03-05 14:07:09.1234567"),
            ("ddd MMM", "Tue Mar"),
            ("h:mm:ss tt", "2:07:09 PM"),
            ("yy", "24"),
            ("%d", "5"),
            ("gg yyyy", "A.D. 2024"),
            ("zzz", "+00:00"),
            ("'at' HH'h'", "at 14h"),
            ("HH\\:mm", "14:07"),
            ("N2", "N2"),
        ] {
            assert_eq!(
                moment(value, format, US).as_deref(),
                Some(expected),
                "{format}"
            );
        }
        assert_eq!(moment(value, "X", US), None);
        assert_eq!(moment(value, "z", US), None);
        assert_eq!(moment(value, "d", IV).as_deref(), Some("03/05/2024"));
        assert_eq!(
            moment(value, "D", IV).as_deref(),
            Some("Tuesday, 05 March 2024")
        );
        assert_eq!(moment(value, "t", IV).as_deref(), Some("14:07"));
        let offset = Moment {
            local: value.local,
            offset: Some(-330),
        };
        assert_eq!(
            moment(offset, "O", US).as_deref(),
            Some("2024-03-05T14:07:09.1234567-05:30")
        );
        assert_eq!(
            moment(offset, "R", US).as_deref(),
            Some("Tue, 05 Mar 2024 19:37:09 GMT")
        );
        assert_eq!(
            moment(offset, "u", US).as_deref(),
            Some("2024-03-05 19:37:09Z")
        );
        assert_eq!(moment(offset, "U", US), None);
        assert_eq!(moment(offset, "zzz", US).as_deref(), Some("-05:30"));
    }

    #[test]
    fn time_formats() {
        let ticks = ((14 * 60 + 7) * 60 + 9) * 10_000_000 + 1_234_567;
        assert_eq!(span(ticks, "c", US).as_deref(), Some("14:07:09.1234567"));
        assert_eq!(span(ticks, "t", US).as_deref(), Some("14:07:09.1234567"));
        assert_eq!(span(ticks, "g", US).as_deref(), Some("14:07:09.1234567"));
        assert_eq!(span(ticks, "G", US).as_deref(), Some("0:14:07:09.1234567"));
        assert_eq!(span(ticks, "hh\\:mm\\:ss", US).as_deref(), Some("14:07:09"));
        assert_eq!(span(ticks, "hh':'mm", US).as_deref(), Some("14:07"));
        assert_eq!(span(ticks, "fffffff", US).as_deref(), Some("1234567"));
        assert_eq!(span(ticks, "hh:mm", US), None);
        assert_eq!(span(ticks, "HH\\:mm", US), None);
        assert_eq!(span(ticks, "D", US), None);
        assert_eq!(span(0, "c", US).as_deref(), Some("00:00:00"));
        assert_eq!(span(0, "g", US).as_deref(), Some("0:00:00"));
        assert_eq!(span(0, "G", US).as_deref(), Some("0:00:00:00.0000000"));
    }
}
