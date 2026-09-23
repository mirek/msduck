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
        if values.len() != self.columns {
            return Err(Error::RowWidth {
                expected: self.columns,
                actual: values.len(),
            });
        }
        let mut visible = vec![false; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate().rev() {
            visible[index] = node.entries.iter().any(|entry| match entry.target {
                Target::Column(column) => {
                    include_null_values || !matches!(values[column], Value::Null)
                }
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
        let mut output = String::new();
        while let Some(task) = pending.pop() {
            match task {
                Task::Object(index) => {
                    output.push('{');
                    pending.push(Task::Close);
                    let mut later = false;
                    for entry in self.nodes[index].entries.iter().rev() {
                        let present = match entry.target {
                            Target::Column(column) => {
                                include_null_values || !matches!(values[column], Value::Null)
                            }
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
                    quoted(&mut output, &entry.name);
                    output.push(':');
                    match entry.target {
                        Target::Object(child) => pending.push(Task::Object(child)),
                        Target::Column(column) => match values[column] {
                            Value::Null => output.push_str("null"),
                            Value::Text(text) => quoted(&mut output, text),
                            Value::Boolean(value) => {
                                output.push_str(if value { "true" } else { "false" })
                            }
                            Value::Number(number) => output.push_str(number.0),
                            Value::Json(fragment) => output.push_str(fragment.0),
                        },
                    }
                }
                Task::Comma => output.push(','),
                Task::Close => output.push('}'),
            }
        }
        Ok(output)
    }
}
fn quoted(output: &mut String, text: &str) {
    output.push('"');
    output.push_str(&json_escape::escape(text, "json").expect("JSON format is supported"));
    output.push('"');
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
