//! Deterministic CHARINDEX and PATINDEX rules captured in
//! `reference/charindex-patindex.json`.
//!
//! Evaluation has two stages. [`resolve_charindex`] and [`resolve_patindex`]
//! check arity and argument types before any column metadata exists and select
//! the INT or BIGINT result. The resolved signature then evaluates one row of
//! already-evaluated operands under one already-resolved collation. Collation
//! precedence and conflicts (error 468), the ESCAPE syntax error and textual
//! conversion of INT, DECIMAL, DATETIME and UNIQUEIDENTIFIER search values
//! belong to the caller.
//!
//! Only the captured collation families are modelled. Linguistic comparisons
//! are limited to printable ASCII, Latin-1 letters that are an ASCII base
//! letter with one diacritic, and the captured supplementary character under
//! `_SC` collations. Every other character, lower-level range tie and
//! uncaptured type combination is reported as [`Evaluation::Unsupported`] or
//! [`Rejection::Unsupported`] instead of guessed.

use std::cmp::Ordering;

/// Argument type as bound by the caller. `Null` is an untyped NULL literal;
/// a typed NULL uses its declared type. MAX variants are the MAX forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgType {
    Null,
    Char,
    VarChar,
    VarCharMax,
    NChar,
    NVarChar,
    NVarCharMax,
    Text,
    NText,
    Binary,
    VarBinary,
    VarBinaryMax,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Decimal,
    Float,
    Money,
    Bit,
    Date,
    DateTime,
    UniqueIdentifier,
    Xml,
    /// Any type without a captured rule for either function.
    Other,
}

impl ArgType {
    /// Type name as SQL Server spells it in error 8116.
    fn error_name(self) -> Option<&'static str> {
        Some(match self {
            Self::Null => "NULL",
            Self::VarChar => "varchar",
            Self::VarBinary => "varbinary",
            Self::Int => "int",
            Self::Float => "float",
            Self::Money => "money",
            Self::Bit => "bit",
            Self::Date => "date",
            Self::Xml => "xml",
            _ => return None,
        })
    }

    fn is_max(self) -> bool {
        matches!(
            self,
            Self::VarCharMax | Self::NVarCharMax | Self::VarBinaryMax
        )
    }

    fn is_binary(self) -> bool {
        matches!(self, Self::VarBinary | Self::VarBinaryMax)
    }

    fn is_unicode(self) -> bool {
        matches!(
            self,
            Self::NChar | Self::NVarChar | Self::NVarCharMax | Self::NText
        )
    }
}

/// Declared result of either function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultType {
    /// IntN length 4.
    Int,
    /// IntN length 8, selected only by a MAX searched expression.
    BigInt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerError {
    pub number: i32,
    pub state: u8,
    pub class: u8,
    pub message: String,
}

impl ServerError {
    fn new(number: i32, state: u8, class: u8, message: impl Into<String>) -> Self {
        Self {
            number,
            state,
            class,
            message: message.into(),
        }
    }

    fn invalid_argument(name: &str, position: usize, function: &str) -> Self {
        Self::new(
            8116,
            1,
            16,
            format!(
                "Argument data type {name} is invalid for argument {position} of {function} function."
            ),
        )
    }

    fn int_overflow() -> Self {
        Self::new(
            8115,
            2,
            16,
            "Arithmetic overflow error converting expression to data type int.",
        )
    }
}

/// A compile-time rejection, raised before any column metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Rejection {
    Error(ServerError),
    Unsupported(&'static str),
}

/// One evaluated row. `Error` is raised after the column descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Evaluation {
    Value(Option<i64>),
    Error(ServerError),
    Unsupported(&'static str),
}

/// Non-NULL string or binary operand. Text is UTF-16 code units; the caller
/// decodes non-Unicode values from their code page and converts INT, DECIMAL,
/// DATETIME and UNIQUEIDENTIFIER search values to their VARCHAR text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operand<'a> {
    Text(&'a [u16]),
    Binary(&'a [u8]),
}

/// Non-NULL CHARINDEX start value. Integer covers TINYINT through BIGINT.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Start {
    Integer(i64),
    /// DECIMAL/NUMERIC as unscaled value and scale.
    Decimal {
        unscaled: i128,
        scale: u8,
    },
}

/// Third CHARINDEX argument for one row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartArgument {
    Omitted,
    Null,
    Value(Start),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    /// `_BIN2`: code unit identity and order.
    Binary,
    /// Windows or SQL linguistic comparison over the modelled characters.
    Linguistic {
        case_sensitive: bool,
        accent_sensitive: bool,
    },
}

/// The resolved collation of the operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Collation {
    pub comparison: Comparison,
    /// `_SC`: positions and `_` count code points instead of UTF-16 units.
    pub supplementary: bool,
}

impl Collation {
    /// The captured server and database default.
    pub const DEFAULT: Self = Self {
        comparison: Comparison::Linguistic {
            case_sensitive: false,
            accent_sensitive: true,
        },
        supplementary: false,
    };

    /// Recognizes the Latin1 collation families this module models:
    /// `SQL_Latin1_General_CP1_{CI|CS}_{AS|AI}`,
    /// `Latin1_General[_100]_{CI|CS}_{AS|AI}`, `Latin1_General_100_.._SC`,
    /// `Latin1_General_100_.._SC_UTF8` and `Latin1_General[_100]_BIN2`.
    /// Kana, width and variation-selector sensitive, `_BIN` and other
    /// language collations return `None`. Names match ASCII case-insensitively,
    /// like the other collation identity comparisons in msduck-core; callers
    /// pass names that binding has already accepted.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_uppercase();
        let (rest, version_100) = if let Some(rest) = name.strip_prefix("SQL_LATIN1_GENERAL_CP1_") {
            (rest, None)
        } else if let Some(rest) = name.strip_prefix("LATIN1_GENERAL_100_") {
            (rest, Some(true))
        } else if let Some(rest) = name.strip_prefix("LATIN1_GENERAL_") {
            (rest, Some(false))
        } else {
            return None;
        };
        if rest == "BIN2" {
            return version_100.map(|_| Self {
                comparison: Comparison::Binary,
                supplementary: false,
            });
        }
        let mut parts = rest.split('_');
        let case_sensitive = match parts.next()? {
            "CI" => false,
            "CS" => true,
            _ => return None,
        };
        let accent_sensitive = match parts.next()? {
            "AI" => false,
            "AS" => true,
            _ => return None,
        };
        let suffix: Vec<&str> = parts.collect();
        let supplementary = match suffix.as_slice() {
            [] => false,
            ["SC"] | ["SC", "UTF8"] if version_100 == Some(true) => true,
            _ => return None,
        };
        Some(Self {
            comparison: Comparison::Linguistic {
                case_sensitive,
                accent_sensitive,
            },
            supplementary,
        })
    }
}

const SPACE: u32 = 0x20;
/// Longest captured find or pattern, in stored bytes.
const CAPTURED_FIND_BYTES: usize = 8000;

/// Whether a find or pattern may exceed the captured 8000 bytes. Unicode
/// text is two bytes per UTF-16 unit. Non-Unicode text is measured as UTF-8,
/// an upper bound for both the Latin1 code page and `_UTF8` collations, so
/// values near the limit may be reported unsupported conservatively.
fn overlong(kind: ArgType, operand: Operand<'_>) -> bool {
    let bytes = match operand {
        Operand::Binary(bytes) => bytes.len(),
        Operand::Text(units) if kind.is_unicode() => units.len().saturating_mul(2),
        Operand::Text(units) => char::decode_utf16(units.iter().copied())
            .map(|decoded| decoded.map_or(3, char::len_utf8))
            .sum(),
    };
    bytes > CAPTURED_FIND_BYTES
}
const CAPTURED_SUPPLEMENTARY: u32 = 0x1F600;

fn unsupported_type(position: usize) -> Rejection {
    Rejection::Unsupported(match position {
        1 => "argument 1 type has no captured rule",
        2 => "argument 2 type has no captured rule",
        _ => "argument 3 type has no captured rule",
    })
}

/// Combines per-argument verdicts. A single captured rejection is reported;
/// several rejections have no captured precedence.
fn admissibility(verdicts: &[Result<(), Rejection>]) -> Result<(), Rejection> {
    let mut error = None;
    for verdict in verdicts {
        match verdict {
            Ok(()) => {}
            Err(Rejection::Unsupported(reason)) => return Err(Rejection::Unsupported(reason)),
            Err(Rejection::Error(found)) => {
                if error.is_some() {
                    return Err(Rejection::Unsupported(
                        "several invalid arguments have no captured precedence",
                    ));
                }
                error = Some(found.clone());
            }
        }
    }
    error.map_or(Ok(()), |error| Err(Rejection::Error(error)))
}

fn result_type(search: ArgType) -> ResultType {
    if search.is_max() {
        ResultType::BigInt
    } else {
        ResultType::Int
    }
}

/// A CHARINDEX call whose arity and argument types were accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Charindex {
    find: ArgType,
    search: ArgType,
    start: Option<ArgType>,
}

pub fn resolve_charindex(args: &[ArgType]) -> Result<Charindex, Rejection> {
    if !(2..=3).contains(&args.len()) {
        return Err(Rejection::Error(ServerError::new(
            189,
            1,
            15,
            "The charindex function requires 2 to 3 arguments.",
        )));
    }
    let (find, search, start) = (args[0], args[1], args.get(2).copied());
    let find_verdict = match find {
        ArgType::Null
        | ArgType::VarChar
        | ArgType::VarCharMax
        | ArgType::NVarChar
        | ArgType::NVarCharMax
        | ArgType::VarBinary => Ok(()),
        ArgType::Int => Err(Rejection::Error(ServerError::invalid_argument(
            "int",
            1,
            "charindex",
        ))),
        _ => Err(unsupported_type(1)),
    };
    let search_verdict = match search {
        ArgType::Null
        | ArgType::Char
        | ArgType::VarChar
        | ArgType::VarCharMax
        | ArgType::NVarChar
        | ArgType::NVarCharMax
        | ArgType::Text
        | ArgType::NText
        | ArgType::VarBinary
        | ArgType::VarBinaryMax
        | ArgType::Int
        | ArgType::Decimal
        | ArgType::DateTime
        | ArgType::UniqueIdentifier => Ok(()),
        ArgType::Xml if find == ArgType::VarChar => Err(Rejection::Error(ServerError::new(
            257,
            3,
            16,
            "Implicit conversion from data type xml to varchar is not allowed. Use the CONVERT function to run this query.",
        ))),
        _ => Err(unsupported_type(2)),
    };
    let start_verdict = match start {
        None
        | Some(
            ArgType::Null
            | ArgType::TinyInt
            | ArgType::SmallInt
            | ArgType::Int
            | ArgType::BigInt
            | ArgType::Decimal,
        ) => Ok(()),
        Some(
            kind @ (ArgType::Float
            | ArgType::VarChar
            | ArgType::Bit
            | ArgType::Date
            | ArgType::Money),
        ) => Err(Rejection::Error(ServerError::invalid_argument(
            kind.error_name().unwrap_or_default(),
            3,
            "charindex",
        ))),
        Some(_) => Err(unsupported_type(3)),
    };
    admissibility(&[find_verdict, search_verdict, start_verdict])?;
    if find.is_binary() && !matches!(search, ArgType::Null) && !search.is_binary()
        || search.is_binary() && !matches!(find, ArgType::Null) && !find.is_binary()
    {
        return Err(Rejection::Unsupported(
            "mixed binary and character operands have no captured rule",
        ));
    }
    Ok(Charindex {
        find,
        search,
        start,
    })
}

impl Charindex {
    pub fn result_type(&self) -> ResultType {
        result_type(self.search)
    }

    /// Evaluates one row. `None` operands are NULL.
    pub fn evaluate(
        &self,
        find: Option<Operand<'_>>,
        search: Option<Operand<'_>>,
        start: StartArgument,
        collation: Collation,
    ) -> Evaluation {
        if find.is_some_and(|find| overlong(self.find, find)) {
            return Evaluation::Unsupported("a find value beyond 8000 bytes was not captured");
        }
        let start = match (self.start, start) {
            (None, StartArgument::Omitted) => None,
            (Some(_), StartArgument::Null) => return Evaluation::Value(None),
            (Some(_), StartArgument::Value(start)) => Some(start),
            _ => return Evaluation::Unsupported("start argument does not match the signature"),
        };
        let start = match start.map(|start| self.normalize_start(start)) {
            None => 1,
            Some(Ok(start)) => start,
            Some(Err(Evaluation::Error(error))) => {
                return if find.is_none() || search.is_none() || is_empty(find) {
                    Evaluation::Unsupported(
                        "start overflow with a NULL or empty operand has no captured order",
                    )
                } else {
                    Evaluation::Error(error)
                };
            }
            Some(Err(other)) => return other,
        };
        let (Some(find), Some(search)) = (find, search) else {
            return Evaluation::Value(None);
        };
        match (find, search) {
            (Operand::Binary(find), Operand::Binary(search)) if self.binary() => Evaluation::Value(
                Some(substring_position(find, search, start, |a, b| Ok(a == b)).unwrap_or(0)),
            ),
            (Operand::Text(find), Operand::Text(search)) if !self.binary() => {
                if find.is_empty() {
                    return Evaluation::Value(Some(0));
                }
                let comparer = Comparer::new(collation, false);
                let (find, search) = match (comparer.characters(find), comparer.characters(search))
                {
                    (Ok(find), Ok(search)) => (find, search),
                    (Err(reason), _) | (_, Err(reason)) => {
                        return Evaluation::Unsupported(reason);
                    }
                };
                match substring_position(&find, &search, start, |a, b| comparer.equal(*a, *b)) {
                    Ok(position) => Evaluation::Value(Some(position)),
                    Err(reason) => Evaluation::Unsupported(reason),
                }
            }
            _ => Evaluation::Unsupported("operand kind does not match the signature"),
        }
    }

    fn binary(&self) -> bool {
        self.find.is_binary() || self.search.is_binary()
    }

    /// One-based start: nonpositive searches from 1. Non-MAX searches
    /// convert the start to INT first; MAX searches keep BIGINT.
    fn normalize_start(&self, start: Start) -> Result<i64, Evaluation> {
        let value = match start {
            Start::Integer(value) => value,
            Start::Decimal { unscaled, scale } => {
                if self.search.is_max() {
                    return Err(Evaluation::Unsupported(
                        "DECIMAL start with a MAX search was not captured",
                    ));
                }
                if scale > 38 {
                    return Err(Evaluation::Unsupported("DECIMAL scale beyond 38"));
                }
                // Conversion to INT truncates toward zero.
                let truncated = unscaled / 10_i128.pow(u32::from(scale));
                i64::try_from(truncated).unwrap_or(if truncated < 0 { i64::MIN } else { i64::MAX })
            }
        };
        if !self.search.is_max() && i32::try_from(value).is_err() {
            return Err(Evaluation::Error(ServerError::int_overflow()));
        }
        Ok(value.max(1))
    }
}

fn is_empty(operand: Option<Operand<'_>>) -> bool {
    match operand {
        Some(Operand::Text(text)) => text.is_empty(),
        Some(Operand::Binary(bytes)) => bytes.is_empty(),
        None => false,
    }
}

/// First one-based position at or after `start` where `find` occurs, else 0.
fn substring_position<T>(
    find: &[T],
    search: &[T],
    start: i64,
    equal: impl Fn(&T, &T) -> Result<bool, &'static str>,
) -> Result<i64, &'static str> {
    if find.is_empty() || find.len() > search.len() {
        return Ok(0);
    }
    let first = usize::try_from(start - 1).unwrap_or(usize::MAX);
    let last = search.len() - find.len();
    if first > last {
        return Ok(0);
    }
    'candidates: for index in first..=last {
        for (offset, wanted) in find.iter().enumerate() {
            if !equal(wanted, &search[index + offset])? {
                continue 'candidates;
            }
        }
        return Ok(i64::try_from(index + 1).unwrap_or(i64::MAX));
    }
    Ok(0)
}

/// Trailing-space handling of PATINDEX, chosen by the searched type family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrailingSpaces {
    /// VARCHAR/CHAR with a non-Unicode pattern: trailing search spaces are ignored.
    Ignored,
    /// NVARCHAR with an NVARCHAR pattern: trailing search spaces are significant.
    Significant,
    /// Other combinations were not captured with trailing search spaces.
    Uncaptured,
}

/// A PATINDEX call whose arity and argument types were accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Patindex {
    pattern: ArgType,
    search: ArgType,
}

pub fn resolve_patindex(args: &[ArgType]) -> Result<Patindex, Rejection> {
    if args.len() != 2 {
        return Err(Rejection::Error(ServerError::new(
            174,
            1,
            15,
            "The patindex function requires 2 argument(s).",
        )));
    }
    let (pattern, search) = (args[0], args[1]);
    let pattern_verdict = match pattern {
        ArgType::Null
        | ArgType::VarChar
        | ArgType::VarCharMax
        | ArgType::NVarChar
        | ArgType::Int => Ok(()),
        _ => Err(unsupported_type(1)),
    };
    let search_verdict = match search {
        ArgType::Char
        | ArgType::VarChar
        | ArgType::VarCharMax
        | ArgType::NVarChar
        | ArgType::NVarCharMax
        | ArgType::Text
        | ArgType::NText => Ok(()),
        kind @ (ArgType::Null | ArgType::Int | ArgType::VarBinary | ArgType::Xml) => {
            Err(Rejection::Error(ServerError::invalid_argument(
                kind.error_name().unwrap_or_default(),
                2,
                "patindex",
            )))
        }
        _ => Err(unsupported_type(2)),
    };
    admissibility(&[pattern_verdict, search_verdict])?;
    Ok(Patindex { pattern, search })
}

impl Patindex {
    pub fn result_type(&self) -> ResultType {
        result_type(self.search)
    }

    fn trailing_spaces(&self) -> TrailingSpaces {
        match (self.pattern, self.search) {
            (ArgType::VarChar | ArgType::Int, ArgType::VarChar | ArgType::Char) => {
                TrailingSpaces::Ignored
            }
            (ArgType::NVarChar, ArgType::NVarChar) => TrailingSpaces::Significant,
            _ => TrailingSpaces::Uncaptured,
        }
    }

    /// Evaluates one row. `None` operands are NULL; text is UTF-16 units.
    pub fn evaluate(
        &self,
        pattern: Option<&[u16]>,
        search: Option<&[u16]>,
        collation: Collation,
    ) -> Evaluation {
        if pattern.is_some_and(|pattern| overlong(self.pattern, Operand::Text(pattern))) {
            return Evaluation::Unsupported("a pattern beyond 8000 bytes was not captured");
        }
        let (Some(pattern), Some(search)) = (pattern, search) else {
            return Evaluation::Value(None);
        };
        let unicode = self.pattern.is_unicode() || self.search.is_unicode();
        let comparer = Comparer::new(collation, !unicode);
        let (pattern, search) = match (comparer.characters(pattern), comparer.characters(search)) {
            (Ok(pattern), Ok(search)) => (pattern, search),
            (Err(reason), _) | (_, Err(reason)) => return Evaluation::Unsupported(reason),
        };
        let ignore_trailing = match self.trailing_spaces() {
            TrailingSpaces::Ignored => true,
            TrailingSpaces::Significant => false,
            TrailingSpaces::Uncaptured if search.last() == Some(&SPACE) => {
                return Evaluation::Unsupported(
                    "trailing search spaces for this type combination were not captured",
                );
            }
            TrailingSpaces::Uncaptured => false,
        };
        let elements = match parse_pattern(&pattern) {
            Ok(elements) => elements,
            Err(reason) => return Evaluation::Unsupported(reason),
        };
        let matcher = Matcher {
            elements: &elements,
            search: &search,
            comparer: &comparer,
            tail: if ignore_trailing {
                search.len() - search.iter().rev().take_while(|&&c| c == SPACE).count()
            } else {
                search.len()
            },
        };
        match matcher.position() {
            Ok(position) => Evaluation::Value(Some(position)),
            Err(reason) => Evaluation::Unsupported(reason),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClassItem {
    Single(u32),
    Range(u32, u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Element {
    /// `%`
    Any,
    /// `_`
    One,
    Literal(u32),
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    /// An unclosed `[`: never matches.
    Never,
}

const PERCENT: u32 = '%' as u32;
const UNDERSCORE: u32 = '_' as u32;
const OPEN: u32 = '[' as u32;
const CLOSE: u32 = ']' as u32;
const CARET: u32 = '^' as u32;
const DASH: u32 = '-' as u32;

fn parse_pattern(pattern: &[u32]) -> Result<Vec<Element>, &'static str> {
    let mut elements = Vec::new();
    let mut index = 0;
    while index < pattern.len() {
        let c = pattern[index];
        index += 1;
        match c {
            PERCENT => {
                if elements.last() != Some(&Element::Any) {
                    elements.push(Element::Any);
                }
            }
            UNDERSCORE => elements.push(Element::One),
            OPEN => {
                let negated = pattern.get(index) == Some(&CARET);
                if negated {
                    index += 1;
                }
                let mut items = Vec::new();
                let mut closed = false;
                while index < pattern.len() {
                    let c = pattern[index];
                    index += 1;
                    if c == CLOSE {
                        closed = true;
                        break;
                    }
                    if c == DASH && matches!(items.last(), Some(ClassItem::Range(..))) {
                        return Err("a dash after a bracket range was not captured");
                    }
                    let is_range = pattern.get(index) == Some(&DASH)
                        && pattern.get(index + 1).is_some_and(|&next| next != CLOSE);
                    if is_range {
                        let high = pattern[index + 1];
                        if c == DASH || high == DASH {
                            return Err("a dash range endpoint was not captured");
                        }
                        items.push(ClassItem::Range(c, high));
                        index += 2;
                    } else {
                        items.push(ClassItem::Single(c));
                    }
                }
                if !closed {
                    if negated {
                        return Err("an unclosed negated bracket was not captured");
                    }
                    elements.push(Element::Never);
                    // The rest of the pattern is inside the unclosed bracket.
                    break;
                }
                if negated && items.is_empty() {
                    return Err("an empty negated bracket was not captured");
                }
                elements.push(Element::Class { negated, items });
            }
            _ => elements.push(Element::Literal(c)),
        }
    }
    Ok(elements)
}

struct Matcher<'a> {
    elements: &'a [Element],
    search: &'a [u32],
    comparer: &'a Comparer,
    /// The match may end at or after this index (trailing-space rule).
    tail: usize,
}

impl Matcher<'_> {
    /// One-based position of the first match, 0 when none.
    fn position(&self) -> Result<i64, &'static str> {
        let Some(Element::Any) = self.elements.first() else {
            return Ok(if self.anchored(0, 0)? { 1 } else { 0 });
        };
        if self.elements.len() == 1 {
            return Ok(1);
        }
        for start in 0..self.search.len() {
            if self.anchored(1, start)? {
                return Ok(i64::try_from(start + 1).unwrap_or(i64::MAX));
            }
        }
        Ok(0)
    }

    /// Whether `elements[element..]` matches `search[start..]` to its end.
    /// Every element except `%` consumes exactly one character, so the last
    /// `%` is the only backtracking point needed.
    fn anchored(&self, mut element: usize, mut index: usize) -> Result<bool, &'static str> {
        let mut resume: Option<(usize, usize)> = None;
        loop {
            if let Some(current) = self.elements.get(element) {
                if *current == Element::Any {
                    resume = Some((element + 1, index));
                    element += 1;
                    continue;
                }
                if let Some(&c) = self.search.get(index)
                    && self.single(current, c)?
                {
                    element += 1;
                    index += 1;
                    continue;
                }
            } else if index >= self.tail {
                return Ok(true);
            }
            match resume {
                Some((after, from)) if from < self.search.len() => {
                    resume = Some((after, from + 1));
                    element = after;
                    index = from + 1;
                }
                _ => return Ok(false),
            }
        }
    }

    fn single(&self, element: &Element, c: u32) -> Result<bool, &'static str> {
        Ok(match element {
            Element::Any => true,
            Element::One => true,
            Element::Never => false,
            Element::Literal(literal) => self.comparer.equal(*literal, c)?,
            Element::Class { negated, items } => {
                let mut found = false;
                for item in items {
                    let hit = match *item {
                        ClassItem::Single(single) => self.comparer.equal(single, c)?,
                        ClassItem::Range(low, high) => {
                            self.comparer.order(low, c)? != Ordering::Greater
                                && self.comparer.order(c, high)? != Ordering::Greater
                        }
                    };
                    if hit {
                        found = true;
                        break;
                    }
                }
                found != *negated
            }
        })
    }
}

/// Modelled linguistic weight of one character.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Weight {
    /// Printable ASCII punctuation, symbols and space: distinct characters
    /// with no captured relative order.
    Symbol,
    Digit(u32),
    Letter {
        base: u8,
        accent: u8,
        upper: bool,
    },
    /// A captured supplementary character under an `_SC` collation.
    Opaque,
}

/// Latin-1 letters U+00C0..=U+00FF as (base, accent); 0 means unmodelled
/// (multiplication/division signs and letters that are not an ASCII base
/// letter plus one diacritic, such as Æ, Ð, Ø, Þ and ß).
const LATIN1_LETTERS: [(u8, u8); 64] = [
    (b'a', 1),
    (b'a', 2),
    (b'a', 3),
    (b'a', 4),
    (b'a', 5),
    (b'a', 6),
    (0, 0),
    (b'c', 7),
    (b'e', 1),
    (b'e', 2),
    (b'e', 3),
    (b'e', 5),
    (b'i', 1),
    (b'i', 2),
    (b'i', 3),
    (b'i', 5),
    (0, 0),
    (b'n', 4),
    (b'o', 1),
    (b'o', 2),
    (b'o', 3),
    (b'o', 4),
    (b'o', 5),
    (0, 0),
    (0, 0),
    (b'u', 1),
    (b'u', 2),
    (b'u', 3),
    (b'u', 5),
    (b'y', 2),
    (0, 0),
    (0, 0),
    (b'a', 1),
    (b'a', 2),
    (b'a', 3),
    (b'a', 4),
    (b'a', 5),
    (b'a', 6),
    (0, 0),
    (b'c', 7),
    (b'e', 1),
    (b'e', 2),
    (b'e', 3),
    (b'e', 5),
    (b'i', 1),
    (b'i', 2),
    (b'i', 3),
    (b'i', 5),
    (0, 0),
    (b'n', 4),
    (b'o', 1),
    (b'o', 2),
    (b'o', 3),
    (b'o', 4),
    (b'o', 5),
    (0, 0),
    (0, 0),
    (b'u', 1),
    (b'u', 2),
    (b'u', 3),
    (b'u', 5),
    (b'y', 2),
    (0, 0),
    (b'y', 5),
];

fn weight(c: u32, supplementary: bool) -> Option<Weight> {
    Some(match c {
        0x30..=0x39 => Weight::Digit(c - 0x30),
        0x41..=0x5A | 0x61..=0x7A => Weight::Letter {
            base: u8::try_from(c | 0x20).ok()?,
            accent: 0,
            upper: c < 0x61,
        },
        0x20..=0x7E => Weight::Symbol,
        0xC0..=0xFF => {
            let (base, accent) = LATIN1_LETTERS[usize::try_from(c - 0xC0).ok()?];
            if base == 0 {
                return None;
            }
            Weight::Letter {
                base,
                accent,
                upper: c < 0xE0,
            }
        }
        CAPTURED_SUPPLEMENTARY if supplementary => Weight::Opaque,
        _ => return None,
    })
}

struct Comparer {
    collation: Collation,
    /// Operands are compared as non-Unicode code page text.
    code_page: bool,
}

impl Comparer {
    fn new(collation: Collation, code_page: bool) -> Self {
        Self {
            collation,
            code_page,
        }
    }

    /// Splits UTF-16 units into the characters that positions count.
    fn characters(&self, units: &[u16]) -> Result<Vec<u32>, &'static str> {
        let linguistic = matches!(self.collation.comparison, Comparison::Linguistic { .. });
        let characters: Vec<u32> = if self.collation.supplementary {
            let mut characters = Vec::with_capacity(units.len());
            for decoded in char::decode_utf16(units.iter().copied()) {
                match decoded {
                    Ok(c) => characters.push(u32::from(c)),
                    Err(_) => return Err("unpaired surrogate under a supplementary collation"),
                }
            }
            characters
        } else {
            units.iter().map(|&unit| u32::from(unit)).collect()
        };
        if linguistic
            && characters
                .iter()
                .any(|&c| weight(c, self.collation.supplementary).is_none())
        {
            return Err("character outside the modelled linguistic set");
        }
        Ok(characters)
    }

    fn equal(&self, a: u32, b: u32) -> Result<bool, &'static str> {
        if a == b {
            return Ok(true);
        }
        let Comparison::Linguistic {
            case_sensitive,
            accent_sensitive,
        } = self.collation.comparison
        else {
            return Ok(false);
        };
        let supplementary = self.collation.supplementary;
        let (Some(left), Some(right)) = (weight(a, supplementary), weight(b, supplementary)) else {
            return Err("character outside the modelled linguistic set");
        };
        Ok(match (left, right) {
            (
                Weight::Letter {
                    base: left_base,
                    accent: left_accent,
                    upper: left_upper,
                },
                Weight::Letter {
                    base: right_base,
                    accent: right_accent,
                    upper: right_upper,
                },
            ) => {
                left_base == right_base
                    && (!accent_sensitive || left_accent == right_accent)
                    && (!case_sensitive || left_upper == right_upper)
            }
            _ => false,
        })
    }

    /// Range order. Only primary differences between digits and letters and
    /// code unit order under BIN2 are modelled.
    fn order(&self, a: u32, b: u32) -> Result<Ordering, &'static str> {
        if self.equal(a, b)? {
            return Ok(Ordering::Equal);
        }
        if self.collation.comparison == Comparison::Binary {
            let modelled = |c: u32| {
                if self.code_page {
                    c < 0x80
                } else {
                    !(0xD800..=0xDFFF).contains(&c)
                }
            };
            return if modelled(a) && modelled(b) {
                Ok(a.cmp(&b))
            } else {
                Err("binary range order outside the modelled code units")
            };
        }
        let supplementary = self.collation.supplementary;
        match (weight(a, supplementary), weight(b, supplementary)) {
            (Some(Weight::Digit(left)), Some(Weight::Digit(right))) => Ok(left.cmp(&right)),
            (Some(Weight::Digit(_)), Some(Weight::Letter { .. })) => Ok(Ordering::Less),
            (Some(Weight::Letter { .. }), Some(Weight::Digit(_))) => Ok(Ordering::Greater),
            (Some(Weight::Letter { base: left, .. }), Some(Weight::Letter { base: right, .. }))
                if left != right =>
            {
                Ok(left.cmp(&right))
            }
            (Some(Weight::Letter { .. }), Some(Weight::Letter { .. })) => {
                Err("case or accent order within a letter was not captured")
            }
            _ => Err("range order of symbols was not captured"),
        }
    }
}
