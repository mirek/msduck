//! Deterministic JSON_OBJECT, JSON_ARRAY and JSON_MODIFY rules captured in
//! `reference/json-constructors.json`. Parsing, binding and runtime wiring are
//! separate: callers pass already-evaluated operands and declared types.
//! Behavior the reference capture does not establish returns
//! [`Error::Unsupported`] rather than a guess.
use msduck_core::{
    datetime2::DateTime2, datetimeoffset::DateTimeOffset, diagnostic::SqlError, for_json, json,
    json_escape, json_path, money, value::Decimal,
};

/// A failure, classified by when SQL Server reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Raised while compiling, before any result descriptor.
    Compile(SqlError),
    /// Raised while executing, after the result descriptor and any earlier rows.
    Runtime(SqlError),
    /// Behavior not established by the reference capture.
    Unsupported(&'static str),
}

/// An evaluated operand. Character values arrive decoded, with fixed-length
/// padding applied by the declared length when using [`Scalar::Char`] or
/// [`Scalar::NChar`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scalar<'a> {
    /// A typed or untyped SQL NULL.
    Null,
    TinyInt(u8),
    SmallInt(i16),
    Int(i32),
    BigInt(i64),
    Bit(bool),
    /// DECIMAL or NUMERIC; the declared scale is kept.
    Decimal(Decimal),
    /// MONEY as a coefficient scaled by 10^4.
    Money(i64),
    /// SMALLMONEY as a coefficient scaled by 10^4.
    SmallMoney(i32),
    Float(f64),
    Real(f32),
    /// DATE; any time part is ignored.
    Date(DateTime2),
    /// TIME(scale); the date part is ignored.
    Time(DateTime2, u8),
    /// DATETIME, already on its millisecond display grid.
    DateTime(DateTime2),
    SmallDateTime(DateTime2),
    DateTime2(DateTime2, u8),
    DateTimeOffset(DateTimeOffset, u8),
    /// UNIQUEIDENTIFIER in SQL Server storage (wire) byte order.
    UniqueIdentifier([u8; 16]),
    /// CHAR(length): text padded with spaces to `length` bytes (ASCII only).
    Char(&'a str, u32),
    /// NCHAR(length): text padded with spaces to `length` UTF-16 units.
    NChar(&'a str, u32),
    /// VARCHAR, NVARCHAR and their MAX forms, and JSON_VALUE results.
    Text(&'a str),
    /// BINARY, VARBINARY, their MAX forms and ROWVERSION.
    Binary(&'a [u8]),
    Xml(&'a str),
    /// JSON-kind source: a JSON_OBJECT, JSON_ARRAY, JSON_QUERY or JSON_MODIFY
    /// result, or a native JSON-typed value. Embedded without escaping.
    Json(&'a str),
    /// SQL_VARIANT holding the inner value.
    Variant(&'a Scalar<'a>),
    /// CLR user-defined types such as HIERARCHYID and GEOMETRY.
    Clr,
}

/// Declared operand types relevant to compile-time checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlType {
    /// An untyped `NULL` literal.
    UntypedNull,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Bit,
    Decimal,
    Numeric,
    Money,
    SmallMoney,
    Float,
    Real,
    Date,
    Time,
    DateTime,
    SmallDateTime,
    DateTime2,
    DateTimeOffset,
    UniqueIdentifier,
    Char,
    VarChar,
    NChar,
    NVarChar,
    Binary,
    VarBinary,
    Xml,
    Json,
    SqlVariant,
    Clr,
}

/// `NULL ON NULL` / `ABSENT ON NULL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NullClause {
    NullOnNull,
    AbsentOnNull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constructor {
    Object,
    Array,
}

/// Declared result of a constructor or JSON_MODIFY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultType {
    /// `nvarchar(max)`, nullable, in the database default collation.
    NVarCharMax,
    /// Native JSON; clients without the JSON feature see `varchar(max)` in
    /// `Latin1_General_100_BIN2_UTF8`.
    Json,
}

impl ResultType {
    /// `sp_describe_first_result_set` system type name for a client without
    /// the native JSON feature.
    pub fn system_type_name(self) -> &'static str {
        match self {
            Self::NVarCharMax => "nvarchar(max)",
            Self::Json => "varchar(max)",
        }
    }
    /// The result collation when it does not follow the database default.
    pub fn fixed_collation(self) -> Option<&'static str> {
        match self {
            Self::NVarCharMax => None,
            Self::Json => Some("Latin1_General_100_BIN2_UTF8"),
        }
    }
}

/// Constructors return JSON with `RETURNING JSON` or any JSON-typed argument.
pub fn constructor_result(returning_json: bool, argument_types: &[SqlType]) -> ResultType {
    if returning_json || argument_types.contains(&SqlType::Json) {
        ResultType::Json
    } else {
        ResultType::NVarCharMax
    }
}

pub const NULL_KEY: &str = "User error : Name parameter value in 'json_object' cannot be null";
pub const OBJECT_CLR: &str =
    "json_object and json_objectagg does not support CLR type as parameters";
pub const ARRAY_CLR: &str = "json_array does not support CLR type as parameters";
pub const NULL_MODIFY_PATH: &str =
    "Argument data type NULL is invalid for argument 2 of JSON_MODIFY function.";
pub const ROOT_PATH: &str = "Unsupported JSON path found in argument 2 of JSON_MODIFY.";
pub const WILDCARD_PATH: &str = "JsonModify not yet supported for advanced JSON array accessors.";
pub const NO_ARRAY: &str = "Array cannot be found in the specified JSON path.";
pub const MODIFY_ARITY: &str = "The json_modify function requires 3 argument(s).";

/// Resolve the ON NULL clauses written for a constructor.
/// JSON_OBJECT defaults to `NULL ON NULL`; JSON_ARRAY to `ABSENT ON NULL`.
pub fn null_clause(constructor: Constructor, written: &[NullClause]) -> Result<NullClause, Error> {
    match (constructor, written) {
        (Constructor::Object, []) => Ok(NullClause::NullOnNull),
        (Constructor::Array, []) => Ok(NullClause::AbsentOnNull),
        (_, [clause]) => Ok(*clause),
        (Constructor::Object, [first, second]) if first != second => Err(Error::Compile(
            SqlError::syntax(102, 20, "Incorrect syntax near 'JSON_OBJECT'."),
        )),
        _ => Err(Error::Unsupported("repeated or mixed ON NULL clauses")),
    }
}

/// Build JSON_OBJECT from evaluated `key:value` pairs, in argument order.
pub fn object(pairs: &[(Scalar<'_>, Scalar<'_>)], nulls: NullClause) -> Result<String, Error> {
    let mut out = String::from("{");
    for (key, value) in pairs {
        let key = key_text(key)?;
        let Some(value) = value_json(value, Constructor::Object)? else {
            if nulls == NullClause::AbsentOnNull {
                continue;
            }
            push_member(&mut out, &key, "null");
            continue;
        };
        push_member(&mut out, &key, &value);
    }
    out.push('}');
    Ok(out)
}

/// Build JSON_ARRAY from evaluated elements, in argument order.
pub fn array(elements: &[Scalar<'_>], nulls: NullClause) -> Result<String, Error> {
    let mut out = String::from("[");
    for element in elements {
        let text = match value_json(element, Constructor::Array)? {
            Some(text) => text,
            None if nulls == NullClause::NullOnNull => "null".to_owned(),
            None => continue,
        };
        if out.len() > 1 {
            out.push(',');
        }
        out.push_str(&text);
    }
    out.push(']');
    Ok(out)
}

fn push_member(out: &mut String, key: &str, value: &str) {
    if out.len() > 1 {
        out.push(',');
    }
    out.push('"');
    out.push_str(&escape(key));
    out.push_str("\":");
    out.push_str(value);
}

fn escape(text: &str) -> std::borrow::Cow<'_, str> {
    json_escape::escape(text, "json").expect("constant JSON format")
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", escape(text))
}

fn padded(text: &str, length: u32, units: usize) -> Result<String, Error> {
    let length = usize::try_from(length).map_err(|_| Error::Unsupported("character length"))?;
    if units > length {
        return Err(Error::Unsupported(
            "fixed character value longer than its type",
        ));
    }
    Ok(format!("{text}{}", " ".repeat(length - units)))
}

fn char_text(text: &str, length: u32) -> Result<String, Error> {
    if !text.is_ascii() {
        return Err(Error::Unsupported("non-ASCII CHAR padding"));
    }
    padded(text, length, text.len())
}

fn nchar_text(text: &str, length: u32) -> Result<String, Error> {
    padded(text, length, text.encode_utf16().count())
}

fn temporal(result: anyhow::Result<String>) -> Result<String, Error> {
    result.map_err(|_| Error::Unsupported("date/time value outside the formatter range"))
}

fn guid(bytes: &[u8; 16]) -> String {
    let mut text = String::with_capacity(36);
    for (index, &at) in [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15]
        .iter()
        .enumerate()
    {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push_str(&format!("{:02X}", bytes[at]));
    }
    text
}

/// Scientific text with `digits` fractional mantissa digits and a signed
/// three-digit exponent, as SQL Server writes FLOAT and REAL into JSON.
fn scientific(text: String) -> String {
    let (mantissa, exponent) = text.split_once('e').expect("exponent form");
    let (sign, digits) = match exponent.strip_prefix('-') {
        Some(digits) => ('-', digits),
        None => ('+', exponent),
    };
    format!("{mantissa}e{sign}{digits:0>3}")
}

/// Unquoted text of a scalar used as a JSON_OBJECT key.
fn key_text(key: &Scalar<'_>) -> Result<String, Error> {
    Ok(match *key {
        Scalar::Null => return Err(Error::Runtime(SqlError::new(13638, 1, NULL_KEY))),
        Scalar::TinyInt(v) => v.to_string(),
        Scalar::SmallInt(v) => v.to_string(),
        Scalar::Int(v) => v.to_string(),
        Scalar::BigInt(v) => v.to_string(),
        Scalar::Decimal(v) => v.to_string(),
        Scalar::Date(v) => temporal(v.format_iso(0))?[..10].to_owned(),
        Scalar::Binary(v) => for_json::base64(v),
        Scalar::Char(v, length) => char_text(v, length)?,
        Scalar::NChar(v, length) => nchar_text(v, length)?,
        Scalar::Text(v) => v.to_owned(),
        _ => return Err(Error::Unsupported("JSON_OBJECT key type not captured")),
    })
}

/// JSON text of a constructor value or element; `None` for SQL NULL.
fn value_json(value: &Scalar<'_>, constructor: Constructor) -> Result<Option<String>, Error> {
    Ok(Some(match *value {
        Scalar::Null => return Ok(None),
        Scalar::TinyInt(v) => v.to_string(),
        Scalar::SmallInt(v) => v.to_string(),
        Scalar::Int(v) => v.to_string(),
        Scalar::BigInt(v) => v.to_string(),
        Scalar::Bit(v) => v.to_string(),
        Scalar::Decimal(v) => v.to_string(),
        Scalar::Money(v) => money::format(v, 2),
        Scalar::SmallMoney(v) => money::format(i64::from(v), 2),
        Scalar::Float(v) => scientific(format!("{v:.15e}")),
        Scalar::Real(v) => scientific(format!("{v:.7e}")),
        Scalar::Date(v) => quoted(&temporal(v.format_iso(0))?[..10]),
        Scalar::Time(v, scale) => quoted(&temporal(v.format_iso(scale))?[11..]),
        Scalar::DateTime(v) => quoted(&temporal(v.format_iso(3))?),
        Scalar::SmallDateTime(v) => quoted(&temporal(v.format_iso(0))?),
        Scalar::DateTime2(v, scale) => quoted(&temporal(v.format_iso(scale))?),
        Scalar::DateTimeOffset(v, scale) => {
            quoted(&temporal(v.format_iso(scale))?.replacen(' ', "", 1))
        }
        Scalar::UniqueIdentifier(v) => quoted(&guid(&v)),
        Scalar::Char(v, length) => quoted(&char_text(v, length)?),
        Scalar::NChar(v, length) => quoted(&nchar_text(v, length)?),
        Scalar::Text(v) | Scalar::Xml(v) => quoted(v),
        Scalar::Binary(v) => quoted(&for_json::base64(v)),
        Scalar::Json(v) => {
            if json::root(v.as_bytes()).is_none() {
                return Err(Error::Unsupported("JSON-kind value is not valid JSON"));
            }
            v.to_owned()
        }
        Scalar::Variant(Scalar::Int(v)) => v.to_string(),
        Scalar::Variant(_) => return Err(Error::Unsupported("SQL_VARIANT base type not captured")),
        Scalar::Clr => {
            return Err(Error::Runtime(match constructor {
                Constructor::Object => SqlError::new(13666, 2, OBJECT_CLR),
                Constructor::Array => SqlError::new(13666, 3, ARRAY_CLR),
            }));
        }
    }))
}

fn invalid_argument(name: &str, argument: u8) -> Error {
    Error::Compile(SqlError::new(
        8116,
        1,
        format!(
            "Argument data type {name} is invalid for argument {argument} of json_modify function."
        ),
    ))
}

/// Compile-time JSON_MODIFY checks: arity and declared argument types.
pub fn modify_signature(arguments: &[SqlType]) -> Result<ResultType, Error> {
    let [input, path, value] = arguments else {
        return Err(if arguments.len() == 2 {
            Error::Compile(SqlError::syntax(174, 1, MODIFY_ARITY))
        } else {
            Error::Unsupported("JSON_MODIFY arity not captured")
        });
    };
    use SqlType as T;
    match input {
        T::UntypedNull | T::VarChar | T::NVarChar | T::Json => {}
        T::Int => return Err(invalid_argument("int", 1)),
        _ => return Err(Error::Unsupported("JSON_MODIFY input type not captured")),
    }
    match path {
        T::VarChar | T::NVarChar => {}
        T::UntypedNull => return Err(invalid_argument("NULL", 2)),
        T::Int => return Err(invalid_argument("int", 2)),
        _ => return Err(Error::Unsupported("JSON_MODIFY path type not captured")),
    }
    let rejected = match value {
        T::UntypedNull
        | T::Int
        | T::BigInt
        | T::Bit
        | T::Decimal
        | T::Float
        | T::VarChar
        | T::NVarChar
        | T::Json => None,
        T::Money => Some("money"),
        T::Date => Some("date"),
        T::DateTime2 => Some("datetime2"),
        T::UniqueIdentifier => Some("uniqueidentifier"),
        T::VarBinary => Some("varbinary"),
        T::Xml => Some("xml"),
        _ => return Err(Error::Unsupported("JSON_MODIFY value type not captured")),
    };
    if let Some(name) = rejected {
        return Err(invalid_argument(name, 3));
    }
    Ok(if *input == T::Json {
        ResultType::Json
    } else {
        ResultType::NVarCharMax
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

#[derive(Debug)]
struct Path {
    strict: bool,
    append: bool,
    steps: Vec<Step>,
}

fn path_error(state: u8, character: char, position: usize) -> Error {
    Error::Runtime(SqlError::new(
        13607,
        state,
        format!(
            "{} Unexpected character '{character}' is found at position {position}.",
            json_path::PATH
        ),
    ))
}

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || (!c.is_ascii() && c.is_alphanumeric())
}

fn parse_path(text: &str) -> Result<Path, Error> {
    if text.is_empty() {
        return Err(path_error(14, '.', 0));
    }
    let mut rest = text;
    let mut append = false;
    let mut strict = false;
    let mut moded = false;
    if let Some(tail) = rest.strip_prefix("append ") {
        (append, moded, rest) = (true, true, tail);
    }
    if let Some(tail) = rest.strip_prefix("lax ") {
        (moded, rest) = (true, tail);
    } else if let Some(tail) = rest.strip_prefix("strict ") {
        (strict, moded, rest) = (true, true, tail);
    }
    let Some(mut rest) = rest.strip_prefix('$') else {
        let word = rest.split(' ').next().unwrap_or_default();
        if ["append", "lax", "strict"]
            .iter()
            .any(|mode| word.eq_ignore_ascii_case(mode))
        {
            return Err(Error::Unsupported("JSON path mode spelling not captured"));
        }
        return match (moded, text.chars().next()) {
            (false, Some(c)) if c != ' ' => Err(path_error(22, c, 0)),
            _ => Err(Error::Unsupported(
                "JSON path error after a mode not captured",
            )),
        };
    };
    let unsupported = |what| {
        if moded || !text.is_ascii() {
            Error::Unsupported("JSON path error position after a mode or non-ASCII text")
        } else {
            what
        }
    };
    let offset = |rest: &str| text.len() - rest.len();
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('.') {
            if tail.is_empty() {
                return Err(unsupported(path_error(14, '.', text.len())));
            }
            if tail.starts_with('*') {
                return Err(unsupported(Error::Runtime(SqlError::new(
                    13660,
                    4,
                    WILDCARD_PATH,
                ))));
            }
            if tail.starts_with('"') {
                let (kind, end) = json::prefix(tail.as_bytes())
                    .ok_or(Error::Unsupported("malformed quoted JSON path key"))?;
                if kind != json::Kind::String {
                    return Err(Error::Unsupported("malformed quoted JSON path key"));
                }
                let key = json_path::decode(&tail[..end])
                    .map_err(|_| Error::Unsupported("JSON path key with isolated surrogate"))?;
                steps.push(Step::Key(key));
                rest = &tail[end..];
            } else {
                let end = tail.find(|c| !key_char(c)).unwrap_or(tail.len());
                if end == 0 {
                    return Err(Error::Unsupported("JSON path key character not captured"));
                }
                steps.push(Step::Key(tail[..end].to_owned()));
                rest = &tail[end..];
            }
        } else if let Some(tail) = rest.strip_prefix('[') {
            let end = tail
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(tail.len());
            if end == 0 {
                return match tail.chars().next() {
                    Some(c) if c != '*' && c != ']' => {
                        Err(unsupported(path_error(21, c, offset(tail))))
                    }
                    _ => Err(Error::Unsupported("JSON path index form not captured")),
                };
            }
            if !tail[end..].starts_with(']') || (end > 1 && tail.starts_with('0')) {
                return Err(Error::Unsupported("JSON path index form not captured"));
            }
            let index = tail[..end]
                .parse::<i32>()
                .map_err(|_| Error::Unsupported("JSON path index beyond INT"))?;
            steps.push(Step::Index(index as usize));
            rest = &tail[end + 1..];
        } else {
            return Err(Error::Unsupported("JSON path character not captured"));
        }
    }
    if steps.is_empty() {
        return Err(if moded {
            Error::Unsupported("root JSON_MODIFY path with a mode not captured")
        } else {
            Error::Runtime(SqlError::new(13619, 1, ROOT_PATH))
        });
    }
    Ok(Path {
        strict,
        append,
        steps,
    })
}

fn document_error(character: char, position: usize) -> Error {
    Error::Runtime(SqlError::new(
        13609,
        7,
        format!(
            "{} Unexpected character '{character}' is found at position {position}.",
            json_path::DOCUMENT
        ),
    ))
}

/// Validate a JSON_MODIFY document, whose root must be an object or array.
/// Captured positions are the root token and the end of truncated input
/// (reported as '.'); other failure positions are unsupported.
fn check_document(text: &str) -> Result<(), Error> {
    let bytes = text.as_bytes();
    let root = skip_ws(bytes, 0);
    let Some(&first) = bytes.get(root) else {
        return if text.is_empty() {
            Err(document_error('.', 0))
        } else {
            Err(Error::Unsupported("whitespace-only document not captured"))
        };
    };
    if root != 0 {
        return match json::root(bytes) {
            Some(json::Kind::Object | json::Kind::Array) => Ok(()),
            _ => Err(Error::Unsupported(
                "invalid document after leading whitespace",
            )),
        };
    }
    if !matches!(first, b'{' | b'[') {
        return Err(document_error(text.chars().next().expect("non-empty"), 0));
    }
    match scan(bytes) {
        Ok(()) => Ok(()),
        Err(at) if at == bytes.len() && text.is_ascii() => Err(document_error('.', at)),
        Err(_) => Err(Error::Unsupported(
            "invalid JSON document position not captured",
        )),
    }
}

/// Iterative JSON scan returning the byte offset where input stops being a
/// valid JSON prefix (the length when input ends early).
fn scan(bytes: &[u8]) -> Result<(), usize> {
    #[derive(Clone, Copy)]
    enum State {
        Value,
        ArrayFirst,
        ArrayComma,
        ObjectFirst,
        ObjectKey,
        ObjectColon,
        ObjectComma,
    }
    let byte = |at: usize| bytes.get(at).copied().ok_or(at);
    let digits = |mut at: usize| -> Result<usize, usize> {
        if !byte(at)?.is_ascii_digit() {
            return Err(at);
        }
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        Ok(at)
    };
    let string = |mut at: usize| -> Result<usize, usize> {
        if byte(at)? != b'"' {
            return Err(at);
        }
        at += 1;
        loop {
            match byte(at)? {
                b'"' => return Ok(at + 1),
                0..=31 => return Err(at),
                b'\\' => {
                    at += 1;
                    match byte(at)? {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => at += 1,
                        b'u' => {
                            for _ in 0..4 {
                                at += 1;
                                if !byte(at)?.is_ascii_hexdigit() {
                                    return Err(at);
                                }
                            }
                            at += 1;
                        }
                        _ => return Err(at),
                    }
                }
                _ => at += 1,
            }
        }
    };
    let mut at = 0;
    let mut stack = vec![State::Value];
    while let Some(state) = stack.pop() {
        at = skip_ws(bytes, at);
        let c = byte(at)?;
        match state {
            State::Value => match c {
                b'{' => {
                    at += 1;
                    stack.push(State::ObjectFirst);
                }
                b'[' => {
                    at += 1;
                    stack.push(State::ArrayFirst);
                }
                b'"' => at = string(at)?,
                b't' | b'f' | b'n' => {
                    let literal: &[u8] = match c {
                        b't' => b"true",
                        b'f' => b"false",
                        _ => b"null",
                    };
                    for &expected in literal {
                        if byte(at)? != expected {
                            return Err(at);
                        }
                        at += 1;
                    }
                }
                b'-' | b'0'..=b'9' => {
                    if c == b'-' {
                        at += 1;
                    }
                    at = if byte(at)? == b'0' {
                        at + 1
                    } else {
                        digits(at)?
                    };
                    if bytes.get(at) == Some(&b'.') {
                        at = digits(at + 1)?;
                    }
                    if matches!(bytes.get(at), Some(b'e' | b'E')) {
                        at += 1;
                        if matches!(byte(at)?, b'+' | b'-') {
                            at += 1;
                        }
                        at = digits(at)?;
                    }
                }
                _ => return Err(at),
            },
            State::ArrayFirst if c == b']' => at += 1,
            State::ArrayFirst => stack.extend([State::ArrayComma, State::Value]),
            State::ArrayComma if c == b',' => {
                at += 1;
                stack.extend([State::ArrayComma, State::Value]);
            }
            State::ArrayComma if c == b']' => at += 1,
            State::ObjectFirst if c == b'}' => at += 1,
            State::ObjectFirst | State::ObjectKey => {
                at = string(at)?;
                stack.push(State::ObjectColon);
            }
            State::ObjectColon if c == b':' => {
                at += 1;
                stack.extend([State::ObjectComma, State::Value]);
            }
            State::ObjectComma if c == b',' => {
                at += 1;
                stack.push(State::ObjectKey);
            }
            State::ObjectComma if c == b'}' => at += 1,
            State::ArrayComma | State::ObjectColon | State::ObjectComma => return Err(at),
        }
    }
    if skip_ws(bytes, at) == bytes.len() {
        Ok(())
    } else {
        Err(skip_ws(bytes, at))
    }
}

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    while bytes
        .get(at)
        .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
    {
        at += 1;
    }
    at
}

/// End of the value starting at `at` in a validated document.
fn value_end(bytes: &[u8], at: usize) -> usize {
    json::prefix(&bytes[at..]).expect("validated document").1 + at
}

/// A member of an object or an element of an array, by byte span.
struct Entry {
    /// Start of the member key, or of the element value.
    start: usize,
    value_start: usize,
    value_end: usize,
    key: Option<String>,
}

/// Entries of the container starting at `at` in a validated document.
fn entries(bytes: &[u8], at: usize) -> Result<Vec<Entry>, Error> {
    let object = bytes[at] == b'{';
    let mut result = Vec::new();
    let mut at = skip_ws(bytes, at + 1);
    if matches!(bytes[at], b'}' | b']') {
        return Ok(result);
    }
    loop {
        let start = at;
        let key = if object {
            let end = value_end(bytes, at);
            let text = std::str::from_utf8(&bytes[at..end]).expect("UTF-8 document");
            let key = json_path::decode(text)
                .map_err(|_| Error::Unsupported("document key with isolated surrogate"))?;
            at = skip_ws(bytes, skip_ws(bytes, end) + 1);
            Some(key)
        } else {
            None
        };
        let value_start = at;
        let end = value_end(bytes, at);
        result.push(Entry {
            start,
            value_start,
            value_end: end,
            key,
        });
        at = skip_ws(bytes, end);
        if bytes[at] == b',' {
            at = skip_ws(bytes, at + 1);
        } else {
            return Ok(result);
        }
    }
}

fn find(entries: &[Entry], step: &Step) -> Option<usize> {
    match step {
        Step::Key(key) => entries.iter().position(|e| e.key.as_ref() == Some(key)),
        Step::Index(index) => (*index < entries.len()).then_some(*index),
    }
}

fn missing() -> Error {
    Error::Runtime(SqlError::new(13608, 2, json_path::MISSING))
}

fn splice(text: &str, start: usize, end: usize, insert: &str) -> String {
    format!("{}{insert}{}", &text[..start], &text[end..])
}

/// JSON text of a JSON_MODIFY value; `None` for SQL NULL.
fn modify_value(value: &Scalar<'_>) -> Result<Option<String>, Error> {
    match value {
        Scalar::Null
        | Scalar::Int(_)
        | Scalar::BigInt(_)
        | Scalar::Bit(_)
        | Scalar::Decimal(_)
        | Scalar::Float(_)
        | Scalar::Text(_)
        | Scalar::Json(_) => value_json(value, Constructor::Object),
        _ => Err(Error::Unsupported(
            "JSON_MODIFY value kind not captured or rejected at compile time",
        )),
    }
}

/// Apply JSON_MODIFY to an evaluated input, path and value.
/// A NULL input returns NULL; a NULL path is the runtime 8116 state 8 error.
pub fn modify(
    input: Option<&str>,
    path: Option<&str>,
    value: &Scalar<'_>,
) -> Result<Option<String>, Error> {
    let Some(text) = input else {
        return Ok(None);
    };
    let Some(path) = path else {
        return Err(Error::Runtime(SqlError::new(8116, 8, NULL_MODIFY_PATH)));
    };
    let value = modify_value(value)?;
    match (parse_path(path), check_document(text)) {
        (Err(Error::Unsupported(what)), _) | (_, Err(Error::Unsupported(what))) => {
            Err(Error::Unsupported(what))
        }
        (Err(_), Err(_)) => Err(Error::Unsupported(
            "precedence of invalid path and invalid document not captured",
        )),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Ok(path), Ok(())) => apply(text, &path, value.as_deref()).map(Some),
    }
}

fn apply(text: &str, path: &Path, value: Option<&str>) -> Result<String, Error> {
    let bytes = text.as_bytes();
    let lax_or_missing = |unchanged: String| {
        if path.strict {
            Err(missing())
        } else {
            Ok(unchanged)
        }
    };
    let (last, parents) = path.steps.split_last().expect("non-root path");
    let mut container = skip_ws(bytes, 0);
    for step in parents {
        let found = match (bytes[container], step) {
            (b'{', Step::Key(_)) | (b'[', Step::Index(_)) => {
                let list = entries(bytes, container)?;
                find(&list, step).map(|i| list[i].value_start)
            }
            (b'{' | b'[', _) => {
                return Err(Error::Unsupported("key/index on mismatched container"));
            }
            _ => None,
        };
        match found {
            Some(at) if matches!(bytes[at], b'{' | b'[') => container = at,
            Some(_) if path.append => {
                return Err(Error::Unsupported("append through a scalar not captured"));
            }
            Some(_) | None if path.append && !path.strict => {
                return Err(Error::Unsupported(
                    "append with a missing parent not captured",
                ));
            }
            _ => return lax_or_missing(text.to_owned()),
        }
    }
    let kind_matches = match (bytes[container], last) {
        (b'{', Step::Key(_)) | (b'[', Step::Index(_)) => true,
        (b'{' | b'[', _) => return Err(Error::Unsupported("key/index on mismatched container")),
        _ => false,
    };
    if !kind_matches {
        if path.append {
            return Err(Error::Unsupported("append through a scalar not captured"));
        }
        return lax_or_missing(text.to_owned());
    }
    let list = entries(bytes, container)?;
    let found = find(&list, last);
    if path.append {
        let Some(i) = found else {
            if path.strict {
                return Err(missing());
            }
            let (Step::Key(key), Some(value)) = (last, value) else {
                return Err(Error::Unsupported("append creating an array not captured"));
            };
            return Ok(insert_member(
                text,
                &list,
                container,
                key,
                &format!("[{value}]"),
            ));
        };
        let target = list[i].value_start;
        if bytes[target] != b'[' {
            return if path.strict {
                Err(Error::Runtime(SqlError::new(13621, 1, NO_ARRAY)))
            } else {
                Ok(text.to_owned())
            };
        }
        let items = entries(bytes, target)?;
        let value = value.unwrap_or("null");
        return Ok(match items.last() {
            Some(item) => splice(text, item.value_end, item.value_end, &format!(",{value}")),
            None => splice(text, target + 1, target + 1, value),
        });
    }
    match (found, last, value) {
        (Some(i), _, Some(value)) => {
            Ok(splice(text, list[i].value_start, list[i].value_end, value))
        }
        (Some(i), Step::Key(_), None) if path.strict => {
            Ok(splice(text, list[i].value_start, list[i].value_end, "null"))
        }
        (Some(i), Step::Key(_), None) => Ok(delete_member(text, &list, i)),
        (Some(_), Step::Index(_), None) if path.strict => {
            Err(Error::Unsupported("strict NULL array element not captured"))
        }
        (Some(i), Step::Index(_), None) => {
            Ok(splice(text, list[i].value_start, list[i].value_end, "null"))
        }
        (None, _, _) if path.strict => Err(missing()),
        (None, Step::Key(key), Some(value)) => {
            Ok(insert_member(text, &list, container, key, value))
        }
        (None, Step::Key(_), None) => Ok(text.to_owned()),
        (None, Step::Index(_), Some(_)) => Ok(text.to_owned()),
        (None, Step::Index(_), None) => Err(Error::Unsupported(
            "lax NULL out-of-range array element not captured",
        )),
    }
}

fn insert_member(text: &str, list: &[Entry], container: usize, key: &str, value: &str) -> String {
    let member = format!("\"{}\":{value}", escape(key));
    match list.last() {
        Some(entry) => splice(
            text,
            entry.value_end,
            entry.value_end,
            &format!(",{member}"),
        ),
        None => splice(text, container + 1, container + 1, &member),
    }
}

fn delete_member(text: &str, list: &[Entry], index: usize) -> String {
    if let Some(next) = list.get(index + 1) {
        splice(text, list[index].start, next.start, "")
    } else if let Some(previous) = index.checked_sub(1).map(|i| &list[i]) {
        splice(text, previous.value_end, list[index].value_end, "")
    } else {
        splice(text, list[index].start, list[index].value_end, "")
    }
}
