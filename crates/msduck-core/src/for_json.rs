//! Deterministic FOR JSON PATH planning and rendering.
//! Path-tree layout is adapted from mssqlite's transpile/src/for-json.ts;
//! SQL binding, value conversion, row ordering and wire framing remain adapters.
use crate::{json, json_escape};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnnamedColumn,
    InvalidAlias(String),
    ConflictingAlias(String),
    RootWithoutArrayWrapper,
    InvalidNumber,
    InvalidJson,
    OutputLimit,
    RowWidth { expected: usize, actual: usize },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnnamedColumn => f.write_str("FOR JSON requires named columns"),
            Self::InvalidAlias(alias) => write!(f, "invalid FOR JSON PATH alias: {alias}"),
            Self::ConflictingAlias(alias) => write!(f, "conflicting FOR JSON PATH alias: {alias}"),
            Self::RootWithoutArrayWrapper => {
                f.write_str("FOR JSON ROOT cannot be combined with WITHOUT_ARRAY_WRAPPER")
            }
            Self::InvalidNumber => f.write_str("invalid JSON number"),
            Self::InvalidJson => f.write_str("invalid JSON fragment"),
            Self::OutputLimit => f.write_str("FOR JSON result exceeds the configured output limit"),
            Self::RowWidth { expected, actual } => {
                write!(f, "FOR JSON row has {actual} values, expected {expected}")
            }
        }
    }
}
impl std::error::Error for Error {}

/// Validated lexical JSON number, without conversion through floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Number<'a>(&'a str);
impl<'a> Number<'a> {
    pub fn new(text: &'a str) -> Result<Self, Error> {
        if json::root(text.as_bytes()) == Some(json::Kind::Number) {
            Ok(Self(crate::json_path::trim(text)))
        } else {
            Err(Error::InvalidNumber)
        }
    }
}
/// Validated JSON text. Its internal spelling, whitespace and duplicates survive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fragment<'a>(&'a str);
impl<'a> Fragment<'a> {
    pub fn new(text: &'a str) -> Result<Self, Error> {
        json::root(text.as_bytes()).ok_or(Error::InvalidJson)?;
        Ok(Self(text))
    }
}
/// SQL NULL is distinct from a non-NULL value containing the JSON literal null.
/// Adapters must explicitly choose text versus promoted JSON fragments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Null,
    Text(&'a str),
    Boolean(bool),
    Number(Number<'a>),
    Json(Fragment<'a>),
}

/// Validated JSON code units. Validation never changes surrogate units or spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utf16Fragment<'a>(&'a [u16]);
impl<'a> Utf16Fragment<'a> {
    pub fn new(units: &'a [u16]) -> Result<Self, Error> {
        if json::valid_utf16(units, 1) {
            Ok(Self(units))
        } else {
            Err(Error::InvalidJson)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Utf16Value<'a> {
    Null,
    Text(&'a [u16]),
    Boolean(bool),
    Number(Number<'a>),
    Json(Utf16Fragment<'a>),
}

enum Part<'a> {
    Punctuation(u8),
    Name(&'a str),
    Value(usize),
}

#[derive(Clone, Debug)]
enum Target {
    Column(usize),
    Object(usize),
}
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    target: Target,
}
#[derive(Clone, Debug, Default)]
struct Node {
    entries: Vec<Entry>,
    // Lookup only: output follows entries, never map iteration order.
    names: BTreeMap<String, usize>,
}
#[derive(Clone, Debug)]
pub struct PathPlan {
    nodes: Vec<Node>,
    columns: usize,
}
impl PathPlan {
    /// Compile ordered aliases. Nested properties must remain contiguous;
    /// reopening an object after another property is a path conflict.
    pub fn new<I, S>(aliases: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut plan = Self {
            nodes: vec![Node::default()],
            columns: 0,
        };
        for alias in aliases {
            let alias = alias.as_ref();
            if alias.is_empty() {
                return Err(Error::UnnamedColumn);
            }
            let parts = alias.split('.').collect::<Vec<_>>();
            if parts.iter().any(|part| part.is_empty()) {
                return Err(Error::InvalidAlias(alias.into()));
            }
            let mut node = 0;
            for (at, name) in parts.iter().enumerate() {
                let last = at + 1 == parts.len();
                if let Some(&entry) = plan.nodes[node].names.get(*name) {
                    let Target::Object(child) = plan.nodes[node].entries[entry].target else {
                        return Err(Error::ConflictingAlias(alias.into()));
                    };
                    if last || entry + 1 != plan.nodes[node].entries.len() {
                        return Err(Error::ConflictingAlias(alias.into()));
                    }
                    node = child;
                } else {
                    let child = plan.nodes.len();
                    let target = if last {
                        Target::Column(plan.columns)
                    } else {
                        Target::Object(child)
                    };
                    let entry = plan.nodes[node].entries.len();
                    plan.nodes[node].names.insert((*name).into(), entry);
                    plan.nodes[node].entries.push(Entry {
                        name: (*name).into(),
                        target,
                    });
                    if !last {
                        plan.nodes.push(Node::default());
                        node = child;
                    }
                }
            }
            plan.columns += 1;
        }
        if plan.columns == 0 {
            return Err(Error::UnnamedColumn);
        }
        Ok(plan)
    }
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Render one row. Empty nested objects caused solely by omitted SQL NULLs
    /// disappear; an explicitly supplied empty JSON object remains present.
    pub fn row(&self, values: &[Value<'_>], include_null_values: bool) -> Result<String, Error> {
        let mut output = String::new();
        self.render(
            values.len(),
            |column| include_null_values || !matches!(values[column], Value::Null),
            |part| {
                match part {
                    Part::Punctuation(byte) => output.push(char::from(byte)),
                    Part::Name(name) => quoted(&mut output, name),
                    Part::Value(column) => match values[column] {
                        Value::Null => output.push_str("null"),
                        Value::Text(text) => quoted(&mut output, text),
                        Value::Boolean(value) => {
                            output.push_str(if value { "true" } else { "false" })
                        }
                        Value::Number(number) => output.push_str(number.0),
                        Value::Json(fragment) => output.push_str(fragment.0),
                    },
                }
                Ok(())
            },
        )?;
        Ok(output)
    }

    /// Render exact UTF-16 units with an explicit maximum output-unit count.
    pub fn row_utf16(
        &self,
        values: &[Utf16Value<'_>],
        include_null_values: bool,
        max_units: usize,
    ) -> Result<Vec<u16>, Error> {
        let mut output = Utf16Buffer::new(max_units);
        self.render(
            values.len(),
            |column| include_null_values || !matches!(values[column], Utf16Value::Null),
            |part| match part {
                Part::Punctuation(byte) => output.ascii(byte),
                Part::Name(name) => output.quoted(&name.encode_utf16().collect::<Vec<_>>()),
                Part::Value(column) => match values[column] {
                    Utf16Value::Null => output.text("null"),
                    Utf16Value::Text(text) => output.quoted(text),
                    Utf16Value::Boolean(value) => output.text(if value { "true" } else { "false" }),
                    Utf16Value::Number(number) => output.text(number.0),
                    Utf16Value::Json(fragment) => output.extend(fragment.0),
                },
            },
        )?;
        Ok(output.units)
    }

    fn render(
        &self,
        width: usize,
        present: impl Fn(usize) -> bool,
        mut emit: impl FnMut(Part<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if width != self.columns {
            return Err(Error::RowWidth {
                expected: self.columns,
                actual: width,
            });
        }
        let mut visible = vec![false; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate().rev() {
            visible[index] = node.entries.iter().any(|entry| match entry.target {
                Target::Column(column) => present(column),
                Target::Object(child) => visible[child],
            });
        }
        enum Task<'a> {
            Object(usize),
            Member(&'a Entry),
            Comma,
            Close,
        }
        let mut pending = vec![Task::Object(0)];
        while let Some(task) = pending.pop() {
            match task {
                Task::Object(index) => {
                    emit(Part::Punctuation(b'{'))?;
                    pending.push(Task::Close);
                    let mut later = false;
                    for entry in self.nodes[index].entries.iter().rev() {
                        let present = match entry.target {
                            Target::Column(column) => present(column),
                            Target::Object(child) => visible[child],
                        };
                        if present {
                            if later {
                                pending.push(Task::Comma);
                            }
                            pending.push(Task::Member(entry));
                            later = true;
                        }
                    }
                }
                Task::Member(entry) => {
                    emit(Part::Name(&entry.name))?;
                    emit(Part::Punctuation(b':'))?;
                    match entry.target {
                        Target::Object(child) => pending.push(Task::Object(child)),
                        Target::Column(column) => emit(Part::Value(column))?,
                    }
                }
                Task::Comma => emit(Part::Punctuation(b','))?,
                Task::Close => emit(Part::Punctuation(b'}'))?,
            }
        }
        Ok(())
    }
}
fn quoted(output: &mut String, text: &str) {
    output.push('"');
    output.push_str(&json_escape::escape(text, "json").expect("JSON format is supported"));
    output.push('"');
}

struct Utf16Buffer {
    units: Vec<u16>,
    limit: usize,
}
impl Utf16Buffer {
    fn new(limit: usize) -> Self {
        Self {
            units: Vec::new(),
            limit,
        }
    }
    fn remaining(&self) -> usize {
        self.limit - self.units.len()
    }
    fn check(&self, count: usize) -> Result<(), Error> {
        if count > self.remaining() {
            Err(Error::OutputLimit)
        } else {
            Ok(())
        }
    }
    fn ascii(&mut self, byte: u8) -> Result<(), Error> {
        self.check(1)?;
        self.units.push(u16::from(byte));
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<(), Error> {
        self.check(text.encode_utf16().count())?;
        self.units.extend(text.encode_utf16());
        Ok(())
    }
    fn extend(&mut self, units: &[u16]) -> Result<(), Error> {
        self.check(units.len())?;
        self.units.extend_from_slice(units);
        Ok(())
    }
    fn quoted(&mut self, units: &[u16]) -> Result<(), Error> {
        let count = units.iter().try_fold(2_usize, |size, &unit| {
            size.checked_add(match unit {
                8 | 9 | 10 | 12 | 13 | 34 | 47 | 92 => 2,
                0..=31 => 6,
                _ => 1,
            })
            .ok_or(Error::OutputLimit)
        })?;
        // Check the expanded size before escape_utf16 allocates a temporary.
        self.check(count)?;
        let escaped = json_escape::escape_utf16(units, &[106, 115, 111, 110])
            .expect("JSON format is supported");
        self.units.push(u16::from(b'"'));
        self.units.extend_from_slice(&escaped);
        self.units.push(u16::from(b'"'));
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub include_null_values: bool,
    pub without_array_wrapper: bool,
    pub root: Option<String>,
}
/// Ordered result writer with caller-owned state. No database iteration or I/O.
/// WITHOUT_ARRAY_WRAPPER deliberately permits comma-separated objects for
/// multiple rows; the result is not necessarily a valid standalone JSON value.
pub struct Writer<'a> {
    plan: &'a PathPlan,
    options: Options,
    output: String,
    has_rows: bool,
}
impl<'a> Writer<'a> {
    pub fn new(plan: &'a PathPlan, options: Options) -> Result<Self, Error> {
        if options.root.is_some() && options.without_array_wrapper {
            return Err(Error::RootWithoutArrayWrapper);
        }
        let mut output = String::new();
        if let Some(root) = &options.root {
            output.push('{');
            quoted(&mut output, root);
            output.push(':');
        }
        if !options.without_array_wrapper {
            output.push('[');
        }
        Ok(Self {
            plan,
            options,
            output,
            has_rows: false,
        })
    }
    /// A row-width error leaves accumulated output unchanged.
    pub fn push(&mut self, row: &[Value<'_>]) -> Result<(), Error> {
        let row = self.plan.row(row, self.options.include_null_values)?;
        if self.has_rows {
            self.output.push(',');
        }
        self.output.push_str(&row);
        self.has_rows = true;
        Ok(())
    }
    /// UTF-8 bytes currently buffered, for adapter-owned resource limits.
    pub fn buffered_bytes(&self) -> usize {
        self.output.len()
    }
    pub fn finish(mut self) -> String {
        if !self.options.without_array_wrapper {
            self.output.push(']');
        }
        if self.options.root.is_some() {
            self.output.push('}');
        }
        self.output
    }
}

/// Caller-owned, bounded UTF-16 result writer. No lossy string conversion.
pub struct Utf16Writer<'a> {
    plan: &'a PathPlan,
    options: Options,
    output: Utf16Buffer,
    has_rows: bool,
}
impl<'a> Utf16Writer<'a> {
    pub fn new(plan: &'a PathPlan, options: Options, max_units: usize) -> Result<Self, Error> {
        if options.root.is_some() && options.without_array_wrapper {
            return Err(Error::RootWithoutArrayWrapper);
        }
        let closing =
            usize::from(!options.without_array_wrapper) + usize::from(options.root.is_some());
        let mut output =
            Utf16Buffer::new(max_units.checked_sub(closing).ok_or(Error::OutputLimit)?);
        if let Some(root) = &options.root {
            output.ascii(b'{')?;
            output.quoted(&root.encode_utf16().collect::<Vec<_>>())?;
            output.ascii(b':')?;
        }
        if !options.without_array_wrapper {
            output.ascii(b'[')?;
        }
        Ok(Self {
            plan,
            options,
            output,
            has_rows: false,
        })
    }
    /// A malformed row or exceeded limit leaves accumulated output unchanged.
    pub fn push(&mut self, row: &[Utf16Value<'_>]) -> Result<(), Error> {
        let available = self
            .output
            .remaining()
            .checked_sub(usize::from(self.has_rows))
            .ok_or(Error::OutputLimit)?;
        let row = self
            .plan
            .row_utf16(row, self.options.include_null_values, available)?;
        // row_utf16 checked the complete size before any accumulated state changes.
        if self.has_rows {
            self.output.units.push(u16::from(b','));
        }
        self.output.units.extend(row);
        self.has_rows = true;
        Ok(())
    }
    pub fn buffered_units(&self) -> usize {
        self.output.units.len()
    }
    pub fn finish(mut self) -> Vec<u16> {
        // Closing punctuation was reserved by new, before any rows were accepted.
        if !self.options.without_array_wrapper {
            self.output.units.push(u16::from(b']'));
        }
        if self.options.root.is_some() {
            self.output.units.push(u16::from(b'}'));
        }
        self.output.units
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_order_null_omission_and_fragment_promotion() {
        let plan = PathPlan::new([
            "id",
            "info.name",
            "info.optional",
            "empty.child",
            "json",
            "text",
            "flag",
        ])
        .unwrap();
        let row = [
            Value::Number(Number::new("12345678901234567890.1200").unwrap()),
            Value::Text("雪/🦆\n"),
            Value::Null,
            Value::Null,
            Value::Json(Fragment::new(r#"{ "a":1,"a":2 }"#).unwrap()),
            Value::Text("{}"),
            Value::Boolean(true),
        ];
        assert_eq!(
            plan.row(&row, false).unwrap(),
            "{\"id\":12345678901234567890.1200,\"info\":{\"name\":\"雪\\/🦆\\n\"},\"json\":{ \"a\":1,\"a\":2 },\"text\":\"{}\",\"flag\":true}"
        );
        let included = plan.row(&row, true).unwrap();
        assert!(
            included
                .contains(r#""info":{"name":"雪\/🦆\n","optional":null},"empty":{"child":null}"#)
        );
        let plan = PathPlan::new(["sql_null", "json_null", "object"]).unwrap();
        assert_eq!(
            plan.row(
                &[
                    Value::Null,
                    Value::Json(Fragment::new("null").unwrap()),
                    Value::Json(Fragment::new("{}").unwrap())
                ],
                false
            )
            .unwrap(),
            r#"{"json_null":null,"object":{}}"#
        );
        assert_eq!(plan.row(&[Value::Null; 3], false).unwrap(), "{}");
    }
    #[test]
    fn aliases_validate_before_rows_including_reopened_objects() {
        for aliases in [
            vec!["a", "a"],
            vec!["a", "a.b"],
            vec!["a.b", "a"],
            vec!["a.b", "c", "a.d"],
            vec!["a.b.x", "a.c", "a.b.y"],
        ] {
            assert!(matches!(
                PathPlan::new(aliases),
                Err(Error::ConflictingAlias(_))
            ));
        }
        for alias in ["a..b", ".a", "a."] {
            assert_eq!(
                PathPlan::new([alias]).unwrap_err(),
                Error::InvalidAlias(alias.into())
            );
        }
        assert_eq!(PathPlan::new([""]).unwrap_err(), Error::UnnamedColumn);
        assert_eq!(
            PathPlan::new(Vec::<String>::new()).unwrap_err(),
            Error::UnnamedColumn
        );
        let plan = PathPlan::new(["A", "a", "x\"/\\"]).unwrap();
        assert_eq!(
            plan.row(&[Value::Null; 3], true).unwrap(),
            r#"{"A":null,"a":null,"x\"\/\\":null}"#
        );
    }
    #[test]
    fn wrappers_empty_results_and_failed_rows() {
        let plan = PathPlan::new(["a"]).unwrap();
        assert_eq!(
            Writer::new(&plan, Options::default()).unwrap().finish(),
            "[]"
        );
        let opts = Options {
            root: Some("root/雪".into()),
            ..Default::default()
        };
        assert_eq!(
            Writer::new(&plan, opts.clone()).unwrap().finish(),
            r#"{"root\/雪":[]}"#
        );
        let mut writer = Writer::new(&plan, opts).unwrap();
        writer.push(&[Value::Boolean(false)]).unwrap();
        assert_eq!(
            writer.push(&[]),
            Err(Error::RowWidth {
                expected: 1,
                actual: 0
            })
        );
        writer.push(&[Value::Null]).unwrap();
        assert_eq!(writer.finish(), r#"{"root\/雪":[{"a":false},{}]}"#);
        let opts = Options {
            without_array_wrapper: true,
            ..Default::default()
        };
        assert_eq!(Writer::new(&plan, opts.clone()).unwrap().finish(), "");
        let mut writer = Writer::new(&plan, opts).unwrap();
        writer.push(&[Value::Text("one")]).unwrap();
        writer.push(&[Value::Text("two")]).unwrap();
        assert_eq!(writer.finish(), r#"{"a":"one"},{"a":"two"}"#);
        assert!(matches!(
            Writer::new(
                &plan,
                Options {
                    root: Some("r".into()),
                    without_array_wrapper: true,
                    ..Default::default()
                }
            ),
            Err(Error::RootWithoutArrayWrapper)
        ));
    }
    #[test]
    fn values_are_validated_without_numeric_rounding() {
        for invalid in ["NaN", "Infinity", "01", "+1", "1,2", "null", "\"1\""] {
            assert_eq!(Number::new(invalid), Err(Error::InvalidNumber));
        }
        for invalid in ["", "[1,]", "{}[]", "{bad}"] {
            assert_eq!(Fragment::new(invalid), Err(Error::InvalidJson));
        }
        let plan = PathPlan::new(["n"]).unwrap();
        assert_eq!(
            plan.row(
                &[Value::Number(Number::new(" 1.20e+99999 ").unwrap())],
                false
            )
            .unwrap(),
            r#"{"n":1.20e+99999}"#
        );
    }
    #[test]
    fn deep_paths_and_many_rows_do_not_use_recursive_rendering() {
        let alias = vec!["a"; 2000].join(".");
        let plan = PathPlan::new([alias]).unwrap();
        let output = plan.row(&[Value::Boolean(true)], false).unwrap();
        assert_eq!(json::root(output.as_bytes()), Some(json::Kind::Object));
        assert_eq!(plan.row(&[Value::Null], false).unwrap(), "{}");
        let plan = PathPlan::new(["n"]).unwrap();
        let mut writer = Writer::new(&plan, Options::default()).unwrap();
        for _ in 0..6000 {
            writer.push(&[Value::Boolean(true)]).unwrap();
        }
        let output = writer.finish();
        assert_eq!(output.matches("{\"n\":true}").count(), 6000);
        assert_eq!(json::root(output.as_bytes()), Some(json::Kind::Array));
    }
}

/// Standard padded Base64 for SQL binary JSON strings.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[usize::from(a >> 2)] as char);
        out.push(ALPHABET[usize::from((a & 3) << 4 | b >> 4)] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[usize::from((b & 15) << 2 | c >> 6)] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[usize::from(c & 63)] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod binary_tests {
    #[test]
    fn standard_padded_base64_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64(input.as_bytes()), expected);
        }
        assert_eq!(super::base64(&[0, 255, 16]), "AP8Q");
    }
}

/// Wrap aggregate output made of comma-separated serialized row objects.
/// Validate the aggregate before exposing it as JSON; an empty input means no rows.
pub fn wrap_rows(
    rows: &str,
    root: Option<&str>,
    without_array_wrapper: bool,
) -> Result<String, Error> {
    if root.is_some() && without_array_wrapper {
        return Err(Error::RootWithoutArrayWrapper);
    }
    let array = format!("[{rows}]");
    if json::root(array.as_bytes()) != Some(json::Kind::Array) {
        return Err(Error::InvalidJson);
    }
    if without_array_wrapper {
        return Ok(rows.to_owned());
    }
    if let Some(root) = root {
        let mut result = String::from("{");
        quoted(&mut result, root);
        result.push(':');
        result.push_str(&array);
        result.push('}');
        Ok(result)
    } else {
        Ok(array)
    }
}

/// Wrap ordered, separately serialized rows without a VARCHAR intermediate.
pub fn wrap_rows_utf16<'a>(
    rows: impl IntoIterator<Item = &'a [u16]>,
    root: Option<&str>,
    without_array_wrapper: bool,
    max_units: usize,
) -> Result<Vec<u16>, Error> {
    if root.is_some() && without_array_wrapper {
        return Err(Error::RootWithoutArrayWrapper);
    }
    let mut output = Utf16Buffer::new(max_units);
    if let Some(root) = root {
        output.ascii(b'{')?;
        output.quoted(&root.encode_utf16().collect::<Vec<_>>())?;
        output.ascii(b':')?;
    }
    if !without_array_wrapper {
        output.ascii(b'[')?;
    }
    for (index, row) in rows.into_iter().enumerate() {
        Utf16Fragment::new(row)?;
        if index != 0 {
            output.ascii(b',')?;
        }
        output.extend(row)?;
    }
    if !without_array_wrapper {
        output.ascii(b']')?;
    }
    if root.is_some() {
        output.ascii(b'}')?;
    }
    Ok(output.units)
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;
    #[test]
    fn aggregate_wrapping_keeps_lexical_values_and_rejects_broken_input() {
        assert_eq!(wrap_rows("", None, false).unwrap(), "[]");
        assert_eq!(wrap_rows("", None, true).unwrap(), "");
        let rows = "{\"n\":9007199254740993},{\"d\":1.2300}";
        assert_eq!(wrap_rows(rows, None, false).unwrap(), format!("[{rows}]"));
        assert_eq!(wrap_rows(rows, None, true).unwrap(), rows);
        assert_eq!(
            wrap_rows("{}", Some("a/\"雪"), false).unwrap(),
            "{\"a\\/\\\"雪\":[{}]}"
        );
        assert_eq!(
            wrap_rows("{}", Some(""), true),
            Err(Error::RootWithoutArrayWrapper)
        );
        for invalid in ["{} ,", "{", "null true", "{}]junk"] {
            assert_eq!(wrap_rows(invalid, None, false), Err(Error::InvalidJson));
        }
    }
}

#[cfg(test)]
mod utf16_tests {
    use super::*;
    fn u(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn raw_surrogates_remain_raw_and_fragments_keep_original_spelling() {
        let plan = PathPlan::new(["s", "fragment", "escaped"]).unwrap();
        let escaped = u("{ \"e\":\"\\ud800\",\"e\":1.20e+2 }");
        for unit in 0xd800..=0xdfff {
            let text = [unit, 0, 47];
            let mut fragment = u("{\"r\":\"");
            fragment.push(unit);
            fragment.extend(u("\"}"));
            let actual = plan
                .row_utf16(
                    &[
                        Utf16Value::Text(&text),
                        Utf16Value::Json(Utf16Fragment::new(&fragment).unwrap()),
                        Utf16Value::Json(Utf16Fragment::new(&escaped).unwrap()),
                    ],
                    false,
                    1000,
                )
                .unwrap();
            let mut expected = u("{\"s\":\"");
            expected.push(unit);
            expected.extend(u("\\u0000\\/\",\"fragment\":"));
            expected.extend(&fragment);
            expected.extend(u(",\"escaped\":"));
            expected.extend(&escaped);
            expected.push(u16::from(b'}'));
            assert_eq!(actual, expected);
        }
        for invalid in [vec![0xd800], u("{} trailing"), u("[1,]"), u("\"\\uxxxx\"")] {
            assert_eq!(Utf16Fragment::new(&invalid), Err(Error::InvalidJson));
        }
    }

    #[test]
    fn utf16_layout_matches_utf8_for_order_omission_numbers_and_fragments() {
        let plan = PathPlan::new([
            "id",
            "info.name",
            "info.optional",
            "omitted.child",
            "json",
            "flag",
        ])
        .unwrap();
        let text = "雪/🦆\n";
        let fragment = "{ \"a\":1,\"a\":2 }";
        let text_units = u(text);
        let fragment_units = u(fragment);
        let number = Number::new("12345678901234567890.1200").unwrap();
        let old = [
            Value::Number(number),
            Value::Text(text),
            Value::Null,
            Value::Null,
            Value::Json(Fragment::new(fragment).unwrap()),
            Value::Boolean(true),
        ];
        let new = [
            Utf16Value::Number(number),
            Utf16Value::Text(&text_units),
            Utf16Value::Null,
            Utf16Value::Null,
            Utf16Value::Json(Utf16Fragment::new(&fragment_units).unwrap()),
            Utf16Value::Boolean(true),
        ];
        for include in [false, true] {
            let expected = u(&plan.row(&old, include).unwrap());
            assert_eq!(
                plan.row_utf16(&new, include, expected.len()).unwrap(),
                expected
            );
            assert_eq!(
                plan.row_utf16(&new, include, expected.len() - 1),
                Err(Error::OutputLimit)
            );
        }
    }

    #[test]
    fn utf16_limits_account_for_escape_expansion_and_keep_writer_atomic() {
        let plan = PathPlan::new(["s"]).unwrap();
        let controls = [0, 1, 8, 9, 10, 12, 13, 31, 34, 47, 92];
        let expected = u(&plan
            .row(
                &[Value::Text(&String::from_utf16(&controls).unwrap())],
                false,
            )
            .unwrap());
        assert_eq!(
            plan.row_utf16(&[Utf16Value::Text(&controls)], false, expected.len())
                .unwrap(),
            expected
        );
        assert_eq!(
            plan.row_utf16(&[Utf16Value::Text(&controls)], false, expected.len() - 1),
            Err(Error::OutputLimit)
        );
        let mut writer = Utf16Writer::new(&plan, Options::default(), 64).unwrap();
        writer.push(&[Utf16Value::Text(&u("first"))]).unwrap();
        let before = writer.buffered_units();
        assert_eq!(
            writer.push(&[]),
            Err(Error::RowWidth {
                expected: 1,
                actual: 0
            })
        );
        assert_eq!(
            writer.push(&[Utf16Value::Text(&[0; 100])]),
            Err(Error::OutputLimit)
        );
        assert_eq!(writer.buffered_units(), before);
        writer.push(&[Utf16Value::Null]).unwrap();
        assert_eq!(writer.finish(), u("[{\"s\":\"first\"},{}]"));
        assert!(matches!(
            Utf16Writer::new(&plan, Options::default(), 1),
            Err(Error::OutputLimit)
        ));
        assert_eq!(
            Utf16Writer::new(&plan, Options::default(), 2)
                .unwrap()
                .finish(),
            u("[]")
        );
    }

    #[test]
    fn utf16_aggregate_and_writer_keep_options_empty_results_and_row_order() {
        let plan = PathPlan::new(["s"]).unwrap();
        let text = [0xdc00, 0xd800];
        let row = plan
            .row_utf16(&[Utf16Value::Text(&text)], false, 100)
            .unwrap();
        for root in [None, Some("a/\"雪")] {
            for without in [false, true] {
                let options = Options {
                    root: root.map(str::to_owned),
                    without_array_wrapper: without,
                    ..Options::default()
                };
                if root.is_some() && without {
                    assert!(matches!(
                        Utf16Writer::new(&plan, options, 100),
                        Err(Error::RootWithoutArrayWrapper)
                    ));
                    assert_eq!(
                        wrap_rows_utf16([row.as_slice()], root, without, 100),
                        Err(Error::RootWithoutArrayWrapper)
                    );
                    continue;
                }
                let mut writer = Utf16Writer::new(&plan, options, 100).unwrap();
                writer.push(&[Utf16Value::Text(&text)]).unwrap();
                writer.push(&[Utf16Value::Text(&text)]).unwrap();
                let result = writer.finish();
                assert_eq!(
                    wrap_rows_utf16(
                        [row.as_slice(), row.as_slice()],
                        root,
                        without,
                        result.len()
                    )
                    .unwrap(),
                    result
                );
                assert_eq!(
                    wrap_rows_utf16(
                        [row.as_slice(), row.as_slice()],
                        root,
                        without,
                        result.len() - 1
                    ),
                    Err(Error::OutputLimit)
                );
                assert_eq!(
                    wrap_rows_utf16([], root, without, 100).unwrap(),
                    u(&wrap_rows("", root, without).unwrap())
                );
            }
        }
        assert_eq!(
            wrap_rows_utf16([u("{},{}").as_slice()], None, false, 100),
            Err(Error::InvalidJson)
        );
    }
}
